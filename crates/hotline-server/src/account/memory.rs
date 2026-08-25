use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use hotline_proto::consts::Access;

use crate::account::{AccessExtra, Account, AccountProvider};

/// An [`AccountProvider`] that keeps accounts in memory only; nothing
/// created at runtime survives a restart. Seeded with `admin`/`admin`
/// (full access) and a passwordless `guest` (default access).
#[derive(Debug)]
pub struct InMemoryAccountProvider {
    accounts: RwLock<HashMap<String, Account>>,
}

impl InMemoryAccountProvider {
    /// Unwraps into the underlying account map, e.g. to seed another
    /// [`AccountProvider`] with the same defaults.
    pub fn into_inner(self) -> HashMap<String, Account> {
        self.accounts.into_inner().unwrap()
    }
}

impl<T: Into<HashMap<String, Account>>> From<T> for InMemoryAccountProvider {
    fn from(value: T) -> Self {
        Self {
            accounts: RwLock::new(value.into()),
        }
    }
}

impl Default for InMemoryAccountProvider {
    fn default() -> Self {
        Self::from([
            (
                "admin".to_string(),
                Account {
                    password: "admin".to_string(),
                    name: "Admin".to_string(),
                    access: Access::all(),
                    access_extra: AccessExtra::all(),
                },
            ),
            (
                "guest".to_string(),
                Account {
                    password: "".to_string(),
                    name: "Guest".to_string(),
                    access: Access::default(),
                    access_extra: AccessExtra::default(),
                },
            ),
        ])
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

    async fn list(&self) -> Vec<(String, Account)> {
        self.accounts
            .read()
            .unwrap()
            .iter()
            .map(|(login, account)| (login.clone(), account.clone()))
            .collect()
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
            access: Access::BROADCAST,
            access_extra: AccessExtra::default(),
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
        assert_eq!(admin.access, Access::all());
    }

    #[tokio::test]
    async fn default_seeds_a_guest_account() {
        let provider = InMemoryAccountProvider::default();
        let guest = provider.get("guest").await.expect("guest account exists");
        assert_eq!(guest.password, "");
        assert_eq!(guest.access, Access::default());
    }
}
