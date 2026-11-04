//! A JSON-file-backed [`AccountProvider`], one folder per login under a
//! directory. `hotline-server` deliberately doesn't own persistence (see
//! its module docs); this is the concrete backend `hls` plugs in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hotline_server::{Account, AccountProvider};
use tokio::io::AsyncWriteExt;
use tokio::sync::RwLock;

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

/// Persists each account as `<login>/account.json` under a directory,
/// with an in-memory cache so `get` never touches disk.
pub struct FileAccountProvider {
    dir: PathBuf,
    cache: RwLock<HashMap<String, Account>>,
}

impl FileAccountProvider {
    /// Loads every account under `dir`. A new `dir` is private to this
    /// user on Unix.
    pub async fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        builder.mode(0o700);
        builder.create(&dir).await?;

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
            let Some(login) = entry.file_name().to_str().map(str::to_string) else {
                let path = path.display();
                tracing::warn!(%path, "skipped an account whose folder name is not UTF-8");
                continue;
            };
            let bytes = tokio::fs::read(&path).await?;
            let account: Account = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            cache.insert(login, account);
        }

        let cache = RwLock::new(cache);
        Ok(Self { dir, cache })
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
    async fn put(&self, login: String, account: Account) -> Result<(), Cow<'static, str>> {
        let folder = self.folder_for(&login).ok_or("invalid login")?;
        let new = !self.cache.read().await.contains_key(&login);
        if new {
            tokio::fs::create_dir(&folder)
                .await
                .map_err(|e| match e.kind() {
                    std::io::ErrorKind::AlreadyExists => format!(
                        "Cannot create account \"{login}\" because there is already an account with that login."
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

    /// Removes the account file, then the rest of the account's folder. A
    /// failure to remove the rest leaves the folder and logs a warning.
    async fn delete(&self, login: &str) -> Result<bool, Cow<'static, str>> {
        let (Some(path), Some(folder)) = (self.path_for(login), self.folder_for(login)) else {
            return Ok(false);
        };
        match tokio::fs::remove_file(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            other => other.map_err(|e| e.to_string())?,
        }
        self.cache.write().await.remove(login);
        if let Err(error) = tokio::fs::remove_dir_all(&folder).await {
            let folder = folder.display();
            tracing::warn!(%error, %folder, "could not remove a deleted account's folder");
        }
        Ok(true)
    }

    /// Moves the account's folder, then writes `account` into it, so a
    /// rename works when the file system sees both logins as one name, such
    /// as a change of only the letter case. A failed write leaves the old
    /// account under `login`.
    async fn rename(
        &self,
        old_login: &str,
        login: String,
        account: Account,
    ) -> Result<(), Cow<'static, str>> {
        let (Some(old_folder), Some(folder)) =
            (self.folder_for(old_login), self.folder_for(&login))
        else {
            Err("invalid login")?
        };
        tokio::fs::rename(old_folder, folder)
            .await
            .map_err(|e| e.to_string())?;
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

    async fn list(&self) -> Vec<(String, Account)> {
        self.cache
            .read()
            .await
            .iter()
            .map(|(login, account)| (login.clone(), account.clone()))
            .collect()
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

/// Whether `login` can name a folder in the accounts directory.
fn is_safe_login(login: &str) -> bool {
    !(login.is_empty() || login.contains(['/', '\\']) || login == "." || login == "..")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::consts::Access;
    use hotline_server::AccessExtra;

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

        provider.put("alice".to_string(), alice()).await.unwrap();

        assert_eq!(provider.get("alice").await, Some(alice()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_directory_and_account_file_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let accounts = dir.path().join("accounts");
        let provider = FileAccountProvider::open(&accounts).await.unwrap();
        provider.put("alice".to_string(), alice()).await.unwrap();

        let mode = |path: PathBuf| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(accounts.clone()), 0o700);
        assert_eq!(mode(accounts.join("alice").join(ACCOUNT_FILE)), 0o600);
    }

    #[tokio::test]
    async fn accounts_survive_reopening_the_same_directory() {
        let dir = tempfile::tempdir().unwrap();
        {
            let provider = FileAccountProvider::open(dir.path()).await.unwrap();
            provider.put("alice".to_string(), alice()).await.unwrap();
        }

        let reopened = FileAccountProvider::open(dir.path()).await.unwrap();

        assert_eq!(reopened.get("alice").await, Some(alice()));
    }

    #[tokio::test]
    async fn delete_removes_the_file_and_reports_whether_it_existed() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".to_string(), alice()).await.unwrap();

        assert_eq!(provider.delete("alice").await, Ok(true));
        assert_eq!(provider.delete("alice").await, Ok(false));
        assert_eq!(provider.get("alice").await, None);
    }

    #[tokio::test]
    async fn is_empty_until_an_account_is_put() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        assert!(provider.is_empty().await);

        provider.put("alice".to_string(), alice()).await.unwrap();

        assert!(!provider.is_empty().await);
    }

    #[test]
    fn random_password_is_16_hex_digits() {
        let password = random_password().unwrap();

        assert_eq!(password.len(), 16);
        assert!(password.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[tokio::test]
    async fn rename_moves_the_account_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".to_string(), alice()).await.unwrap();
        std::fs::write(dir.path().join("alice").join("own.txt"), "x").unwrap();

        provider
            .rename("alice", "bob".to_string(), alice())
            .await
            .unwrap();

        assert_eq!(provider.get("alice").await, None);
        assert_eq!(provider.get("bob").await, Some(alice()));
        assert!(!dir.path().join("alice").exists());
        assert!(dir.path().join("bob").join(ACCOUNT_FILE).is_file());
        assert!(dir.path().join("bob").join("own.txt").is_file());
    }

    #[tokio::test]
    async fn rename_that_changes_only_the_case_keeps_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("bob".to_string(), alice()).await.unwrap();

        provider
            .rename("bob", "Bob".to_string(), alice())
            .await
            .unwrap();

        assert_eq!(provider.get("bob").await, None);
        assert_eq!(provider.get("Bob").await, Some(alice()));
        let reopened = FileAccountProvider::open(dir.path()).await.unwrap();
        assert_eq!(reopened.get("Bob").await, Some(alice()));
    }

    #[tokio::test]
    async fn rename_onto_a_folder_with_files_fails_and_keeps_the_account() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".to_string(), alice()).await.unwrap();
        std::fs::create_dir(dir.path().join("bob")).unwrap();
        std::fs::write(dir.path().join("bob").join("own.txt"), "x").unwrap();

        assert!(provider
            .rename("alice", "bob".to_string(), alice())
            .await
            .is_err());

        assert_eq!(provider.get("alice").await, Some(alice()));
        assert_eq!(provider.get("bob").await, None);
        assert!(dir.path().join("alice").join(ACCOUNT_FILE).is_file());
        assert!(!dir.path().join("bob").join(ACCOUNT_FILE).exists());
    }

    #[tokio::test]
    async fn delete_removes_the_account_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        provider.put("alice".to_string(), alice()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice").join("own")).unwrap();

        assert_eq!(provider.delete("alice").await, Ok(true));

        assert!(!dir.path().join("alice").exists());
    }

    #[tokio::test]
    async fn delete_that_cannot_remove_the_account_file_keeps_the_folder() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir_all(dir.path().join("alice").join(ACCOUNT_FILE)).unwrap();
        std::fs::write(dir.path().join("alice").join("own.txt"), "x").unwrap();

        assert!(provider.delete("alice").await.is_err());

        assert!(dir.path().join("alice").join("own.txt").is_file());
    }

    #[tokio::test]
    async fn a_new_login_whose_folder_exists_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();
        std::fs::create_dir(dir.path().join("alice")).unwrap();

        assert_eq!(
            provider.put("alice".to_string(), alice()).await,
            Err(
                "Cannot create account \"alice\" because there is already an account with that login."
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

        assert!(provider
            .put("../escape".to_string(), alice())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn open_skips_a_folder_without_an_account_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("alice")).unwrap();
        std::fs::write(dir.path().join("bob"), "x").unwrap();

        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        assert!(provider.is_empty().await);
    }
}
