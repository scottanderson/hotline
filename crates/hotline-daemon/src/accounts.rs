//! A JSON-file-backed [`AccountProvider`], one file per login under a
//! directory. `hotline-server` deliberately doesn't own persistence (see
//! its module docs); this is the concrete backend `hls` plugs in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

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

/// Persists accounts as one `<login>.json` file per account under a
/// directory, with an in-memory cache so `get` never touches disk.
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
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let Some(login) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let bytes = tokio::fs::read(&path).await?;
            let account: Account = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            cache.insert(login.to_string(), account);
        }

        let cache = RwLock::new(cache);
        Ok(Self { dir, cache })
    }

    /// Whether the directory holds no accounts.
    pub async fn is_empty(&self) -> bool {
        self.cache.read().await.is_empty()
    }

    /// `None` for a login that can't be a safe filename. mhxd also rejects a `/`.
    fn path_for(&self, login: &str) -> Option<PathBuf> {
        if login.is_empty() || login.contains(['/', '\\']) || login == "." || login == ".." {
            return None;
        }
        Some(self.dir.join(format!("{login}.json")))
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

    async fn put(&self, login: String, account: Account) -> Result<(), Cow<'static, str>> {
        self.write_account(&login, &account)
            .await
            .map_err(|e| e.to_string())?;
        self.cache.write().await.insert(login, account);
        Ok(())
    }

    async fn delete(&self, login: &str) -> Result<bool, Cow<'static, str>> {
        let Some(path) = self.path_for(login) else {
            return Ok(false);
        };
        match tokio::fs::remove_file(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            other => other.map_err(|e| e.to_string())?,
        }
        self.cache.write().await.remove(login);
        Ok(true)
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
        assert_eq!(mode(accounts.join("alice.json")), 0o600);
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
    async fn a_login_with_a_path_separator_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        assert!(provider
            .put("../escape".to_string(), alice())
            .await
            .is_err());
    }
}
