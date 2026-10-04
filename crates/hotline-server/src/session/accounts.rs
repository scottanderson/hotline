//! Account CRUD and the bulk user-list admin opcodes.

use std::borrow::Cow;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::text_encoding::Encoding;
use hotline_proto::FieldId;
use hotline_proto::{Field, FieldList, Transaction};

use crate::account::{AccessExtra, Account};
use crate::activity::{AccountAction, Activity};
use crate::state::{AccessUpdate, SharedState};

use super::connection::Session;

impl Session {
    /// [`myTran_NewUser`] (350)
    ///
    /// [`myTran_NewUser`]: hotline_proto::Opcode::NewUser
    pub(super) async fn on_new_user(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.login(encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let access = tx.access().unwrap_or_else(Access::empty);
        if !self.access.contains(access - Access::NO_AGREEMENT) {
            return vec![tx.reply_error("Cannot create account with more access than yourself.")];
        }
        let account = Account {
            password: tx.password(encoding).unwrap_or_default().into(),
            name: tx.name(encoding).unwrap_or_default().into(),
            access,
            access_extra: AccessExtra::default(),
        };
        if let Err(reason) = state.create_account(login.clone(), account).await {
            return vec![tx.reply_error(reason)];
        }
        self.record_account_change(AccountAction::Create, login, state)
            .await;
        vec![tx.reply(vec![])]
    }

    /// [`myTran_DeleteUser`] (351)
    ///
    /// [`myTran_DeleteUser`]: hotline_proto::Opcode::DeleteUser
    pub(super) async fn on_delete_user(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DELETE_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.login(encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        if let Err(reason) = state.delete_account(&login).await {
            return vec![tx.reply_error(reason)];
        }
        Self::disconnect_account_sessions(&login, state).await;
        self.record_account_change(AccountAction::Delete, login, state)
            .await;
        vec![tx.reply(vec![])]
    }

    /// Report an account change to the activity log.
    async fn record_account_change(
        &self,
        action: AccountAction,
        account: String,
        state: &SharedState,
    ) {
        self.record_activity(
            Activity::AccountChange {
                action,
                account: account.into(),
            },
            state,
        )
        .await;
    }

    /// Disconnect every session currently logged in under `login`, with
    /// openline's own message text for an account deleted out from
    /// under it.
    pub(super) async fn disconnect_account_sessions(login: &str, state: &SharedState) {
        for target in state.user_ids_for_login(login).await {
            state
                .send_to(
                    target,
                    Transaction::push(
                        Opcode::DisconnectMsg,
                        vec![Field::string(
                            FieldId::Data,
                            "You are logged in with an account which was deleted.",
                        )],
                    ),
                )
                .await;
        }
    }

    /// [`myTran_GetUser`] (352)
    ///
    /// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
    pub(super) async fn on_get_user(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::OPEN_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.string(FieldId::UserLogin, encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let Some(account) = state.account(&login).await else {
            return vec![tx.reply_error("no such account")];
        };
        vec![tx.reply(vec![
            Field::string(FieldId::UserName, account.name),
            Field::obfuscated_string(FieldId::UserLogin, login),
            Self::redacted_password_field(&account.password),
            Field::u64(FieldId::UserAccess, account.access.bits()),
        ])]
    }

    /// [`myTran_SetUser`] (353)
    ///
    /// [`myTran_SetUser`]: hotline_proto::Opcode::SetUser
    pub(super) async fn on_set_user(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::MODIFY_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.login(encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let password = Self::new_password(tx.field(FieldId::UserPassword), encoding);
        let name = tx.name(encoding);
        let access = tx.access();
        let edit = |existing: Account| Account {
            password: password.unwrap_or(existing.password),
            name: name.map_or(existing.name, Into::into),
            access: access.unwrap_or(existing.access),
            access_extra: existing.access_extra,
        };
        let account = match state.replace_account(&login, login.clone(), edit).await {
            Ok(account) => account,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        state
            .push_access_update(AccessUpdate {
                login: login.as_str().into(),
                renamed_to: None,
                access: account.access,
                name: (!account.name.is_empty()).then_some(account.name),
            })
            .await;
        self.record_account_change(AccountAction::Modify, login, state)
            .await;
        vec![tx.reply(vec![])]
    }

    /// The new password in a [`myField_UserPassword`] (106) field, or `None`
    /// for the single byte 0, which keeps the old one. No field clears it.
    ///
    /// [`myField_UserPassword`]: hotline_proto::FieldId::UserPassword
    fn new_password(field: Option<&Field>, encoding: &'static Encoding) -> Option<Box<str>> {
        match field {
            Some(f) if f.as_bytes(encoding).as_ref() == [0u8] => None,
            Some(f) => Some(f.as_deobfuscated_string(encoding).into()),
            None => Some(Box::default()),
        }
    }

    /// The password field as sent back for [`myTran_GetUser`] (352)
    /// and each record of [`myTran_GetUserList`] (348): empty if unset,
    /// a single `x` otherwise, so the real password is never echoed.
    ///
    /// [`myTran_GetUser`]: hotline_proto::Opcode::GetUser
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub(super) fn redacted_password_field(password: &str) -> Field {
        Field::string(
            FieldId::UserPassword,
            if password.is_empty() { "" } else { "x" },
        )
    }

    /// [`myTran_GetUserList`] (348): every account at once, for the
    /// bulk account editor. Each account is one [`myField_Data`] (101) field
    /// wrapping its own nested field list of [`myField_UserName`] (102),
    /// [`myField_UserLogin`] (105), [`myField_UserPassword`] (106), and
    /// [`myField_UserAccess`] (110), the same `count`-prefixed shape a
    /// transaction body uses.
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    /// [`myField_UserName`]: hotline_proto::FieldId::UserName
    /// [`myField_UserLogin`]: hotline_proto::FieldId::UserLogin
    /// [`myField_UserPassword`]: hotline_proto::FieldId::UserPassword
    /// [`myField_UserAccess`]: hotline_proto::FieldId::UserAccess
    pub(super) async fn on_get_user_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::OPEN_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let fields = state
            .list_accounts()
            .await
            .into_iter()
            .filter_map(|(login, account)| {
                FieldList::encode(
                    &[
                        Field::string(FieldId::UserName, account.name),
                        Field::obfuscated_string(FieldId::UserLogin, login),
                        Self::redacted_password_field(&account.password),
                        Field::u64(FieldId::UserAccess, account.access.bits()),
                    ],
                    encoding,
                )
                .ok()
            })
            .map(|record| Field::bytes(FieldId::Data, record))
            .collect();
        vec![tx.reply(fields)]
    }

    /// [`myTran_SetUserList`] (349): save a bulk-edited account list.
    /// Each [`myField_Data`] (101) field is one nested account record in the same
    /// shape [`myTran_GetUserList`] (348) sent; see
    /// [`Session::apply_user_list_record`] for the per-record rules.
    /// Processes every record even after a soft (storage) failure, but
    /// stops and reports immediately on a hard (permission) failure,
    /// matching openline.
    ///
    /// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub(super) async fn on_set_user_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let encoding = self.encoding();
        let mut save_error = false;
        for field in tx.fields(FieldId::Data) {
            let Ok(record) = FieldList::decode(&field.as_bytes(encoding)) else {
                save_error = true;
                continue;
            };
            match self.apply_user_list_record(&record, state).await {
                Ok(true) => {}
                Ok(false) => save_error = true,
                Err(reason) => return vec![tx.reply_error(reason)],
            }
        }
        if save_error {
            return vec![tx.reply_error("Error saving list of accounts.")];
        }
        vec![tx.reply(vec![])]
    }

    /// Apply one [`myTran_SetUserList`] (349) record: [`myField_Data`] (101), if
    /// present, is the account's login before this edit (deobfuscated,
    /// scrambled the same way as [`myField_UserLogin`] (105)), absent for a
    /// brand new record.
    /// An empty name and login means delete; otherwise this is a rename
    /// (login changed), a plain modify, or a create, depending on whether
    /// the old login was given and whether the new login already exists.
    ///
    /// `Ok(true)` if applied, `Ok(false)` for a soft (storage) failure
    /// that shouldn't stop the rest of the list from applying, `Err` for
    /// a hard (permission) failure that aborts the whole request.
    ///
    /// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    /// [`myField_UserLogin`]: hotline_proto::FieldId::UserLogin
    pub(super) async fn apply_user_list_record(
        &self,
        record: &FieldList,
        state: &SharedState,
    ) -> std::result::Result<bool, Cow<'static, str>> {
        let encoding = self.encoding();
        let as_deobfuscated_string = |f: &Field| f.as_deobfuscated_string(encoding);
        let old_login = record
            .field(FieldId::Data)
            .map(as_deobfuscated_string)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        let name = record
            .string(FieldId::UserName, encoding)
            .unwrap_or_default();
        let login = record
            .field(FieldId::UserLogin)
            .map(as_deobfuscated_string)
            .unwrap_or_default()
            .to_lowercase();

        if name.is_empty() && login.is_empty() {
            let Some(old_login) = old_login else {
                return Ok(false);
            };
            self.access.require(Access::DELETE_USER)?;
            return match state.delete_account(&old_login).await {
                Ok(()) => {
                    Self::disconnect_account_sessions(&old_login, state).await;
                    self.record_account_change(AccountAction::Delete, old_login, state)
                        .await;
                    Ok(true)
                }
                Err(_) => Ok(false),
            };
        }

        let password = Self::new_password(record.field(FieldId::UserPassword), encoding);
        let access = record
            .field(FieldId::UserAccess)
            .and_then(Field::as_u64)
            .map(Access::from_bits_truncate)
            .unwrap_or_else(Access::empty);

        if let Some(old_login) = old_login {
            // Modify (and rename, if the login changed).
            self.access.require(Access::MODIFY_USER)?;
            let renamed = old_login != login;
            let edit = |existing: Account| Account {
                password: password.unwrap_or(existing.password),
                name: name.as_str().into(),
                access,
                access_extra: existing.access_extra,
            };
            return match state.replace_account(&old_login, login.clone(), edit).await {
                Ok(_) => {
                    state
                        .push_access_update(AccessUpdate {
                            login: old_login.as_str().into(),
                            renamed_to: renamed.then(|| login.as_str().into()),
                            access,
                            name: (!name.is_empty()).then(|| name.into()),
                        })
                        .await;
                    if renamed {
                        let action = AccountAction::Rename {
                            from: old_login.into(),
                        };
                        self.record_account_change(action, login.clone(), state)
                            .await;
                    }
                    self.record_account_change(AccountAction::Modify, login, state)
                        .await;
                    Ok(true)
                }
                Err(_) => Ok(false),
            };
        }

        // A new record whose login is taken gets a notice and is skipped.
        if state.account(&login).await.is_some() {
            state
                .send_to(
                    self.user_id,
                    Transaction::push(
                        Opcode::ServerMsg,
                        vec![Field::string(
                            FieldId::Data,
                            format!(
                                "Cannot create account \"{login}\" because there is already an account with that login."
                            ),
                        )],
                    ),
                )
                .await;
            return Ok(true);
        }

        self.access.require(Access::CREATE_USER)?;
        if !self.access.contains(access - Access::NO_AGREEMENT) {
            Err("Cannot create account with more access than yourself.")?;
        }
        let Some(password) = password else {
            return Ok(false);
        };
        let account = Account {
            password,
            name: name.into(),
            access,
            access_extra: AccessExtra::default(),
        };
        match state.create_account(login.clone(), account).await {
            Ok(()) => {
                self.record_account_change(AccountAction::Create, login, state)
                    .await;
                Ok(true)
            }
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::AccountProvider;
    use crate::state::{RecipientMessage, UserEntry};
    use hotline_proto::opcode::TransactionType;
    use std::time::Instant;

    use crate::session::test_support::*;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn new_user_then_get_user_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            Opcode::NewUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::obfuscated_string(FieldId::UserPassword, "secret"),
                Field::string(FieldId::UserName, "Bob"),
                Field::u64(FieldId::UserAccess, Access::SEND_CHAT.bits()),
            ],
        );
        let replies = session.on_new_user(&create, &state).await;
        assert!(!replies[0].is_error());

        let read = Transaction::request(
            Opcode::GetUser,
            2,
            vec![Field::obfuscated_string(FieldId::UserLogin, "bob")],
        );
        let replies = session.on_get_user(&read, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(
            replies[0].string(FieldId::UserName, encoding).unwrap(),
            "Bob"
        );
        assert_eq!(
            replies[0]
                .field(FieldId::UserAccess)
                .and_then(Field::as_u64),
            Some(Access::SEND_CHAT.bits())
        );
    }

    #[tokio::test]
    async fn new_user_folds_the_login_to_lowercase() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            Opcode::NewUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "Bob"),
                Field::obfuscated_string(FieldId::UserPassword, "secret"),
                Field::string(FieldId::UserName, "Bob"),
                Field::u64(FieldId::UserAccess, Access::SEND_CHAT.bits()),
            ],
        );
        assert!(!session.on_new_user(&create, &state).await[0].is_error());

        assert!(state.account("bob").await.is_some());
        assert!(state.account("Bob").await.is_none());
    }

    #[tokio::test]
    async fn new_user_refuses_a_taken_login() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account("bob".to_string(), Account::guest())
            .await
            .unwrap();

        let create = Transaction::request(
            Opcode::NewUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::u64(FieldId::UserAccess, Access::all().bits()),
            ],
        );
        let replies = session.on_new_user(&create, &state).await;
        assert_eq!(
            replies[0].string(FieldId::ErrorText, encoding).unwrap(),
            "Cannot create account \"bob\" because there is already an account with that login."
        );
        assert_eq!(state.account("bob").await.unwrap(), Account::guest());
    }

    #[tokio::test]
    async fn new_user_cannot_create_with_more_access_than_self() {
        let session = test_session("Admin", Access::CREATE_USER);
        let state = SharedState::default();

        let create = Transaction::request(
            Opcode::NewUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::u64(FieldId::UserAccess, Access::all().bits()),
            ],
        );
        assert!(session.on_new_user(&create, &state).await[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn set_user_refuses_a_missing_login() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let modify = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::string(FieldId::UserName, "Bob"),
            ],
        );
        let replies = session.on_set_user(&modify, &state).await;
        assert_eq!(
            replies[0].string(FieldId::ErrorText, encoding).unwrap(),
            "No account with login \"bob\"."
        );
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn delete_user_matches_the_login_regardless_of_case() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let delete = Transaction::request(
            Opcode::DeleteUser,
            1,
            vec![Field::obfuscated_string(FieldId::UserLogin, "BOB")],
        );
        assert!(!session.on_delete_user(&delete, &state).await[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn set_user_only_overwrites_fields_present() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        // Password unchanged via the sentinel, not by omitting the field.
        let modify = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::string(FieldId::UserName, "Bobby"),
                Field::bytes(FieldId::UserPassword, vec![0]),
            ],
        );
        session.on_set_user(&modify, &state).await;

        let account = state.account("bob").await.unwrap();
        assert_eq!(&*account.name, "Bobby");
        assert_eq!(&*account.password, "secret");
        assert_eq!(account.access, Access::SEND_CHAT);
    }

    #[tokio::test]
    async fn set_user_without_a_password_field_at_all_clears_it() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        // No PASSWORD field at all, not the "unchanged" sentinel: this
        // clears the password, matching openline's own behavior.
        let modify = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::string(FieldId::UserName, "Bobby"),
            ],
        );
        session.on_set_user(&modify, &state).await;

        let account = state.account("bob").await.unwrap();
        assert_eq!(&*account.password, "");
    }

    #[tokio::test]
    async fn get_user_never_echoes_the_real_password() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();
        state
            .put_account(
                "carol".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Carol".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let read = |login: &str| {
            Transaction::request(
                Opcode::GetUser,
                1,
                vec![Field::obfuscated_string(FieldId::UserLogin, login)],
            )
        };

        let [with_password] = session
            .on_get_user(&read("bob"), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            with_password
                .field(FieldId::UserPassword)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b"x"
        );

        let [without_password] = session
            .on_get_user(&read("carol"), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            without_password
                .field(FieldId::UserPassword)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b""
        );
    }

    #[tokio::test]
    async fn set_user_propagates_to_a_live_session_without_reconnect() {
        let admin = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Bob".into(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        // Bob is already connected, logged in under the "bob" account.
        let mut bob = test_session("Bob", Access::empty());
        bob.login = "bob".into();
        state
            .insert_user(UserEntry {
                user_id: bob.user_id,
                login: bob.login.clone(),
                name: bob.name.clone(),
                icon: 0,
                client_version: None,
                access: bob.access,
                access_extra: bob.access_extra,
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let mut bob_rx = state.register_recipient(bob.user_id).await;

        let modify = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::u64(FieldId::UserAccess, Access::BROADCAST.bits()),
            ],
        );
        admin.on_set_user(&modify, &state).await;

        // Simulate bob's session loop applying the access update.
        let received = bob_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_access_update)
            .expect("expected an access update");
        bob.apply_access_update(received, &state).await;

        assert_eq!(bob.access, Access::BROADCAST);
        let notify = bob_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("the live update should also push a UserChange");
        assert_eq!(
            notify.field(FieldId::UserID).and_then(Field::as_u16),
            Some(bob.user_id)
        );
    }

    #[tokio::test]
    async fn set_user_renames_only_when_use_any_name_is_revoked() {
        let admin = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Bob".into(),
                    access: Access::ANY_NAME,
                },
            )
            .await
            .unwrap();

        // Bob is connected with USE_ANY_NAME and has picked a custom name.
        let mut bob = test_session("Bobby the Great", Access::ANY_NAME);
        bob.login = "bob".into();
        state
            .insert_user(UserEntry {
                user_id: bob.user_id,
                login: bob.login.clone(),
                name: bob.name.clone(),
                icon: 0,
                client_version: None,
                access: bob.access,
                access_extra: bob.access_extra,
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let mut bob_rx = state.register_recipient(bob.user_id).await;

        let revoke = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::u64(FieldId::UserAccess, Access::empty().bits()),
            ],
        );
        admin.on_set_user(&revoke, &state).await;
        let update = next_access_update(&mut bob_rx);
        bob.apply_access_update(update, &state).await;
        assert_eq!(&*bob.name, "Bob");

        // Still holds USE_ANY_NAME, so this modify must not touch the name.
        bob.name = "Bobby Returns".into();
        bob.access = Access::ANY_NAME;
        let regrant = Transaction::request(
            Opcode::SetUser,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "bob"),
                Field::u64(
                    FieldId::UserAccess,
                    (Access::ANY_NAME | Access::SEND_CHAT).bits(),
                ),
            ],
        );
        admin.on_set_user(&regrant, &state).await;
        let update = next_access_update(&mut bob_rx);
        bob.apply_access_update(update, &state).await;
        assert_eq!(&*bob.name, "Bobby Returns");
    }

    #[tokio::test]
    async fn delete_user_removes_it() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Bob".into(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        let delete = Transaction::request(
            Opcode::DeleteUser,
            1,
            vec![Field::obfuscated_string(FieldId::UserLogin, "bob")],
        );
        let replies = session.on_delete_user(&delete, &state).await;
        assert!(!replies[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn account_crud_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let login_field = Field::obfuscated_string(FieldId::UserLogin, "bob");

        let create = Transaction::request(Opcode::NewUser, 1, vec![login_field.clone()]);
        assert!(session.on_new_user(&create, &state).await[0].is_error());

        let read = Transaction::request(Opcode::GetUser, 2, vec![login_field.clone()]);
        assert!(session.on_get_user(&read, &state).await[0].is_error());

        let modify = Transaction::request(Opcode::SetUser, 3, vec![login_field.clone()]);
        assert!(session.on_set_user(&modify, &state).await[0].is_error());

        let delete = Transaction::request(Opcode::DeleteUser, 4, vec![login_field]);
        assert!(session.on_delete_user(&delete, &state).await[0].is_error());
    }

    /// One [`myTran_SetUserList`] (349) record: `old_login` is the account's
    /// login before this edit, `None` for a brand new record.
    ///
    /// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
    fn user_list_record(
        old_login: Option<&str>,
        name: &str,
        login: &str,
        password: Field,
        access: Access,
    ) -> Field {
        let encoding = text_encoding::DEFAULT;
        let mut fields = Vec::new();
        if let Some(old_login) = old_login {
            fields.push(Field::obfuscated_string(FieldId::Data, old_login));
        }
        if !name.is_empty() {
            fields.push(Field::string(FieldId::UserName, name));
        }
        if !login.is_empty() {
            fields.push(Field::obfuscated_string(FieldId::UserLogin, login));
        }
        fields.push(password);
        fields.push(Field::u64(FieldId::UserAccess, access.bits()));
        Field::bytes(FieldId::Data, FieldList::encode(&fields, encoding).unwrap())
    }

    #[tokio::test]
    async fn get_user_list_denied_without_open_user() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let request = Transaction::request(Opcode::GetUserList, 1, vec![]);
        assert!(session.on_get_user_list(&request, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn get_user_list_returns_every_account_with_a_redacted_password() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let request = Transaction::request(Opcode::GetUserList, 1, vec![]);
        let [reply] = session
            .on_get_user_list(&request, &state)
            .await
            .try_into()
            .unwrap();

        // The seeded admin/guest accounts plus the one just added.
        let records: Vec<_> = reply.fields(FieldId::Data).collect();
        assert_eq!(records.len(), 3);

        let bob = records
            .iter()
            .map(|f| FieldList::decode(&f.as_bytes(encoding)).unwrap())
            .find(|fields| fields.string(FieldId::UserName, encoding).as_deref() == Some("Bob"))
            .expect("bob's record");
        let login = bob
            .field(FieldId::UserLogin)
            .unwrap()
            .as_deobfuscated_string(encoding);
        assert_eq!(login, "bob");
        let password = bob.field(FieldId::UserPassword).unwrap();
        assert_eq!(password.as_bytes(encoding).as_ref(), b"x");
    }

    #[tokio::test]
    async fn set_user_list_creates_a_new_account() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let record = user_list_record(
            None,
            "Bob",
            "bob",
            Field::obfuscated_string(FieldId::UserPassword, "secret"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());

        let account = state.account("bob").await.unwrap();
        assert_eq!(&*account.name, "Bob");
        assert_eq!(&*account.password, "secret");
        assert_eq!(account.access, Access::SEND_CHAT);
    }

    #[tokio::test]
    async fn set_user_list_cannot_create_with_more_access_than_self() {
        let session = test_session("Admin", Access::SEND_CHAT);
        let state = SharedState::default();

        let record = user_list_record(
            None,
            "Bob",
            "bob",
            Field::obfuscated_string(FieldId::UserPassword, "secret"),
            Access::all(),
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(replies[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn set_user_list_creating_over_an_existing_login_is_a_notice_not_an_error() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "original".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();
        let mut rx = state.register_recipient(1).await;

        let record = user_list_record(
            None,
            "Bob Two",
            "bob",
            Field::obfuscated_string(FieldId::UserPassword, "new"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());
        // Untouched: the collision is a notice, not an applied overwrite.
        assert_eq!(&*state.account("bob").await.unwrap().password, "original");
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn set_user_list_renames_and_reuses_the_password_on_the_unchanged_sentinel() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let record = user_list_record(
            Some("bob"),
            "Bobby",
            "bobby",
            Field::bytes(FieldId::UserPassword, vec![0u8]),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());

        assert!(state.account("bob").await.is_none());
        let renamed = state.account("bobby").await.unwrap();
        assert_eq!(&*renamed.name, "Bobby");
        assert_eq!(&*renamed.password, "secret");
    }

    #[tokio::test]
    async fn account_changes_are_recorded_after_they_succeed() {
        let session = test_session("Admin", Access::all());
        let (state, log) = state_recording_activity(Default::default());
        let login = |login: &str| vec![Field::obfuscated_string(FieldId::UserLogin, login)];

        let create = Transaction::request(Opcode::NewUser, 1, login("bob"));
        session.on_new_user(&create, &state).await;
        let modify = Transaction::request(Opcode::SetUser, 2, login("bob"));
        session.on_set_user(&modify, &state).await;
        let delete = Transaction::request(Opcode::DeleteUser, 3, login("bob"));
        session.on_delete_user(&delete, &state).await;
        let missing = Transaction::request(Opcode::DeleteUser, 4, login("bob"));
        session.on_delete_user(&missing, &state).await;

        let change = |action, account: &str| Activity::AccountChange {
            action,
            account: account.into(),
        };
        assert_eq!(
            log.activities(),
            [
                change(AccountAction::Create, "bob"),
                change(AccountAction::Modify, "bob"),
                change(AccountAction::Delete, "bob"),
            ]
        );
    }

    #[tokio::test]
    async fn set_user_list_records_a_rename_then_a_modify() {
        let session = test_session("Admin", Access::all());
        let (state, log) = state_recording_activity(Default::default());
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".into(),
                    name: "Bob".into(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let record = user_list_record(
            Some("bob"),
            "Bobby",
            "bobby",
            Field::bytes(FieldId::UserPassword, vec![0u8]),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        session.on_set_user_list(&request, &state).await;

        let change = |action| Activity::AccountChange {
            action,
            account: "bobby".into(),
        };
        assert_eq!(
            log.activities(),
            [
                change(AccountAction::Rename { from: "bob".into() }),
                change(AccountAction::Modify),
            ]
        );
    }

    /// Account storage whose `put` always fails.
    struct FailingPutAccounts(crate::account::InMemoryAccountProvider);

    #[async_trait::async_trait]
    impl AccountProvider for FailingPutAccounts {
        async fn get(&self, login: &str) -> Option<Account> {
            self.0.get(login).await
        }

        async fn put(&self, _login: String, _account: Account) -> Result<(), Cow<'static, str>> {
            Err("disk full")?
        }

        async fn delete(&self, login: &str) -> Result<bool, Cow<'static, str>> {
            self.0.delete(login).await
        }

        async fn list(&self) -> Vec<(String, Account)> {
            self.0.list().await
        }
    }

    #[tokio::test]
    async fn set_user_list_keeps_the_old_login_when_a_rename_fails_to_save() {
        let session = test_session("Admin", Access::all());
        let accounts = crate::account::InMemoryAccountProvider::default();
        let bob = Account {
            access_extra: AccessExtra::default(),
            password: "secret".into(),
            name: "Bob".into(),
            access: Access::SEND_CHAT,
        };
        accounts.put("bob".to_string(), bob).await.unwrap();
        let state = state_with_accounts(std::sync::Arc::new(FailingPutAccounts(accounts)));

        let record = user_list_record(
            Some("bob"),
            "Bobby",
            "bobby",
            Field::bytes(FieldId::UserPassword, vec![0u8]),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        session.on_set_user_list(&request, &state).await;

        assert!(state.account("bob").await.is_some());
        assert!(state.account("bobby").await.is_none());
    }

    #[tokio::test]
    async fn set_user_list_rename_updates_sessions_under_the_old_login() {
        let admin = test_session("Admin", Access::all());
        let state = SharedState::default();
        let account = Account {
            access_extra: AccessExtra::default(),
            password: "secret".into(),
            name: "Bob".into(),
            access: Access::empty(),
        };
        state.put_account("bob".to_string(), account).await.unwrap();
        let mut bob = test_session("Bob", Access::empty());
        bob.login = "bob".into();
        let mut bob_rx = state.register_recipient(bob.user_id).await;

        let record = user_list_record(
            Some("bob"),
            "Robert",
            "bobby",
            Field::bytes(FieldId::UserPassword, vec![0u8]),
            Access::BROADCAST,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        admin.on_set_user_list(&request, &state).await;
        let update = next_access_update(&mut bob_rx);
        bob.apply_access_update(update, &state).await;

        assert_eq!(bob.access, Access::BROADCAST);
        assert_eq!(&*bob.login, "bobby");
        assert_eq!(&*bob.account_name, "Robert");
        assert_eq!(state.user_ids_for_login("bobby").await, [bob.user_id]);
    }

    #[tokio::test]
    async fn set_user_list_rename_onto_a_taken_login_changes_nothing() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        for (login, password) in [("bob", "bob's"), ("alice", "alice's")] {
            let account = Account {
                access_extra: AccessExtra::default(),
                password: password.into(),
                name: login.into(),
                access: Access::SEND_CHAT,
            };
            state.put_account(login.to_string(), account).await.unwrap();
        }

        let record = user_list_record(
            Some("bob"),
            "Bob",
            "alice",
            Field::bytes(FieldId::UserPassword, vec![0u8]),
            Access::all(),
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;

        assert!(replies[0].is_error());
        assert_eq!(&*state.account("bob").await.unwrap().password, "bob's");
        let alice = state.account("alice").await.unwrap();
        assert_eq!(&*alice.password, "alice's");
        assert_eq!(alice.access, Access::SEND_CHAT);
    }

    #[tokio::test]
    async fn set_user_list_delete_disconnects_the_online_session() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Bob".into(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();
        state
            .insert_user(UserEntry {
                user_id: 1,
                login: "bob".into(),
                name: "Bob".into(),
                icon: 0,
                client_version: None,
                access: Access::empty(),
                access_extra: AccessExtra::default(),
                account_name: "Bob".into(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let mut rx = state.register_recipient(1).await;

        let record = user_list_record(
            Some("bob"),
            "",
            "",
            Field::bytes(FieldId::UserPassword, vec![]),
            Access::empty(),
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());
        assert!(state.account("bob").await.is_none());

        let kick = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a DisconnectMsg push");
        assert_eq!(kick.r#type(), TransactionType::Push(Opcode::DisconnectMsg));
    }

    #[tokio::test]
    async fn set_user_list_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();

        let create = user_list_record(
            None,
            "Bob",
            "bob",
            Field::obfuscated_string(FieldId::UserPassword, "secret"),
            Access::empty(),
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![create]);
        assert!(session.on_set_user_list(&request, &state).await[0].is_error());
    }
}
