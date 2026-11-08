//! Account administration. The admin opcodes changed over time: a server
//! at version 184 or more lists and saves accounts in bulk with
//! [`myTran_GetUserList`] (348) and [`myTran_SetUserList`] (349). An older
//! server has only [`myTran_NewUser`] (350), [`myTran_DeleteUser`] (351),
//! [`myTran_GetUser`] (352), and [`myTran_SetUser`] (353).
//!
//! [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
//! [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
//! [`myTran_NewUser`]: hotline_proto::Opcode::NewUser
//! [`myTran_DeleteUser`]: hotline_proto::Opcode::DeleteUser
//! [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
//! [`myTran_SetUser`]: hotline_proto::Opcode::SetUser

use hotline_client::{AccountChange, HotlineClient, UserAccount};
use hotline_proto::Access;

use super::ClientSession;
use crate::line::LineKind;

/// The lowest server version that lists and saves accounts in bulk.
const BULK_ACCOUNTS_VERSION: u16 = 184;

impl ClientSession {
    /// `true` if the server lists and saves accounts in bulk.
    fn bulk_accounts(&self) -> bool {
        self.server_at_least(BULK_ACCOUNTS_VERSION)
    }

    /// List every account, or keep the accounts opened so far on an older
    /// server. Without the open user privilege, it lists none.
    pub async fn list_accounts(&mut self, client: &HotlineClient) {
        if !self.granted().contains(Access::OPEN_USER) {
            self.accounts.get_or_insert_default();
            return;
        }
        if !self.bulk_accounts() {
            if self.accounts.is_none() {
                self.accounts = Some(Vec::new());
                self.push(
                    "this server cannot list accounts, press o to open one",
                    LineKind::System,
                );
            }
            return;
        }
        match client.get_user_list().await {
            Ok(mut accounts) => {
                accounts.sort_by(|a, b| a.login.cmp(&b.login));
                self.accounts = Some(accounts);
            }
            Err(e) => {
                self.accounts = Some(Vec::new());
                self.push(format!("failed to list accounts: {e}"), LineKind::Error);
            }
        }
    }

    /// Read the account `login` and add it to the list.
    pub async fn open_account(
        &mut self,
        client: &HotlineClient,
        login: &str,
    ) -> Option<UserAccount> {
        match client.get_user(login).await {
            Ok(account) => {
                self.keep_account(account.clone());
                Some(account)
            }
            Err(e) => {
                self.push(format!("failed to open {login}: {e}"), LineKind::Error);
                None
            }
        }
    }

    /// Create `account`, or replace the account `old_login`.
    pub async fn save_account(
        &mut self,
        client: &HotlineClient,
        old_login: Option<Box<str>>,
        account: UserAccount,
    ) {
        let login = account.login.clone();
        let saved = if self.bulk_accounts() {
            let change = AccountChange::Save { old_login, account };
            client.set_user_list(&[change]).await
        } else {
            match old_login {
                None => client.new_user(&account).await,
                Some(old) if old == account.login => client.set_user(&account).await,
                Some(_) => {
                    self.push("this server cannot rename an account", LineKind::Error);
                    return;
                }
            }
        };
        match saved {
            Ok(()) if self.bulk_accounts() => self.list_accounts(client).await,
            Ok(()) => {
                let _ = self.open_account(client, &login).await;
            }
            Err(e) => self.push(format!("failed to save {login}: {e}"), LineKind::Error),
        }
    }

    /// Delete the account `login`.
    pub async fn delete_account(&mut self, client: &HotlineClient, login: &str) {
        let deleted = if self.bulk_accounts() {
            let change = AccountChange::Delete(login.into());
            client.set_user_list(&[change]).await
        } else {
            client.delete_user(login).await
        };
        if let Err(e) = deleted {
            self.push(format!("failed to delete {login}: {e}"), LineKind::Error);
        } else if let Some(accounts) = &mut self.accounts {
            accounts.retain(|a| &*a.login != login);
        }
    }

    /// Add `account` to the list, or replace the listed account with its login.
    fn keep_account(&mut self, account: UserAccount) {
        let accounts = self.accounts.get_or_insert_with(Vec::new);
        match accounts.iter_mut().find(|a| a.login == account.login) {
            Some(listed) => *listed = account,
            None => {
                accounts.push(account);
                accounts.sort_by(|a, b| a.login.cmp(&b.login));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use hotline_client::Password;

    use super::*;
    use crate::session::test_support;

    fn account(login: &str) -> UserAccount {
        UserAccount {
            name: login.to_uppercase().into(),
            login: login.into(),
            password: Password::Empty,
            access: Access::READ_CHAT,
        }
    }

    fn logins(session: &ClientSession) -> Vec<&str> {
        let accounts = session.accounts.as_deref().unwrap_or_default();
        accounts.iter().map(|a| &*a.login).collect()
    }

    async fn session(version: u16) -> (ClientSession, HotlineClient) {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        session.server_version = Some(version);
        session.access = Access::all();
        (session, client)
    }

    #[tokio::test]
    async fn list_accounts_lists_none_without_open_user() {
        let (mut session, client) = session(190).await;
        session.access = Access::CREATE_USER;
        session.list_accounts(&client).await;
        assert_eq!(session.accounts, Some(Vec::new()));
    }

    #[tokio::test]
    async fn a_new_server_lists_saves_and_deletes_in_bulk() {
        let (mut session, client) = session(184).await;
        session.list_accounts(&client).await;
        assert_eq!(logins(&session), ["admin", "guest"]);

        session.save_account(&client, None, account("bo")).await;
        assert_eq!(logins(&session), ["admin", "bo", "guest"]);
        session
            .save_account(&client, Some("bo".into()), account("cy"))
            .await;
        assert_eq!(logins(&session), ["admin", "cy", "guest"]);
        session.delete_account(&client, "cy").await;
        assert_eq!(logins(&session), ["admin", "guest"]);
    }

    #[tokio::test]
    async fn an_old_server_opens_saves_and_deletes_one_account() {
        let (mut session, client) = session(151).await;
        session.list_accounts(&client).await;
        assert_eq!(logins(&session), Vec::<&str>::new());

        session.save_account(&client, None, account("bo")).await;
        assert_eq!(logins(&session), ["bo"]);
        session
            .save_account(&client, Some("bo".into()), account("cy"))
            .await;
        assert_eq!(logins(&session), ["bo"]);
        assert!(session.open_account(&client, "admin").await.is_some());
        assert_eq!(logins(&session), ["admin", "bo"]);
        session.delete_account(&client, "bo").await;
        assert_eq!(logins(&session), ["admin"]);
    }
}
