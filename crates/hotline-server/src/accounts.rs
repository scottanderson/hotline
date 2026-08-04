//! Named accounts: login, password, display name, and the `USER_ACCESS`
//! privileges granted on successful login.
//!
//! [`AccountProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`InMemoryAccountProvider`] as a default. The
//! embedding application supplies the real backend to [`crate::Server::new`].

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use hotline_proto::consts::AccessPrivileges;

/// One named account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Compared for exact equality against the submitted password; an
    /// empty stored password requires an empty submission too.
    pub password: String,
    pub name: String,
    pub access: AccessPrivileges,
}

/// Account storage: looked up on login, mutated by the account CRUD
/// opcodes (`HTLC_HDR_ACCOUNT_CREATE`/`_READ`/`_MODIFY`/`_DELETE`).
#[async_trait]
pub trait AccountProvider: Send + Sync {
    /// Look up an account by login.
    async fn get(&self, login: &str) -> Option<Account>;
    /// Create or overwrite an account.
    async fn put(&self, login: String, account: Account) -> Result<(), String>;
    /// Delete an account. `false` if it didn't exist.
    async fn delete(&self, login: &str) -> Result<bool, String>;
}

/// An [`AccountProvider`] that keeps accounts in memory only; nothing
/// created at runtime survives a restart. Starts with `admin`/`admin`
/// (full access) and a passwordless `guest` (`guest_default()` access),
/// so there's always a way to log in and create further accounts.
#[derive(Debug)]
pub struct InMemoryAccountProvider {
    accounts: RwLock<HashMap<String, Account>>,
}

impl Default for InMemoryAccountProvider {
    fn default() -> Self {
        Self {
            accounts: RwLock::new(HashMap::from([
                (
                    "admin".to_string(),
                    Account {
                        password: "admin".to_string(),
                        name: "Admin".to_string(),
                        access: AccessPrivileges::all(),
                    },
                ),
                (
                    "guest".to_string(),
                    Account {
                        password: "".to_string(),
                        name: "Guest".to_string(),
                        access: AccessPrivileges::guest_default(),
                    },
                ),
            ])),
        }
    }
}

#[async_trait]
impl AccountProvider for InMemoryAccountProvider {
    async fn get(&self, login: &str) -> Option<Account> {
        self.accounts.read().unwrap().get(login).cloned()
    }

    async fn put(&self, login: String, account: Account) -> Result<(), String> {
        self.accounts.write().unwrap().insert(login, account);
        Ok(())
    }

    async fn delete(&self, login: &str) -> Result<bool, String> {
        Ok(self.accounts.write().unwrap().remove(login).is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER_ALICE: &str = "alice";
    const PASS_ALICE: &str = "";
    const NAME_ALICE: &str = "Alice";

    fn alice() -> Account {
        Account {
            password: PASS_ALICE.to_string(),
            name: NAME_ALICE.to_string(),
            access: AccessPrivileges::CAN_BROADCAST,
        }
    }

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let provider = InMemoryAccountProvider::default();
        provider.put(USER_ALICE.to_string(), alice()).await.unwrap();
        assert_eq!(provider.get(USER_ALICE).await, Some(alice()));
    }

    #[tokio::test]
    async fn get_of_unknown_login_is_none() {
        let provider = InMemoryAccountProvider::default();
        assert_eq!(provider.get("nobody").await, None);
    }

    #[tokio::test]
    async fn remove_reports_whether_it_existed() {
        let provider = InMemoryAccountProvider::default();
        provider.put(USER_ALICE.to_string(), alice()).await.unwrap();

        assert_eq!(provider.delete(USER_ALICE).await, Ok(true));
        assert_eq!(provider.delete(USER_ALICE).await, Ok(false));
        assert_eq!(provider.get(USER_ALICE).await, None);
    }

    #[tokio::test]
    async fn default_seeds_an_admin_account() {
        let provider = InMemoryAccountProvider::default();
        let admin = provider.get("admin").await.expect("admin account exists");
        assert_eq!(admin.password, "admin");
        assert_eq!(admin.access, AccessPrivileges::all());
    }

    #[tokio::test]
    async fn default_seeds_a_guest_account() {
        let provider = InMemoryAccountProvider::default();
        let guest = provider.get("guest").await.expect("guest account exists");
        assert_eq!(guest.password, "");
        assert_eq!(guest.access, AccessPrivileges::guest_default());
    }
}
