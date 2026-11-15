//! A JSON-file-backed [`AccountProvider`], one folder per login under a
//! directory. `hotline-server` deliberately doesn't own persistence (see
//! its module docs); this is the concrete backend `hls` plugs in.
//!
//! An account's folder can also hold its own stores, for [`AccountStores`].

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError};

use async_trait::async_trait;
use hotline_server::{Account, AccountProvider, AccountStores, FileProvider, ThreadedNewsProvider};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

use crate::file::DiskFileProvider;
use crate::fork_store::ForkStoreKind;
use crate::news::FileThreadedNewsProvider;

/// The seeded account with full access.
pub const ADMIN_LOGIN: &str = "admin";

/// The seeded account with default access.
pub const GUEST_LOGIN: &str = "guest";

/// 16 random hex digits from the OS random source.
pub fn random_password() -> std::io::Result<String> {
    getrandom::u64()
        .map(|bits| format!("{bits:016x}"))
        .map_err(std::io::Error::other)
}

/// The file in an account's folder that holds the account.
const ACCOUNT_FILE: &str = "account.json";

/// The folder in an account's folder that holds its own files.
const FILES_DIR: &str = "files";

/// The file in an account's folder that holds its own threaded news.
const NEWS_FILE: &str = "news.json";

/// What a delete does with the folder of an account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountDeletion {
    /// Move the folder into this trash folder, as openline does.
    Trash(PathBuf),
    /// Remove the folder with all that it holds.
    Remove,
}

/// Persists each account as `<login>/account.json` under a directory,
/// with an in-memory cache so `get` never touches disk.
///
/// An account's own stores are in its `<login>` folder too: `files/` and
/// `news.json`. Each store that exists replaces the shared one for that
/// account.
pub struct FileAccountProvider {
    dir: PathBuf,
    cache: RwLock<HashMap<Box<str>, Account>>,
    fork_store: ForkStoreKind,
    deletion: AccountDeletion,
    files: Stores<DiskFileProvider>,
    news: Stores<FileThreadedNewsProvider>,
}

/// The open stores of each login.
type Stores<T> = std::sync::RwLock<HashMap<Box<str>, Arc<T>>>;

impl FileAccountProvider {
    /// Loads every account under `dir`. A new `dir` is private to this
    /// user on Unix. A delete moves the folder into `trash/`.
    pub async fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        create_private_dir(&dir).await?;

        let mut cache = HashMap::new();
        let mut entries = tokio::fs::read_dir(&dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let folder = entry.path();
            let path = folder.join(ACCOUNT_FILE);
            if !metadata(&folder).await?.is_some_and(|meta| meta.is_dir())
                || !metadata(&path).await?.is_some_and(|meta| meta.is_file())
            {
                continue;
            }
            let Some(login) = entry.file_name().to_str().map(Box::from) else {
                let path = path.display();
                tracing::warn!(%path, "skipped an account whose folder name is not UTF-8");
                continue;
            };
            let bytes = tokio::fs::read(&path).await?;
            let account: Account = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            cache.insert(login, account);
        }

        Ok(Self {
            dir,
            cache: RwLock::new(cache),
            fork_store: ForkStoreKind::AppleDouble,
            deletion: AccountDeletion::Trash(PathBuf::from("trash")),
            files: Stores::default(),
            news: Stores::default(),
        })
    }

    /// Keep the Mac metadata of each account's own files in `fork_store`.
    pub const fn with_fork_store(mut self, fork_store: ForkStoreKind) -> Self {
        self.fork_store = fork_store;
        self
    }

    /// Delete the folder of an account as `deletion` says.
    pub fn with_deletion(mut self, deletion: AccountDeletion) -> Self {
        self.deletion = deletion;
        self
    }

    /// Whether the directory holds no accounts.
    pub async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }

    /// The folder of `login`. `None` for a login that can't be a safe
    /// folder name. mhxd also rejects a `/`.
    fn folder_for(&self, login: &str) -> Option<PathBuf> {
        is_safe_login(login).then(|| self.dir.join(login))
    }

    /// The account file of `login`. `None` as for [`Self::folder_for`].
    fn path_for(&self, login: &str) -> Option<PathBuf> {
        self.folder_for(login)
            .map(|folder| folder.join(ACCOUNT_FILE))
    }

    /// Drop the open stores of `login`.
    fn forget_stores(&self, login: &str) {
        forget(&self.files, login);
        forget(&self.news, login);
    }

    /// Give the open files store of `old_login` to `login`, and serve it
    /// from the files folder in `folder`.
    fn move_files(&self, old_login: &str, login: &str, folder: &Path) {
        let mut files = self.files.write().unwrap_or_else(PoisonError::into_inner);
        let store = files.remove(old_login);
        files.remove(login);
        if let Some(store) = store {
            store.move_root(folder.join(FILES_DIR));
            files.insert(login.into(), store);
        }
    }

    /// Removes the account file `path`, then the rest of `folder`. A
    /// failure to remove the rest leaves the folder and logs a warning.
    async fn remove_account(
        &self,
        login: &str,
        path: &Path,
        folder: &Path,
    ) -> Result<bool, Cow<'static, str>> {
        match tokio::fs::remove_file(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            other => other.map_err(|e| e.to_string())?,
        }
        self.cache.write().await.remove(login);
        self.forget_stores(login);
        if let Err(error) = tokio::fs::remove_dir_all(folder).await {
            let folder = folder.display();
            tracing::warn!(%error, %folder, "could not remove a deleted account's folder");
        }
        Ok(true)
    }

    /// Moves `folder` into `trash` under the name that [`free_path`]
    /// gives.
    async fn trash_account(
        &self,
        login: &str,
        path: &Path,
        folder: &Path,
        trash: &Path,
    ) -> Result<bool, Cow<'static, str>> {
        if metadata(path).await.map_err(|e| e.to_string())?.is_none() {
            return Ok(false);
        }
        create_private_dir(trash).await.map_err(|e| e.to_string())?;
        let trashed = free_path(trash, login).await.map_err(|e| e.to_string())?;
        tokio::fs::rename(folder, &trashed)
            .await
            .map_err(|e| e.to_string())?;
        self.cache.write().await.remove(login);
        self.forget_stores(login);
        Ok(true)
    }

    async fn write_account(&self, login: &str, account: &Account) -> std::io::Result<()> {
        let path = self.path_for(login).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid login")
        })?;
        let bytes = serde_json::to_vec_pretty(account)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(path).await?;
        file.write_all(&bytes).await?;
        file.flush().await
    }
}

#[async_trait]
impl AccountProvider for FileAccountProvider {
    async fn get(&self, login: &str) -> Option<Account> {
        self.cache.read().await.get(login).cloned()
    }

    /// Makes the folder of a new login first, and fails if that folder
    /// exists. A failed write removes the new folder again.
    async fn put(&self, login: Box<str>, account: Account) -> Result<(), Cow<'static, str>> {
        let folder = self.folder_for(&login).ok_or("invalid login")?;
        let new = !self.cache.read().await.contains_key(&login);
        if new {
            tokio::fs::create_dir(&folder)
                .await
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => format!(
                        "Cannot create account “{login}” because there is already an account with that login."
                    ),
                    _ => e.to_string(),
                })?;
        }
        if let Err(error) = self.write_account(&login, &account).await {
            if new {
                let _ = tokio::fs::remove_file(folder.join(ACCOUNT_FILE)).await;
                let _ = tokio::fs::remove_dir(&folder).await;
            }
            Err(error.to_string())?;
        }
        self.cache.write().await.insert(login, account);
        Ok(())
    }

    /// Deletes the account's folder as [`Self::with_deletion`] says.
    async fn delete(&self, login: &str) -> Result<bool, Cow<'static, str>> {
        let (Some(path), Some(folder)) = (self.path_for(login), self.folder_for(login)) else {
            return Ok(false);
        };
        match &self.deletion {
            AccountDeletion::Trash(trash) => self.trash_account(login, &path, &folder, trash).await,
            AccountDeletion::Remove => self.remove_account(login, &path, &folder).await,
        }
    }

    /// Moves the account's folder, then writes `account` into it, so a
    /// rename works when the file system sees both logins as one name, such
    /// as a change of only the letter case. A failed write leaves the old
    /// account under `login`.
    async fn rename(
        &self,
        old_login: &str,
        login: Box<str>,
        account: Account,
    ) -> Result<(), Cow<'static, str>> {
        let (Some(old_folder), Some(folder)) =
            (self.folder_for(old_login), self.folder_for(&login))
        else {
            Err("invalid login")?
        };
        let moved = tokio::fs::rename(old_folder, &folder).await;
        forget(&self.news, old_login);
        forget(&self.news, &login);
        moved.map_err(|e| e.to_string())?;
        self.move_files(old_login, &login, &folder);
        let written = self.write_account(&login, &account).await;
        let mut cache = self.cache.write().await;
        let old = cache.remove(old_login);
        match written {
            Ok(()) => {
                cache.insert(login, account);
                Ok(())
            }
            Err(error) => {
                cache.extend(old.map(|old| (login, old)));
                Err(error.to_string().into())
            }
        }
    }

    async fn list(&self) -> Vec<(Box<str>, Account)> {
        self.cache
            .read()
            .await
            .iter()
            .map(|(login, account)| (login.clone(), account.clone()))
            .collect()
    }
}

#[async_trait]
impl AccountStores for FileAccountProvider {
    async fn files(&self, login: &str) -> Result<Option<Arc<dyn FileProvider>>, &'static str> {
        let failed = |error: std::io::Error| {
            tracing::debug!(login, %error, "could not open an account's files");
            "the account's files cannot be opened"
        };
        let Some(root) = self.folder_for(login).map(|folder| folder.join(FILES_DIR)) else {
            return Ok(None);
        };
        let exists = metadata(&root)
            .await
            .map_err(failed)?
            .is_some_and(|meta| meta.is_dir());
        if !exists {
            forget(&self.files, login);
            return Ok(None);
        }
        let provider = match open_store(&self.files, login) {
            Some(provider) => provider,
            None => {
                let provider = DiskFileProvider::new(root).with_fork_store(self.fork_store.store());
                keep(&self.files, login, provider)
            }
        };
        Ok(Some(provider))
    }

    async fn threaded_news(
        &self,
        login: &str,
    ) -> Result<Option<Arc<dyn ThreadedNewsProvider>>, &'static str> {
        let failed = |error: std::io::Error| {
            tracing::debug!(login, %error, "could not open an account's news");
            "the account's news cannot be opened"
        };
        let Some(path) = self.folder_for(login).map(|folder| folder.join(NEWS_FILE)) else {
            return Ok(None);
        };
        let exists = metadata(&path)
            .await
            .map_err(failed)?
            .is_some_and(|meta| meta.is_file());
        if !exists {
            forget(&self.news, login);
            return Ok(None);
        }
        let provider = match open_store(&self.news, login) {
            Some(provider) => provider,
            None => {
                let provider = FileThreadedNewsProvider::open(path).await.map_err(failed)?;
                keep(&self.news, login, provider)
            }
        };
        Ok(Some(provider))
    }
}

/// The metadata of `path`, or `None` if it does not exist.
async fn metadata(path: &Path) -> std::io::Result<Option<std::fs::Metadata>> {
    match tokio::fs::metadata(path).await {
        Ok(meta) => Ok(Some(meta)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Make `dir` and its parents. A new `dir` is private to this user on
/// Unix.
async fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(dir).await
}

/// `name` in `dir`, or else `name` with the first number from 1 that
/// gives a name not in `dir`, as openline names an item in the trash.
async fn free_path(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    let mut path = dir.join(name);
    for number in 1u32.. {
        if metadata(&path).await?.is_none() {
            break;
        }
        path = dir.join(format!("{name}{number}"));
    }
    Ok(path)
}

/// The open store of `login` in `stores`.
fn open_store<T>(stores: &Stores<T>, login: &str) -> Option<Arc<T>> {
    stores
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(login)
        .cloned()
}

/// Keep `store` as the store of `login`, or give the store that another
/// lookup kept first, so each login uses one copy of its store.
fn keep<T>(stores: &Stores<T>, login: &str, store: T) -> Arc<T> {
    stores
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .entry(login.into())
        .or_insert_with(|| Arc::new(store))
        .clone()
}

/// Forget the open store of `login` in `stores`.
fn forget<T>(stores: &Stores<T>, login: &str) {
    let open = stores
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .contains_key(login);
    if open {
        stores
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(login);
    }
}

/// Whether `login` can name a folder in the accounts directory.
fn is_safe_login(login: &str) -> bool {
    !(login.is_empty() || login.contains(['/', '\\']) || login == "." || login == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::consts::Access;
    use hotline_proto::types::HlPath;
    use hotline_server::{AccessExtra, FileRequester};

    fn alice() -> Account {
        Account {
            password: "hunter2".into(),
            name: "Alice".into(),
            access: Access::BROADCAST,
            access_extra: AccessExtra::USER_GETLIST,
        }
    }

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        provider.put("alice".into(), alice()).await.unwrap();

        assert_eq!(provider.get("alice").await, Some(alice()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_directory_and_account_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("accounts");
        let provider = FileAccountProvider::open(&accounts).await.unwrap();
        provider.put("alice".into(), alice()).await.unwrap();

        let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(accounts.clone()), 0o700);
        assert_eq!(mode(accounts.join("alice").join(ACCOUNT_FILE)), 0o600);
    }

    #[tokio::test]
    async fn accounts_survive_reopening_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        {
            let provider = FileAccountProvider::open(dir.path()).await.unwrap();
            provider.put("alice".into(), alice()).await.unwrap();
        }

        let reopened = FileAccountProvider::open(dir.path()).await.unwrap();

        assert_eq!(reopened.get("alice").await, Some(alice()));
    }

    #[tokio::test]
    async fn delete_removes_the_file_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path())
            .await
            .unwrap()
            .with_deletion(AccountDeletion::Remove);
        provider.put("alice".into(), alice()).await.unwrap();

        assert_eq!(provider.delete("alice").await, Ok(true));
        assert_eq!(provider.delete("alice").await, Ok(false));
        assert_eq!(provider.get("alice").await, None);
    }

    #[tokio::test]
    async fn is_empty_until_an_account_is_put() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        assert!(provider.is_empty().await);

        provider.put("alice".into(), alice()).await.unwrap();

        assert!(!provider.is_empty().await);
    }

    #[test]
    fn random_password_is_16_hex_digits() {
        let password = random_password().unwrap();

        assert_eq!(password.len(), 16);
        assert!(password.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn an_account_has_its_own_files_while_its_files_folder_exists() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".into(), alice()).await.unwrap();
        assert!(provider.files("alice").await.unwrap().is_none());

        let root = dir.path().join("alice").join(FILES_DIR);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("own.txt"), "x").unwrap();
        let requester = FileRequester { login: "alice" };
        let files = provider.files("alice").await.unwrap().unwrap();
        let listed = files.list(requester, &HlPath::default()).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(&*listed[0].name, "own.txt");

        std::fs::remove_dir_all(&root).unwrap();
        assert!(provider.files("alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn an_account_has_its_own_threaded_news_while_its_news_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        assert!(provider.threaded_news("alice").await.unwrap().is_none());

        std::fs::create_dir(dir.path().join("alice")).unwrap();
        std::fs::write(dir.path().join("alice").join(NEWS_FILE), "{}").unwrap();

        let news = provider.threaded_news("alice").await.unwrap().unwrap();
        news.mkcategory(&[], "Own").await.unwrap();
        let again = provider.threaded_news("alice").await.unwrap().unwrap();
        assert_eq!(again.list_dir(&[]).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_account_whose_news_file_does_not_parse_gets_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice")).unwrap();
        std::fs::write(dir.path().join("alice").join(NEWS_FILE), "not json").unwrap();

        assert!(provider.threaded_news("alice").await.is_err());
    }

    #[tokio::test]
    async fn a_folder_called_news_json_gives_the_shared_news() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir_all(dir.path().join("alice").join(NEWS_FILE)).unwrap();

        assert!(provider.threaded_news("alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_file_called_files_gives_the_shared_files() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice")).unwrap();
        std::fs::write(dir.path().join("alice").join(FILES_DIR), "x").unwrap();

        assert!(provider.files("alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn rename_moves_the_account_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".into(), alice()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice").join(FILES_DIR)).unwrap();
        assert!(provider.files("alice").await.unwrap().is_some());

        provider
            .rename("alice", "bob".into(), alice())
            .await
            .unwrap();

        assert_eq!(provider.get("alice").await, None);
        assert_eq!(provider.get("bob").await, Some(alice()));
        assert!(!dir.path().join("alice").exists());
        assert!(dir.path().join("bob").join(ACCOUNT_FILE).is_file());
        assert!(provider.files("alice").await.unwrap().is_none());
        assert!(provider.files("bob").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn rename_moves_an_open_files_store_with_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".into(), alice()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice").join(FILES_DIR)).unwrap();
        let files = provider.files("alice").await.unwrap().unwrap();
        let requester = FileRequester { login: "alice" };
        let root = HlPath::default();
        let mut writer = files.create_write(requester, &root, "a.txt").await.unwrap();
        writer.write_all(b"he").await.unwrap();

        provider
            .rename("alice", "bob".into(), alice())
            .await
            .unwrap();

        writer.write_all(b"llo").await.unwrap();
        writer.shutdown().await.unwrap();
        files
            .finish_upload(requester, &root, "a.txt")
            .await
            .unwrap();
        let moved = dir.path().join("bob").join(FILES_DIR).join("a.txt");
        assert_eq!(std::fs::read(moved).unwrap(), b"hello");
        let bob = provider.files("bob").await.unwrap().unwrap();
        assert!(Arc::ptr_eq(&files, &bob));
    }

    #[tokio::test]
    async fn rename_that_changes_only_the_case_keeps_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("bob".into(), alice()).await.unwrap();

        provider.rename("bob", "Bob".into(), alice()).await.unwrap();

        assert_eq!(provider.get("bob").await, None);
        assert_eq!(provider.get("Bob").await, Some(alice()));
        let reopened = FileAccountProvider::open(dir.path()).await.unwrap();
        assert_eq!(reopened.get("Bob").await, Some(alice()));
    }

    #[tokio::test]
    async fn rename_onto_a_folder_with_files_fails_and_keeps_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".into(), alice()).await.unwrap();
        std::fs::create_dir_all(dir.path().join("bob").join(FILES_DIR)).unwrap();

        assert!(
            provider
                .rename("alice", "bob".into(), alice())
                .await
                .is_err()
        );

        assert_eq!(provider.get("alice").await, Some(alice()));
        assert_eq!(provider.get("bob").await, None);
        assert!(dir.path().join("alice").join(ACCOUNT_FILE).is_file());
        assert!(!dir.path().join("bob").join(ACCOUNT_FILE).exists());
    }

    #[tokio::test]
    async fn delete_removes_the_account_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path())
            .await
            .unwrap()
            .with_deletion(AccountDeletion::Remove);
        provider.put("alice".into(), alice()).await.unwrap();
        let root = dir.path().join("alice").join(FILES_DIR);
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("own.txt"), "x").unwrap();
        assert!(provider.files("alice").await.unwrap().is_some());

        assert_eq!(provider.delete("alice").await, Ok(true));

        assert!(!dir.path().join("alice").exists());
        assert!(provider.files("alice").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn delete_moves_the_account_folder_to_the_trash_under_a_free_name() {
        let dir = tempfile::tempdir().unwrap();
        let trash = dir.path().join("trash");
        let provider = FileAccountProvider::open(dir.path().join("accounts"))
            .await
            .unwrap()
            .with_deletion(AccountDeletion::Trash(trash.clone()));
        for _ in 0..3 {
            provider.put("alice".into(), alice()).await.unwrap();
            assert_eq!(provider.delete("alice").await, Ok(true));
        }

        assert_eq!(provider.delete("alice").await, Ok(false));
        assert_eq!(provider.get("alice").await, None);
        assert!(!dir.path().join("accounts").join("alice").exists());
        for name in ["alice", "alice1", "alice2"] {
            assert!(trash.join(name).join(ACCOUNT_FILE).is_file());
        }
    }

    #[tokio::test]
    async fn delete_that_cannot_remove_the_account_file_keeps_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path())
            .await
            .unwrap()
            .with_deletion(AccountDeletion::Remove);
        std::fs::create_dir_all(dir.path().join("alice").join(ACCOUNT_FILE)).unwrap();
        std::fs::create_dir(dir.path().join("alice").join(FILES_DIR)).unwrap();

        assert!(provider.delete("alice").await.is_err());

        assert!(dir.path().join("alice").join(FILES_DIR).is_dir());
        assert!(provider.files("alice").await.unwrap().is_some());
    }

    #[tokio::test]
    async fn a_new_login_whose_folder_exists_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice")).unwrap();

        assert_eq!(
            provider.put("alice".into(), alice()).await,
            Err(
                "Cannot create account “alice” because there is already an account with that login."
                    .into()
            )
        );

        assert_eq!(provider.get("alice").await, None);
        assert!(!dir.path().join("alice").join(ACCOUNT_FILE).exists());
    }

    #[tokio::test]
    async fn a_login_with_a_path_separator_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        assert!(provider.put("../escape".into(), alice()).await.is_err());
    }

    #[tokio::test]
    async fn open_skips_a_folder_without_an_account_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("alice").join(FILES_DIR)).unwrap();
        std::fs::write(dir.path().join("bob"), "x").unwrap();

        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        assert!(provider.is_empty().await);
    }
}
