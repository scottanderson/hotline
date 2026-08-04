//! A JSON-file-backed [`AccountProvider`], one file per login under a
//! directory. `hotline-server` deliberately doesn't own persistence (see
//! its module docs); this is the concrete backend `hls` plugs in.

use std::collections::HashMap;
use std::path::PathBuf;

use async_trait::async_trait;
use hotline_proto::consts::AccessPrivileges;
use hotline_server::{default_accounts, AccessExtra, Account, AccountProvider};
use tokio::sync::RwLock;

/// The on-disk shape of an [`Account`]; keeps `serde` out of
/// `hotline-proto`/`hotline-server` by converting through plain integers
/// here instead of deriving `Serialize`/`Deserialize` on the bitflags types.
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredAccount {
    password: String,
    name: String,
    access: u64,
    access_extra: u32,
}

impl From<&Account> for StoredAccount {
    fn from(account: &Account) -> Self {
        Self {
            password: account.password.clone(),
            name: account.name.clone(),
            access: account.access.bits(),
            access_extra: account.access_extra.bits(),
        }
    }
}

impl From<StoredAccount> for Account {
    fn from(stored: StoredAccount) -> Self {
        Self {
            password: stored.password,
            name: stored.name,
            access: AccessPrivileges::from_bits_truncate(stored.access),
            access_extra: AccessExtra::from_bits_truncate(stored.access_extra),
        }
    }
}

/// Persists accounts as one `<login>.json` file per account under a
/// directory, with an in-memory cache so `get` never touches disk.
pub struct FileAccountProvider {
    dir: PathBuf,
    cache: RwLock<HashMap<String, Account>>,
}

impl FileAccountProvider {
    /// Loads every account under `dir`, creating it and seeding
    /// [`default_accounts`] if it's empty (fresh directory or none of the
    /// files parsed), so a new deployment always has a way to log in.
    pub async fn open(dir: impl Into<PathBuf>) -> std::io::Result<Self> {
        let dir = dir.into();
        tokio::fs::create_dir_all(&dir).await?;

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
            let stored: StoredAccount = serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            cache.insert(login.to_string(), stored.into());
        }

        let provider = Self {
            dir,
            cache: RwLock::new(cache),
        };
        if provider.cache.read().await.is_empty() {
            for (login, account) in default_accounts() {
                provider.write_account(&login, &account).await?;
                provider.cache.write().await.insert(login, account);
            }
        }
        Ok(provider)
    }

    /// `None` for a login that can't be a safe filename (matches the
    /// reference's own rejection of a `/` in a login).
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
        let bytes = serde_json::to_vec_pretty(&StoredAccount::from(account))
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(path, bytes).await
    }
}

#[async_trait]
impl AccountProvider for FileAccountProvider {
    async fn get(&self, login: &str) -> Option<Account> {
        self.cache.read().await.get(login).cloned()
    }

    async fn put(&self, login: String, account: Account) -> Result<(), String> {
        self.write_account(&login, &account)
            .await
            .map_err(|e| e.to_string())?;
        self.cache.write().await.insert(login, account);
        Ok(())
    }

    async fn delete(&self, login: &str) -> Result<bool, String> {
        let Some(path) = self.path_for(login) else {
            return Ok(false);
        };
        match tokio::fs::remove_file(path).await {
            Ok(()) => {
                self.cache.write().await.remove(login);
                Ok(true)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alice() -> Account {
        Account {
            password: "hunter2".to_string(),
            name: "Alice".to_string(),
            access: AccessPrivileges::CAN_BROADCAST,
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
    async fn opening_an_empty_directory_seeds_default_accounts() {
        let dir = tempfile::tempdir().unwrap();

        let provider = FileAccountProvider::open(dir.path()).await.unwrap();

        assert!(provider.get("admin").await.is_some());
        assert!(provider.get("guest").await.is_some());
        // The seed persisted to disk, not just the in-memory cache.
        let reopened = FileAccountProvider::open(dir.path()).await.unwrap();
        assert!(reopened.get("admin").await.is_some());
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
