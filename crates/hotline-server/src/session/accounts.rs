//! Account CRUD and the bulk user-list admin opcodes.

use std::borrow::Cow;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::FieldId;
use hotline_proto::{Field, FieldList, Transaction};

use crate::account::{AccessExtra, Account};
use crate::state::SharedState;

use super::connection::Session;

impl Session {
    /// `myTran_NewUser` (350)
    pub(super) async fn on_account_create(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_USER) {
            return vec![tx.reply_error(reason)];
        }
        self.on_account_create_or_modify(tx, state).await
    }

    /// `myTran_DeleteUser` (351)
    pub(super) async fn on_account_delete(
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
        vec![tx.reply(vec![])]
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
                        Opcode::Politequit,
                        vec![Field::string(
                            FieldId::Text,
                            "You are logged in with an account which was deleted.",
                        )],
                    ),
                )
                .await;
        }
    }

    /// `myTran_GetUser` (352)
    pub(super) async fn on_account_read(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::OPEN_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.string(FieldId::Login, encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let Some(account) = state.account(&login).await else {
            return vec![tx.reply_error("no such account")];
        };
        vec![tx.reply(vec![
            Field::string(FieldId::Name, account.name),
            Field::obfuscated_string(FieldId::Login, login),
            Self::redacted_password_field(&account.password),
            Field::u64(FieldId::Access, account.access.bits()),
        ])]
    }

    /// `myTran_SetUser` (353)
    pub(super) async fn on_account_modify(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::MODIFY_USER) {
            return vec![tx.reply_error(reason)];
        }
        self.on_account_create_or_modify(tx, state).await
    }

    /// Implementation for
    /// `myTran_NewUser` (350) and
    /// `myTran_SetUser` (353).
    pub(super) async fn on_account_create_or_modify(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let encoding = self.encoding();
        let Some(login) = tx.login(encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let existing = state.account(&login).await;
        // Absent PASSWORD clears it; raw byte 0x00 means unchanged.
        let password = match tx.field(FieldId::Password) {
            Some(field) if field.as_bytes(encoding).as_ref() == [0u8] => existing
                .as_ref()
                .map(|a| a.password.clone())
                .unwrap_or_default(),
            Some(field) => field.as_deobfuscated_string(encoding),
            None => String::new(),
        };
        let name = tx
            .name(encoding)
            .or_else(|| existing.as_ref().map(|a| a.name.clone()))
            .unwrap_or_default();
        // No wire field for this; unset on create means the default.
        let access_extra = existing
            .as_ref()
            .map(|a| a.access_extra)
            .unwrap_or_else(AccessExtra::default);
        let access = tx
            .access()
            .or_else(|| existing.map(|a| a.access))
            .unwrap_or_else(Access::empty);

        let account = Account {
            password,
            name: name.clone(),
            access,
            access_extra,
        };

        let result = state.put_account(login.clone(), account).await;
        let reply = match result {
            Ok(()) => {
                let name = (!name.is_empty()).then_some(name);
                state.push_access_update(login, access, name).await;
                tx.reply(vec![])
            }
            Err(reason) => tx.reply_error(format!("could not save account ({reason})")),
        };
        vec![reply]
    }

    /// The password field as sent back for `myTran_GetUser` (352)
    /// and each record of `myTran_GetUserList` (348): empty if unset,
    /// a single `x` otherwise, so the real password is never echoed.
    pub(super) fn redacted_password_field(password: &str) -> Field {
        Field::string(
            FieldId::Password,
            if password.is_empty() { "" } else { "x" },
        )
    }

    /// `myTran_GetUserList` (348): every account at once, for the
    /// bulk account editor. Each account is one `Text` (101) field
    /// wrapping its own nested `Name`/`Login`/`Password`/`Access` field
    /// list, the same `count`-prefixed shape a transaction body uses.
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
                        Field::string(FieldId::Name, account.name),
                        Field::obfuscated_string(FieldId::Login, login),
                        Self::redacted_password_field(&account.password),
                        Field::u64(FieldId::Access, account.access.bits()),
                    ],
                    encoding,
                )
                .ok()
            })
            .map(|record| Field::bytes(FieldId::Text, record))
            .collect();
        vec![tx.reply(fields)]
    }

    /// `myTran_SetUserList` (349): save a bulk-edited account list.
    /// Each `Text` (101) field is one nested account record in the same
    /// shape `myTran_GetUserList` sent; see
    /// [`Session::apply_user_list_record`] for the per-record rules.
    /// Processes every record even after a soft (storage) failure, but
    /// stops and reports immediately on a hard (permission) failure,
    /// matching openline.
    pub(super) async fn on_set_user_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let encoding = self.encoding();
        let mut save_error = false;
        for field in tx.fields(FieldId::Text) {
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

    /// Apply one `myTran_SetUserList` record: `Text` (101), if
    /// present, is the account's login before this edit (deobfuscated,
    /// scrambled the same way as `Login`), absent for a brand new record.
    /// An empty name and login means delete; otherwise this is a rename
    /// (login changed), a plain modify, or a create, depending on whether
    /// the old login was given and whether the new login already exists.
    ///
    /// `Ok(true)` if applied, `Ok(false)` for a soft (storage) failure
    /// that shouldn't stop the rest of the list from applying, `Err` for
    /// a hard (permission) failure that aborts the whole request.
    pub(super) async fn apply_user_list_record(
        &self,
        record: &FieldList,
        state: &SharedState,
    ) -> std::result::Result<bool, Cow<'static, str>> {
        let encoding = self.encoding();
        let as_deobfuscated_string = |f: &Field| f.as_deobfuscated_string(encoding);
        let old_login = record
            .field(FieldId::Text)
            .map(as_deobfuscated_string)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        let name = record.string(FieldId::Name, encoding).unwrap_or_default();
        let login = record
            .field(FieldId::Login)
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
                    Ok(true)
                }
                Err(_) => Ok(false),
            };
        }

        let password = match record.field(FieldId::Password) {
            Some(f) if f.as_bytes(encoding).as_ref() == [0u8] => {
                let Some(old_login) = &old_login else {
                    return Ok(false);
                };
                match state.account(old_login).await {
                    Some(existing) => existing.password,
                    None => return Ok(false),
                }
            }
            Some(f) => as_deobfuscated_string(f),
            None => String::new(),
        };
        let access = record
            .field(FieldId::Access)
            .and_then(Field::as_u64)
            .map(Access::from_bits_truncate)
            .unwrap_or_else(Access::empty);

        if let Some(old_login) = old_login {
            // Modify (and rename, if the login changed).
            self.access.require(Access::MODIFY_USER)?;
            let existing = state.account(&old_login).await;
            if !old_login.eq_ignore_ascii_case(&login) {
                let _ = state.delete_account(&old_login).await;
            }
            let access_extra = existing
                .map(|a| a.access_extra)
                .unwrap_or_else(AccessExtra::default);
            let account = Account {
                password,
                name: name.clone(),
                access,
                access_extra,
            };
            return match state.put_account(login.clone(), account).await {
                Ok(()) => {
                    let name = (!name.is_empty()).then_some(name);
                    state.push_access_update(login, access, name).await;
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
                            FieldId::Text,
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
        let account = Account {
            password,
            name,
            access,
            access_extra: AccessExtra::default(),
        };
        match state.put_account(login, account).await {
            Ok(()) => Ok(true),
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{RecipientMessage, UserEntry};
    use hotline_proto::opcode::TransactionType;
    use std::time::Instant;

    use crate::session::test_support::*;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn account_create_then_read_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            Opcode::AccountCreate,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::obfuscated_string(FieldId::Password, "secret"),
                Field::string(FieldId::Name, "Bob"),
                Field::u64(FieldId::Access, Access::SEND_CHAT.bits()),
            ],
        );
        let replies = session.on_account_create(&create, &state).await;
        assert!(!replies[0].is_error());

        let read = Transaction::request(
            Opcode::AccountRead,
            2,
            vec![Field::obfuscated_string(FieldId::Login, "bob")],
        );
        let replies = session.on_account_read(&read, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(replies[0].string(FieldId::Name, encoding).unwrap(), "Bob");
        assert_eq!(
            replies[0].field(FieldId::Access).and_then(Field::as_u64),
            Some(Access::SEND_CHAT.bits())
        );
    }

    #[tokio::test]
    async fn account_create_folds_the_login_to_lowercase() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            Opcode::AccountCreate,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "Bob"),
                Field::obfuscated_string(FieldId::Password, "secret"),
                Field::string(FieldId::Name, "Bob"),
                Field::u64(FieldId::Access, Access::SEND_CHAT.bits()),
            ],
        );
        assert!(!session.on_account_create(&create, &state).await[0].is_error());

        assert!(state.account("bob").await.is_some());
        assert!(state.account("Bob").await.is_none());
    }

    #[tokio::test]
    async fn account_delete_matches_the_login_regardless_of_case() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let delete = Transaction::request(
            Opcode::AccountDelete,
            1,
            vec![Field::obfuscated_string(FieldId::Login, "BOB")],
        );
        assert!(!session.on_account_delete(&delete, &state).await[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn account_modify_only_overwrites_fields_present() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        // Password unchanged via the sentinel, not by omitting the field.
        let modify = Transaction::request(
            Opcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::string(FieldId::Name, "Bobby"),
                Field::bytes(FieldId::Password, vec![0]),
            ],
        );
        session.on_account_modify(&modify, &state).await;

        let account = state.account("bob").await.unwrap();
        assert_eq!(account.name, "Bobby");
        assert_eq!(account.password, "secret");
        assert_eq!(account.access, Access::SEND_CHAT);
    }

    #[tokio::test]
    async fn account_modify_without_a_password_field_at_all_clears_it() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        // No PASSWORD field at all, not the "unchanged" sentinel: this
        // clears the password, matching openline's own behavior.
        let modify = Transaction::request(
            Opcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::string(FieldId::Name, "Bobby"),
            ],
        );
        session.on_account_modify(&modify, &state).await;

        let account = state.account("bob").await.unwrap();
        assert_eq!(account.password, "");
    }

    #[tokio::test]
    async fn account_read_never_echoes_the_real_password() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
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
                    password: String::new(),
                    name: "Carol".to_string(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let read = |login: &str| {
            Transaction::request(
                Opcode::AccountRead,
                1,
                vec![Field::obfuscated_string(FieldId::Login, login)],
            )
        };

        let [with_password] = session
            .on_account_read(&read("bob"), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            with_password
                .field(FieldId::Password)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b"x"
        );

        let [without_password] = session
            .on_account_read(&read("carol"), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            without_password
                .field(FieldId::Password)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b""
        );
    }

    #[tokio::test]
    async fn account_modify_propagates_to_a_live_session_without_reconnect() {
        let admin = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Bob".to_string(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        // Bob is already connected, logged in under the "bob" account.
        let mut bob = test_session("Bob", Access::empty());
        bob.login = "bob".to_string();
        state
            .insert_user(UserEntry {
                user_id: bob.user_id,
                login: bob.login.clone(),
                name: bob.name.clone(),
                icon: 0,
                client_version: None,
                access: bob.access,
                access_extra: bob.access_extra,
                account_name: String::new(),
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
            Opcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::u64(FieldId::Access, Access::BROADCAST.bits()),
            ],
        );
        admin.on_account_modify(&modify, &state).await;

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
            notify.field(FieldId::UserId).and_then(Field::as_u16),
            Some(bob.user_id)
        );
    }

    #[tokio::test]
    async fn account_modify_renames_only_when_use_any_name_is_revoked() {
        let admin = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Bob".to_string(),
                    access: Access::ANY_NAME,
                },
            )
            .await
            .unwrap();

        // Bob is connected with USE_ANY_NAME and has picked a custom name.
        let mut bob = test_session("Bobby the Great", Access::ANY_NAME);
        bob.login = "bob".to_string();
        state
            .insert_user(UserEntry {
                user_id: bob.user_id,
                login: bob.login.clone(),
                name: bob.name.clone(),
                icon: 0,
                client_version: None,
                access: bob.access,
                access_extra: bob.access_extra,
                account_name: String::new(),
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
            Opcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::u64(FieldId::Access, Access::empty().bits()),
            ],
        );
        admin.on_account_modify(&revoke, &state).await;
        let update = next_access_update(&mut bob_rx);
        bob.apply_access_update(update, &state).await;
        assert_eq!(bob.name, "Bob");

        // Still holds USE_ANY_NAME, so this modify must not touch the name.
        bob.name = "Bobby Returns".to_string();
        bob.access = Access::ANY_NAME;
        let regrant = Transaction::request(
            Opcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "bob"),
                Field::u64(
                    FieldId::Access,
                    (Access::ANY_NAME | Access::SEND_CHAT).bits(),
                ),
            ],
        );
        admin.on_account_modify(&regrant, &state).await;
        let update = next_access_update(&mut bob_rx);
        bob.apply_access_update(update, &state).await;
        assert_eq!(bob.name, "Bobby Returns");
    }

    #[tokio::test]
    async fn account_delete_removes_it() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();
        state
            .put_account(
                "bob".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Bob".to_string(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        let delete = Transaction::request(
            Opcode::AccountDelete,
            1,
            vec![Field::obfuscated_string(FieldId::Login, "bob")],
        );
        let replies = session.on_account_delete(&delete, &state).await;
        assert!(!replies[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn account_crud_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let login_field = Field::obfuscated_string(FieldId::Login, "bob");

        let create = Transaction::request(Opcode::AccountCreate, 1, vec![login_field.clone()]);
        assert!(session.on_account_create(&create, &state).await[0].is_error());

        let read = Transaction::request(Opcode::AccountRead, 2, vec![login_field.clone()]);
        assert!(session.on_account_read(&read, &state).await[0].is_error());

        let modify = Transaction::request(Opcode::AccountModify, 3, vec![login_field.clone()]);
        assert!(session.on_account_modify(&modify, &state).await[0].is_error());

        let delete = Transaction::request(Opcode::AccountDelete, 4, vec![login_field]);
        assert!(session.on_account_delete(&delete, &state).await[0].is_error());
    }

    /// One `myTran_SetUserList` record: `old_login` is the account's
    /// login before this edit, `None` for a brand new record.
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
            fields.push(Field::obfuscated_string(FieldId::Text, old_login));
        }
        if !name.is_empty() {
            fields.push(Field::string(FieldId::Name, name));
        }
        if !login.is_empty() {
            fields.push(Field::obfuscated_string(FieldId::Login, login));
        }
        fields.push(password);
        fields.push(Field::u64(FieldId::Access, access.bits()));
        Field::bytes(FieldId::Text, FieldList::encode(&fields, encoding).unwrap())
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
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
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
        let records: Vec<_> = reply.fields(FieldId::Text).collect();
        assert_eq!(records.len(), 3);

        let bob = records
            .iter()
            .map(|f| FieldList::decode(&f.as_bytes(encoding)).unwrap())
            .find(|fields| fields.string(FieldId::Name, encoding).as_deref() == Some("Bob"))
            .expect("bob's record");
        let login = bob
            .field(FieldId::Login)
            .unwrap()
            .as_deobfuscated_string(encoding);
        assert_eq!(login, "bob");
        let password = bob.field(FieldId::Password).unwrap();
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
            Field::obfuscated_string(FieldId::Password, "secret"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());

        let account = state.account("bob").await.unwrap();
        assert_eq!(account.name, "Bob");
        assert_eq!(account.password, "secret");
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
            Field::obfuscated_string(FieldId::Password, "secret"),
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
                    password: "original".to_string(),
                    name: "Bob".to_string(),
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
            Field::obfuscated_string(FieldId::Password, "new"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());
        // Untouched: the collision is a notice, not an applied overwrite.
        assert_eq!(state.account("bob").await.unwrap().password, "original");
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
                    password: "secret".to_string(),
                    name: "Bob".to_string(),
                    access: Access::SEND_CHAT,
                },
            )
            .await
            .unwrap();

        let record = user_list_record(
            Some("bob"),
            "Bobby",
            "bobby",
            Field::bytes(FieldId::Password, vec![0u8]),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());

        assert!(state.account("bob").await.is_none());
        let renamed = state.account("bobby").await.unwrap();
        assert_eq!(renamed.name, "Bobby");
        assert_eq!(renamed.password, "secret");
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
                    password: String::new(),
                    name: "Bob".to_string(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();
        state
            .insert_user(UserEntry {
                user_id: 1,
                login: "bob".to_string(),
                name: "Bob".to_string(),
                icon: 0,
                client_version: None,
                access: Access::empty(),
                access_extra: AccessExtra::default(),
                account_name: "Bob".to_string(),
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
            Field::bytes(FieldId::Password, vec![]),
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
            .expect("expected a Politequit push");
        assert_eq!(kick.r#type(), TransactionType::Push(Opcode::Politequit));
    }

    #[tokio::test]
    async fn set_user_list_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();

        let create = user_list_record(
            None,
            "Bob",
            "bob",
            Field::obfuscated_string(FieldId::Password, "secret"),
            Access::empty(),
        );
        let request = Transaction::request(Opcode::SetUserList, 1, vec![create]);
        assert!(session.on_set_user_list(&request, &state).await[0].is_error());
    }
}
