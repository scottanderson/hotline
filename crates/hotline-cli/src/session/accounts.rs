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

use super::{ClientSession, Request};
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
    pub fn list_accounts(&mut self, client: &HotlineClient) -> Request<()> {
        if !self.granted().contains(Access::OPEN_USER) {
            self.accounts.get_or_insert_default();
            return Request::ready(());
        }
        if !self.bulk_accounts() {
            if self.accounts.is_none() {
                self.accounts = Some(Vec::new());
                self.push(
                    "this server cannot list accounts, press o to open one",
                    LineKind::System,
                );
            }
            return Request::ready(());
        }
        let client = client.clone();
        Request::new(async move {
            let accounts = client.get_user_list().await;
            move |session: &mut Self| match accounts {
                Ok(mut accounts) => {
                    accounts.sort_by(|a, b| a.login.cmp(&b.login));
                    session.accounts = Some(accounts);
                }
                Err(e) => {
                    session.accounts = Some(Vec::new());
                    session.push(format!("failed to list accounts: {e}"), LineKind::Error);
                }
            }
        })
    }

    /// Read the account `login` and add it to the list.
    pub fn open_account(client: &HotlineClient, login: &str) -> Request<Option<UserAccount>> {
        let client = client.clone();
        let login = login.to_string();
        let failed = format!("failed to open {login}");
        Request::report(
            async move { client.get_user(&login).await },
            failed,
            |session: &mut Self, account| {
                session.keep_account(account.clone());
                Some(account)
            },
        )
    }

    /// Create `account`, or replace the account `old_login`.
    pub fn save_account(
        &mut self,
        client: &HotlineClient,
        old_login: Option<Box<str>>,
        account: UserAccount,
    ) -> Request<()> {
        let bulk = self.bulk_accounts();
        if !bulk && old_login.as_ref().is_some_and(|old| *old != account.login) {
            self.push("this server cannot rename an account", LineKind::Error);
            return Request::ready(());
        }
        let client = client.clone();
        let login = account.login.clone();
        Request::wait({
            let client = client.clone();
            async move {
                if bulk {
                    let change = AccountChange::Save { old_login, account };
                    client.set_user_list(&[change]).await
                } else if old_login.is_none() {
                    client.new_user(&account).await
                } else {
                    client.set_user(&account).await
                }
            }
        })
        .and_then(move |session, saved| match saved {
            Ok(()) if bulk => session.list_accounts(&client),
            Ok(()) => Self::open_account(&client, &login).map(|_, _| ()),
            Err(e) => {
                session.push(format!("failed to save {login}: {e}"), LineKind::Error);
                Request::ready(())
            }
        })
    }

    /// Delete the account `login`.
    pub fn delete_account(&self, client: &HotlineClient, login: &str) -> Request<()> {
        let bulk = self.bulk_accounts();
        let client = client.clone();
        let login = login.to_string();
        let failed = format!("failed to delete {login}");
        Request::report(
            async move {
                let deleted = if bulk {
                    let change = AccountChange::Delete(login.as_str().into());
                    client.set_user_list(&[change]).await
                } else {
                    client.delete_user(&login).await
                };
                deleted.map(|()| login)
            },
            failed,
            |session: &mut Self, login| {
                if let Some(accounts) = &mut session.accounts {
                    accounts.retain(|a| *a.login != *login);
                }
            },
        )
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
        session.list_accounts(&client).run(&mut session).await;
        assert_eq!(session.accounts, Some(Vec::new()));
    }

    #[tokio::test]
    async fn a_new_server_lists_saves_and_deletes_in_bulk() {
        let (mut session, client) = session(184).await;
        session.list_accounts(&client).run(&mut session).await;
        assert_eq!(logins(&session), ["admin", "guest"]);

        session
            .save_account(&client, None, account("bo"))
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["admin", "bo", "guest"]);
        session
            .save_account(&client, Some("bo".into()), account("cy"))
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["admin", "cy", "guest"]);
        session
            .delete_account(&client, "cy")
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["admin", "guest"]);
    }

    #[tokio::test]
    async fn an_old_server_opens_saves_and_deletes_one_account() {
        let (mut session, client) = session(151).await;
        session.list_accounts(&client).run(&mut session).await;
        assert_eq!(logins(&session), Vec::<&str>::new());

        session
            .save_account(&client, None, account("bo"))
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["bo"]);
        session
            .save_account(&client, Some("bo".into()), account("cy"))
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["bo"]);
        assert!(
            ClientSession::open_account(&client, "admin")
                .run(&mut session)
                .await
                .is_some()
        );
        assert_eq!(logins(&session), ["admin", "bo"]);
        session
            .delete_account(&client, "bo")
            .run(&mut session)
            .await;
        assert_eq!(logins(&session), ["admin"]);
    }
}
