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

bitflags::bitflags! {
    /// Per-account settings with no `USER_ACCESS` wire bit at all, only
    /// resolved server-side at login.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AccessExtra: u32 {
        /// Open or create a private chat.
        /// Config key: `chat_private`.
        const CHAT_PRIVATE       = 1 << 0;
        /// Send a private message even without `SEND_MSGS`.
        /// Config key: `msg`.
        const MSG                = 1 << 1;
        /// List users and see join/part/change traffic.
        /// Config key: `user_getlist`.
        const USER_GETLIST       = 1 << 2;
        /// List files.
        /// Config key: `file_list`.
        const FILE_LIST          = 1 << 3;
        /// Get a file's info.
        /// Config key: `file_getinfo`.
        const FILE_GETINFO       = 1 << 4;
        /// Get a file's hash.
        /// Config key: `file_hash`.
        const FILE_HASH          = 1 << 5;
        /// May process `HTLC_HDR_LOGIN`; cleared once logged in.
        /// Config key: `can_login`.
        const CAN_LOGIN          = 1 << 6;
        /// May be hidden from `GETLIST` while invisible.
        /// Config key: `user_visibility`.
        const USER_VISIBILITY    = 1 << 7;
        /// Set a custom color via a console command.
        /// Config key: `user_color`.
        const USER_COLOR         = 1 << 8;
        /// Exempt from spam/flood limits.
        /// Config key: `can_spam`.
        const CAN_SPAM           = 1 << 9;
        /// Set the global chat room's subject.
        /// Config key: `set_subject`.
        const SET_SUBJECT        = 1 << 10;
        /// Use debug console commands.
        /// Config key: `debug`.
        const DEBUG              = 1 << 11;
        /// View another user's access bits via a console command.
        /// Config key: `user_access`.
        const USER_ACCESS        = 1 << 12;
        /// Console-side access changes don't persist to the account.
        /// Config key: `access_volatile`.
        const ACCESS_VOLATILE    = 1 << 13;
        /// Use the console `/0wn` command on other users.
        /// Config key: `user_0wn`.
        const USER_0WN           = 1 << 14;
        /// Can be targeted by `/0wn`.
        /// Config key: `is_0wn3d`.
        const IS_0WN3D           = 1 << 15;
        /// Manage other users via console commands.
        /// Config key: `manage_users`.
        const MANAGE_USERS       = 1 << 16;
        /// Reveal this account's IP to `GETINFO` callers.
        /// Config key: `info_get_address`.
        const INFO_GET_ADDRESS   = 1 << 17;
        /// Reveal this account's login to `GETINFO` callers.
        /// Config key: `info_get_login`.
        const INFO_GET_LOGIN     = 1 << 18;
        /// Blocks `USE_ANY_NAME` from renaming via chat commands.
        /// Config key: `name_lock`.
        const NAME_LOCK          = 1 << 19;
        /// May (re-)agree to the server agreement.
        /// Config key: `can_agree`.
        const CAN_AGREE          = 1 << 20;
        /// May issue a ping/keepalive request.
        /// Config key: `can_ping`.
        const CAN_PING           = 1 << 21;
        /// Download the server banner image.
        /// Config key: `banner_get`.
        const BANNER_GET         = 1 << 22;
        /// Skip the file transfer queue.
        /// Config key: `ignore_queue`.
        const IGNORE_QUEUE       = 1 << 23;
    }
}

impl Default for AccessExtra {
    /// Matches the reference's own defaults for a fresh account.
    fn default() -> Self {
        Self::CHAT_PRIVATE
            | Self::USER_GETLIST
            | Self::FILE_LIST
            | Self::FILE_GETINFO
            | Self::FILE_HASH
            | Self::ACCESS_VOLATILE
            | Self::IS_0WN3D
            | Self::INFO_GET_ADDRESS
            | Self::INFO_GET_LOGIN
    }
}

/// One named account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    /// Compared for exact equality against the submitted password; an
    /// empty stored password requires an empty submission too.
    pub password: String,
    pub name: String,
    pub access: AccessPrivileges,
    /// See [`AccessExtra`].
    pub access_extra: AccessExtra,
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

/// The `admin`/`admin` (full access) and passwordless `guest` (default
/// access) accounts every fresh [`AccountProvider`] backend seeds itself
/// with, so there's always a way to log in and create further accounts.
pub fn default_accounts() -> [(String, Account); 2] {
    [
        (
            "admin".to_string(),
            Account {
                password: "admin".to_string(),
                name: "Admin".to_string(),
                access: AccessPrivileges::all(),
                access_extra: AccessExtra::default(),
            },
        ),
        (
            "guest".to_string(),
            Account {
                password: "".to_string(),
                name: "Guest".to_string(),
                access: AccessPrivileges::default(),
                access_extra: AccessExtra::default(),
            },
        ),
    ]
}

/// An [`AccountProvider`] that keeps accounts in memory only; nothing
/// created at runtime survives a restart. Seeded with [`default_accounts`].
#[derive(Debug)]
pub struct InMemoryAccountProvider {
    accounts: RwLock<HashMap<String, Account>>,
}

impl Default for InMemoryAccountProvider {
    fn default() -> Self {
        Self {
            accounts: RwLock::new(HashMap::from(default_accounts())),
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
        assert_eq!(admin.access, AccessPrivileges::all());
    }

    #[tokio::test]
    async fn default_seeds_a_guest_account() {
        let provider = InMemoryAccountProvider::default();
        let guest = provider.get("guest").await.expect("guest account exists");
        assert_eq!(guest.password, "");
        assert_eq!(guest.access, AccessPrivileges::default());
    }

    #[test]
    fn access_extra_default_matches_the_reference() {
        let expected = AccessExtra::CHAT_PRIVATE
            | AccessExtra::USER_GETLIST
            | AccessExtra::FILE_LIST
            | AccessExtra::FILE_GETINFO
            | AccessExtra::FILE_HASH
            | AccessExtra::ACCESS_VOLATILE
            | AccessExtra::IS_0WN3D
            | AccessExtra::INFO_GET_ADDRESS
            | AccessExtra::INFO_GET_LOGIN;
        assert_eq!(AccessExtra::default(), expected);
    }
}
