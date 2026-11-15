//! Handlers that act on another connected user: listing, info,
//! kick, private messages, and broadcast.

use std::borrow::Cow;

use hotline_proto::FieldId;
use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::{Field, Received, Transaction};

use crate::account::AccessExtra;
use crate::ban::BanRecord;
use crate::flood::FloodEvent;
use crate::policy::Silent;
use crate::state::{SharedState, UserEntry};

use super::connection::{KickBan, MsgOption, Session};

impl Session {
    /// [`myTran_GetUserNameList`] (300): every user with a name.
    ///
    /// [`myTran_GetUserNameList`]: hotline_proto::Opcode::GetUserNameList
    pub(super) async fn on_get_user_name_list(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::USER_GETLIST) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let fields = state
            .users()
            .await
            .into_iter()
            .filter(|u| !u.name.is_empty())
            .map(|u| UserNameWithInfo {
                user_id: u.user_id,
                icon: u.icon,
                flags: u.flags(),
                name: u.name,
            })
            .map(|entry| Field::bytes(FieldId::UserNameWithInfo, entry.encode(encoding)))
            .collect();
        vec![tx.reply(fields)]
    }

    /// [`myTran_GetClientInfoText`] (303): renders as a single scrollable text
    /// block in the client's "Get Info" window, so its shape follows openline's
    /// own labels and section order rather than this server's own report
    /// design.
    ///
    /// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
    pub(super) async fn on_get_client_info_text(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::GET_CLIENT_INFO) {
            return vec![tx.reply_error(reason)];
        }
        let target_id = tx.field(FieldId::UserID).map(Field::as_u16).unwrap_or(0);
        let Some(target) = state.user(target_id).await else {
            return vec![tx.reply_error(
                "Cannot get info for the specified client because it does not exist.",
            )];
        };

        let transfers = state.transfer_report(target_id).await;
        let text = state.policy().client_info_text(&target, &transfers);

        vec![tx.reply(vec![
            Field::string(FieldId::Data, text),
            Field::string(FieldId::UserName, target.name),
        ])]
    }

    /// [`myTran_DisconnectUser`] (110): disconnect another user, and if the
    /// [`myField_Options`] (113) kick-to-ban bit is set, ban their address and
    /// notify them why before disconnecting them. A permanent ban keeps
    /// [`myField_Data`] (101), cut to [`TextLimits::ban_note`], as its
    /// note. A missing target id defaults to 0, which no session holds, so
    /// the kick is just a no-op.
    ///
    /// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    /// [`TextLimits::ban_note`]: crate::TextLimits::ban_note
    pub(super) async fn on_disconnect_user(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DISCON_USER) {
            return vec![tx.reply_error(reason)];
        }
        let target_id = tx.field(FieldId::UserID).map(Field::as_u16).unwrap_or(0);
        if let Err(reason) = state.authorize_kick(target_id).await {
            return vec![tx.reply_error(reason)];
        }
        let ban = KickBan::from(tx.field(FieldId::Options).map(Field::as_u16).unwrap_or(0));
        let target_ip = state.user(target_id).await.map(|u| u.remote_addr.ip());

        let notice = match (ban, target_ip) {
            (KickBan::Temporary, Some(ip)) => {
                state.temp_ban(ip).await;
                Some("You are temporarily banned on this server.")
            }
            (KickBan::Permanent, Some(ip)) => {
                let note = tx.string(FieldId::Data).unwrap_or_default();
                let record = BanRecord {
                    note: self
                        .cut_text(state.policy().text_limits().ban_note, &note)
                        .to_owned(),
                    ..BanRecord::new(ip, None)
                };
                if let Err(error) = state.ban(record).await {
                    tracing::warn!(%ip, %error, "failed to persist ban");
                }
                Some("You are permanently banned on this server.")
            }
            (KickBan::None, _) | (_, None) => None,
        };

        match notice {
            Some(reason) => state.kick(target_id, reason).await,
            None => state.disconnect(target_id).await,
        }

        vec![tx.reply(vec![])]
    }

    /// The [`myField_Data`] (101) text of a private message or broadcast.
    /// A missing field is empty text, unless the policy sends an error.
    ///
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    fn message_data(
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Result<String, Cow<'static, str>> {
        match tx.string(FieldId::Data) {
            Some(data) => Ok(data),
            None => state
                .policy()
                .error(Silent::MessageWithoutData)
                .map_or(Ok(String::new()), Err),
        }
    }

    /// The repeat and message count checks for a private message or broadcast.
    async fn message_flood_banned(&self, tx: &Transaction<Received>, state: &SharedState) -> bool {
        self.flood_banned(FloodEvent::Text(self.data_bytes(tx)), state)
            .await
            || self.flood_banned(FloodEvent::Message, state).await
    }

    /// [`myTran_SendInstantMsg`] (108): a private instant message to one other
    /// user. Options 2 and 3 send the sender's refusal text in place of the
    /// message. The policy cuts the message and its quote to
    /// [`TextLimits::message`], and an empty message is not sent.
    ///
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    /// [`TextLimits::message`]: crate::TextLimits::message
    pub(super) async fn on_send_instant_msg(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if self.message_flood_banned(tx, state).await {
            return vec![];
        }
        if let Err(reason) = self.access.require(Access::SEND_MESSAGE)
            && !self.access_extra.contains(AccessExtra::MSG_OVERRIDE)
        {
            return vec![tx.reply_error(reason)];
        }
        let target_id = tx.field(FieldId::UserID).map(Field::as_u16).unwrap_or(0);
        let message = match Self::message_data(tx, state) {
            Ok(message) => message,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let options = tx.field(FieldId::Options).map(Field::as_u16).unwrap_or(0);
        let option = MsgOption::from(options);
        let is_user_message = option == MsgOption::UserMessage;
        let message = match option {
            MsgOption::RefuseMessage => format!("{} does not accept private messages.", self.name),
            MsgOption::RefuseChat => format!("{} does not accept private chats.", self.name),
            _ => message,
        };
        let limits = state.policy().text_limits();
        let message = self.cut_text(limits.message, &message);

        let target = match state.require_connected_user(target_id).await {
            Ok(target) => target,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let mut replies = Vec::new();
        if is_user_message {
            replies.extend(self.refusal_bounce(target_id, &target, false));
        }
        if !target.refuses_pm && !message.is_empty() {
            let mut fields = vec![
                Field::u16(FieldId::UserID, self.user_id),
                Field::string(FieldId::UserName, self.name.clone()),
                Field::u16(FieldId::Options, options),
                Field::string(FieldId::Data, message),
            ];
            if let Some(quote) = tx
                .string(FieldId::QuotingMsg)
                .filter(|quote| !quote.is_empty())
            {
                let quote = self.cut_text(limits.message, &quote);
                fields.push(Field::string(FieldId::QuotingMsg, quote));
            }
            state
                .send_to(target_id, Transaction::push(Opcode::ServerMsg, fields))
                .await;
        }
        replies.push(tx.reply(vec![]));
        replies
    }

    /// Builds the bounce sent back to this session when `target` refuses
    /// private messages/chat invites or has an automatic response
    /// configured, or `None` when delivery should proceed normally. Gated
    /// on this session's own PM-refusal setting for both message and
    /// chat-invite bounces, matching openline's own reuse of that
    /// one flag.
    pub(super) fn refusal_bounce(
        &self,
        target_id: u16,
        target: &UserEntry,
        is_chat: bool,
    ) -> Option<Transaction> {
        let refuses = if is_chat {
            target.refuses_chat
        } else {
            target.refuses_pm
        };
        if self.refuses_pm || !(target.automatic_response.is_some() || refuses) {
            return None;
        }
        let (option, text) = match &target.automatic_response {
            Some(response) => (MsgOption::AutomaticResponse, response.clone()),
            None if is_chat => (
                MsgOption::RefuseChat,
                format!("{} does not accept private chats.", target.name).into(),
            ),
            None => (
                MsgOption::RefuseMessage,
                format!("{} does not accept private messages.", target.name).into(),
            ),
        };
        Some(Transaction::push(
            Opcode::ServerMsg,
            vec![
                Field::u16(FieldId::UserID, target_id),
                Field::string(FieldId::UserName, target.name.clone()),
                Field::u16(FieldId::Options, option.into()),
                Field::string(FieldId::Data, text),
            ],
        ))
    }

    /// [`myTran_UserBroadcast`] (355): a message broadcast to every connected
    /// user, cut to [`TextLimits::message`]. An empty message is not sent.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    /// [`TextLimits::message`]: crate::TextLimits::message
    pub(super) async fn on_user_broadcast(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::BROADCAST) {
            return vec![tx.reply_error(reason)];
        }
        if self.message_flood_banned(tx, state).await {
            return vec![tx.reply(vec![])];
        }
        let message = match Self::message_data(tx, state) {
            Ok(message) => message,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let message = self.cut_text(state.policy().text_limits().message, &message);
        if message.is_empty() {
            return vec![tx.reply(vec![])];
        }

        state
            .announce(
                self.user_id,
                Transaction::push(
                    Opcode::UserBroadcast,
                    vec![
                        Field::u16(FieldId::UserID, self.user_id),
                        Field::string(FieldId::Data, message),
                        Field::string(FieldId::UserName, self.name.clone()),
                    ],
                ),
            )
            .await;
        vec![tx.reply(vec![])]
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::server::ServerConfig;
    use crate::session::test_support::*;
    use crate::state::RecipientMessage;
    use hotline_proto::opcode::TransactionType;
    use hotline_proto::text_encoding;
    use std::time::Instant;

    #[tokio::test]
    async fn get_user_name_list_denied_without_user_getlist() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::USER_GETLIST);
        let state = Arc::<SharedState>::default();
        let request = request(Opcode::GetUserNameList, 1, vec![]);

        assert!(session.on_get_user_name_list(&request, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn get_user_name_list_skips_a_user_with_an_empty_name() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        for (user_id, name) in [(2, ""), (3, "Carol")] {
            state
                .insert_user(UserEntry {
                    user_id,
                    login: "guest".into(),
                    account_name: Box::default(),
                    name: name.into(),
                    icon: 0,
                    client_version: None,
                    access: Access::empty(),
                    access_extra: AccessExtra::default(),
                    remote_addr: "192.0.2.1:5500".parse().unwrap(),
                    connected_at: Instant::now(),
                    last_active: Instant::now(),
                    refuses_pm: false,
                    refuses_chat: false,
                    automatic_response: None,
                })
                .await;
        }
        let request = request(Opcode::GetUserNameList, 1, vec![]);

        let reply = &session.on_get_user_name_list(&request, &state).await[0];
        let encoding = text_encoding::DEFAULT;
        let ids: Vec<u16> = reply
            .received(encoding)
            .fields(FieldId::UserNameWithInfo)
            .map(|f| {
                UserNameWithInfo::decode(f.as_bytes(), encoding)
                    .unwrap()
                    .user_id
            })
            .collect();
        assert_eq!(ids, [3]);
    }

    #[tokio::test]
    async fn get_user_name_list_encodes_entries_using_the_sessions_active_encoding() {
        let session = test_session("Alice", Access::all());
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        session.encoding.set(sjis);
        let state = Arc::<SharedState>::default();
        let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"; // "こんにちは"
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: "bob".into(),
                account_name: konnichiwa.into(),
                name: konnichiwa.into(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                remote_addr: "192.0.2.1:5500".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let request = request(Opcode::GetUserNameList, 1, vec![]);

        let reply = &session.on_get_user_name_list(&request, &state).await[0];
        let entry = reply
            .received(sjis)
            .field(FieldId::UserNameWithInfo)
            .map(|f| UserNameWithInfo::decode(f.as_bytes(), sjis).unwrap())
            .expect("expected bob's user list entry");
        assert_eq!(&*entry.name, konnichiwa);
    }

    #[tokio::test]
    async fn get_client_info_text_without_a_user_id_finds_no_client() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();

        let request = request(Opcode::GetClientInfoText, 1, vec![]);
        let replies = session.on_get_client_info_text(&request, &state).await;

        assert_eq!(
            error_text(&replies[0]),
            "Cannot get info for the specified client because it does not exist."
        );
    }

    #[tokio::test]
    async fn get_client_info_text_sends_the_basic_lines_by_default() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: Box::default(),
                account_name: Box::default(),
                name: "Bob".into(),
                icon: 42,
                client_version: Some(185),
                access: Access::empty(),
                access_extra: AccessExtra::default(),
                remote_addr: "192.0.2.1:5500".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let request = request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        let text = reply.received(encoding).string(FieldId::Data).unwrap();
        assert!(text.starts_with(
            "Nickname:   Bob\r\
             Name:       \r\
             Account:    \r\
             Address:    192.0.2.1\r\r\
             -------- File Downloads ---------\r"
        ));
    }

    #[tokio::test]
    async fn get_client_info_text_names_the_user_and_lists_no_transfers() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: "bob".into(),
                account_name: "Bobby".into(),
                name: "Bob".into(),
                icon: 42,
                client_version: Some(185),
                access: Access::all(),
                access_extra: AccessExtra::default(),
                remote_addr: "192.0.2.1:5500".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let request = request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        assert_eq!(
            reply.received(encoding).string(FieldId::UserName).unwrap(),
            "Bob"
        );
        assert!(reply.field(FieldId::UserID).is_none());
        let text = reply.received(encoding).string(FieldId::Data).unwrap();
        assert!(text.contains(
            "-------- File Downloads ---------\r\rNone.\r\r\
             ------- Folder Downloads --------\r\rNone.\r\r\
             --------- File Uploads ----------\r\rNone.\r\r\
             -------- Folder Uploads ---------\r\rNone.\r\r\
             ------- Waiting Downloads -------\r\rNone.\r"
        ));
    }

    async fn file_transfer(
        state: &SharedState,
        name: &str,
        direction: crate::state::TransferDirection,
        size: u64,
    ) -> crate::state::PendingTransfer {
        crate::state::PendingTransfer {
            kind: crate::state::TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "bob".into(),
                    store: state.files_for("bob").await.unwrap(),
                    dir: Default::default(),
                    name: name.into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
            }),
            direction,
            size,
            resume_offset: 0,
            client_ip: "192.0.2.1".parse().unwrap(),
            user_id: 2,
        }
    }

    #[tokio::test]
    async fn get_client_info_text_lists_uploads_and_waiting_downloads() {
        use crate::state::TransferDirection::{Get, Put};
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: "bob".into(),
                account_name: "Bobby".into(),
                name: "Bob".into(),
                icon: 42,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                remote_addr: "192.0.2.1:5500".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        state.set_download_limits(1, 1);
        state
            .register_transfer(file_transfer(&state, "up.txt", Put, 2048).await)
            .await;
        for name in ["first.txt", "second.txt"] {
            let transfer = file_transfer(&state, name, Get, 10).await;
            let reference = state.register_transfer(transfer).await;
            state.admit_download(reference, 2).await;
        }
        let request = request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        let text = reply.received(encoding).string(FieldId::Data).unwrap();
        assert!(text.contains(
            "-------- File Downloads ---------\r\rfirst.txt             0%   <1K\r\r\
             ------- Folder Downloads --------\r\rNone.\r\r\
             --------- File Uploads ----------\r\rup.txt                0%   2K\r\r\
             -------- Folder Uploads ---------\r\rNone.\r\r\
             ------- Waiting Downloads -------\r\rsecond.txt            (File)\r"
        ));
    }

    #[tokio::test]
    async fn kick_denied_without_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::DisconnectUser,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn kick_disconnects_target_with_no_wire_notice() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(2).await;
        let request = request(
            Opcode::DisconnectUser,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());

        assert!(target_rx.is_ended());
        assert!(
            target_rx.try_recv().is_err(),
            "plain kick must send no wire notice"
        );
    }

    #[tokio::test]
    async fn disconnect_user_names_a_protected_user() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state.insert_user(test_user(2, "Bob")).await;

        let request = request(
            Opcode::DisconnectUser,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let replies = session.on_disconnect_user(&request, &state).await;

        assert_eq!(
            error_text(&replies[0]),
            "You are not allowed to disconnect user “Bob” (user is protected)."
        );
    }

    #[tokio::test]
    async fn kick_with_permanent_ban_option_bans_the_target_address() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target = test_user(2, "Bob");
        target.access.remove(Access::CANNOT_BE_DISCON);
        target.remote_addr = "192.0.2.9:5500".parse().unwrap();
        state.insert_user(target).await;
        let mut target_rx = state.register_recipient(2).await;

        let request = request(
            Opcode::DisconnectUser,
            1,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::Options, 2),
                Field::string(FieldId::Data, "x".repeat(70)),
            ],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert!(!replies[0].is_error());

        let ban = state
            .is_banned("192.0.2.9".parse().unwrap())
            .await
            .expect("expected a permanent ban");
        assert_eq!(
            ban.note,
            "x".repeat(state.policy().text_limits().ban_note.unwrap())
        );

        let kick = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_kick);
        assert_eq!(
            kick.as_deref(),
            Some("You are permanently banned on this server.")
        );
    }

    #[tokio::test]
    async fn kick_with_temporary_ban_option_sends_the_temporary_ban_notice() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target = test_user(2, "Bob");
        target.access.remove(Access::CANNOT_BE_DISCON);
        target.remote_addr = "192.0.2.9:5500".parse().unwrap();
        state.insert_user(target).await;
        let mut target_rx = state.register_recipient(2).await;

        let request = request(
            Opcode::DisconnectUser,
            1,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::Options, 1),
            ],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert!(!replies[0].is_error());

        let ban = state
            .is_banned("192.0.2.9".parse().unwrap())
            .await
            .expect("expected a temporary ban");
        assert!(ban.until.is_some());

        let kick = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_kick);
        assert_eq!(
            kick.as_deref(),
            Some("You are temporarily banned on this server.")
        );
    }

    #[tokio::test]
    async fn kick_with_missing_uid_replies_success() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let request = request(Opcode::DisconnectUser, 1, vec![]);
        let replies = session.on_disconnect_user(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
    }

    #[tokio::test]
    async fn the_122nd_transaction_in_a_minute_bans_and_disconnects() {
        let state = state_with_flood();
        let mut session = test_session("Alice", Access::all());
        let mut rx = state.register_recipient(session.user_id).await;
        let ping = || request(Opcode::KeepConnectionAlive, 1, vec![]);
        let config = ServerConfig::default();

        for _ in 0..121 {
            assert_eq!(session.handle(&ping(), &state, &config).await.len(), 1);
        }
        let replies = session.handle(&ping(), &state, &config).await;

        assert!(replies.is_empty());
        assert!(rx.is_ended());
        assert!(rx.try_recv().is_err(), "a flood ban sends no wire notice");
        assert!(state.is_banned(session.remote_addr.ip()).await.is_some());
    }

    #[tokio::test]
    async fn can_spam_exempts_a_session_from_the_flood_policy() {
        let state = state_with_flood();
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::CAN_SPAM);
        let mut rx = state.register_recipient(session.user_id).await;
        let config = ServerConfig::default();

        for _ in 0..200 {
            let replies = session
                .handle(
                    &request(Opcode::KeepConnectionAlive, 1, vec![]),
                    &state,
                    &config,
                )
                .await;
            assert_eq!(replies.len(), 1);
        }
        assert!(
            rx.try_recv().is_err(),
            "exempt session was never disconnected"
        );
    }

    #[tokio::test]
    async fn the_sixth_identical_private_message_bans() {
        let state = state_with_flood();
        let session = test_session("Alice", Access::all());
        let rx = state.register_recipient(session.user_id).await;
        let message = || {
            request(
                Opcode::SendInstantMsg,
                1,
                vec![
                    Field::u16(FieldId::UserID, 2),
                    Field::string(FieldId::Data, "buy now"),
                ],
            )
        };

        for _ in 0..5 {
            assert!(
                !session
                    .on_send_instant_msg(&message(), &state)
                    .await
                    .is_empty()
            );
        }

        assert!(
            session
                .on_send_instant_msg(&message(), &state)
                .await
                .is_empty()
        );
        assert!(rx.is_ended());
    }

    #[tokio::test]
    async fn user_broadcast_still_replies_to_the_broadcast_that_bans() {
        let state = state_with_flood();
        let session = test_session("Alice", Access::all());
        let broadcast = request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, "buy now")],
        );
        for _ in 0..5 {
            session.on_user_broadcast(&broadcast, &state).await;
        }
        let rx = state.register_recipient(session.user_id).await;

        let replies = session.on_user_broadcast(&broadcast, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
        assert!(rx.is_ended());
    }

    #[tokio::test]
    async fn private_message_delivered_only_to_target() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;
        let mut sender_rx = state.register_recipient(session.user_id).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;
        assert!(!replies[0].is_error());

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Msg push");
        assert_eq!(
            delivered.received(encoding).string(FieldId::Data).unwrap(),
            "hi there"
        );
        assert!(
            sender_rx.try_recv().is_err(),
            "must not also deliver to the sender"
        );
    }

    #[tokio::test]
    async fn private_message_echoes_options_onto_the_delivered_push() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::u16(FieldId::Options, MsgOption::UserMessage.into()),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;
        assert!(!replies[0].is_error());

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Msg push");
        assert_eq!(
            delivered
                .received(text_encoding::DEFAULT)
                .field(FieldId::Options)
                .unwrap()
                .as_u16(),
            u16::from(MsgOption::UserMessage)
        );
    }

    #[tokio::test]
    async fn private_message_bounces_and_is_not_delivered_when_target_refuses() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                refuses_pm: true,
                ..test_user(42, "Bob")
            })
            .await;
        let mut target_rx = state.register_recipient(42).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::u16(FieldId::Options, MsgOption::UserMessage.into()),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].received(encoding).string(FieldId::Data).unwrap(),
            "Bob does not accept private messages."
        );
        assert_eq!(
            replies[0]
                .received(encoding)
                .field(FieldId::Options)
                .unwrap()
                .as_u16(),
            u16::from(MsgOption::RefuseMessage)
        );
        assert!(!replies[1].is_error());
        assert!(
            target_rx.try_recv().is_err(),
            "a refusing target must not receive the message"
        );
    }

    #[tokio::test]
    async fn private_message_delivers_and_bounces_an_automatic_response() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                automatic_response: Some("At lunch, back soon.".into()),
                ..test_user(42, "Bob")
            })
            .await;
        let mut target_rx = state.register_recipient(42).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::u16(FieldId::Options, MsgOption::UserMessage.into()),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].received(encoding).string(FieldId::Data).unwrap(),
            "At lunch, back soon."
        );
        assert_eq!(
            replies[0]
                .received(encoding)
                .field(FieldId::Options)
                .unwrap()
                .as_u16(),
            u16::from(MsgOption::AutomaticResponse)
        );
        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("automatic response still lets the message through");
        assert_eq!(
            delivered.received(encoding).string(FieldId::Data).unwrap(),
            "hi there"
        );
    }

    #[tokio::test]
    async fn private_message_no_bounce_when_sender_also_refuses() {
        let mut session = test_session("Alice", Access::all());
        session.refuses_pm = true;
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                refuses_pm: true,
                ..test_user(42, "Bob")
            })
            .await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::u16(FieldId::Options, MsgOption::UserMessage.into()),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
    }

    #[tokio::test]
    async fn send_instant_msg_relays_the_quote_in_the_recipients_encoding() {
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        let utf8 = hotline_proto::Encoding::for_label(b"utf-8").unwrap();
        let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}";
        let session = test_session("Alice", Access::all());
        session.encoding.set(sjis);
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;

        let request = Transaction::request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi"),
                Field::string(FieldId::QuotingMsg, konnichiwa),
            ],
        )
        .received(sjis);
        session.on_send_instant_msg(&request, &state).await;

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ServerMsg push");
        assert_eq!(
            delivered
                .received(utf8)
                .string(FieldId::QuotingMsg)
                .as_deref(),
            Some(konnichiwa)
        );
    }

    #[tokio::test]
    async fn send_instant_msg_with_an_empty_quote_sends_no_quote() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi"),
                Field::string(FieldId::QuotingMsg, ""),
            ],
        );
        session.on_send_instant_msg(&request, &state).await;

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ServerMsg push");
        assert!(delivered.field(FieldId::QuotingMsg).is_none());
    }

    #[tokio::test]
    async fn send_instant_msg_with_a_refusal_option_sends_the_refusal_text() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;
        let send = |option| {
            request(
                Opcode::SendInstantMsg,
                1,
                vec![
                    Field::u16(FieldId::UserID, 42),
                    Field::u16(FieldId::Options, option),
                    Field::string(FieldId::Data, "ignored"),
                ],
            )
        };

        for (option, text) in [
            (2, "Alice does not accept private messages."),
            (3, "Alice does not accept private chats."),
        ] {
            session.on_send_instant_msg(&send(option), &state).await;
            let delivered = target_rx
                .try_recv()
                .ok()
                .and_then(RecipientMessage::into_push)
                .expect("expected a ServerMsg push");
            assert_eq!(
                delivered
                    .received(encoding)
                    .string(FieldId::Data)
                    .as_deref(),
                Some(text)
            );
        }
    }

    #[tokio::test]
    async fn send_instant_msg_cuts_the_message_and_quote_and_skips_an_empty_one() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;
        let send = |data: String| {
            request(
                Opcode::SendInstantMsg,
                1,
                vec![
                    Field::u16(FieldId::UserID, 42),
                    Field::string(FieldId::Data, data),
                    Field::string(FieldId::QuotingMsg, "q".repeat(5000)),
                ],
            )
        };

        session
            .on_send_instant_msg(&send("m".repeat(5000)), &state)
            .await;
        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ServerMsg push")
            .received(encoding);
        let limits = state.policy().text_limits();
        assert_eq!(
            delivered.string(FieldId::Data).unwrap().len(),
            limits.message.unwrap()
        );
        assert_eq!(
            delivered.string(FieldId::QuotingMsg).unwrap().len(),
            limits.message.unwrap()
        );

        let replies = session
            .on_send_instant_msg(&send(String::new()), &state)
            .await;
        assert!(!replies.last().unwrap().is_error());
        assert!(target_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn send_instant_msg_and_user_broadcast_without_data_follow_the_policy() {
        struct Refuse;
        impl crate::ServerPolicy for Refuse {
            fn error(&self, _case: crate::Silent) -> Option<std::borrow::Cow<'static, str>> {
                Some("refused".into())
            }
        }
        let session = test_session("Alice", Access::all());
        let pm = request(
            Opcode::SendInstantMsg,
            1,
            vec![Field::u16(FieldId::UserID, 42)],
        );
        let broadcast = request(Opcode::UserBroadcast, 2, vec![]);

        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;
        let replies = session.on_send_instant_msg(&pm, &state).await;
        assert!(!replies.last().unwrap().is_error());
        assert!(!session.on_user_broadcast(&broadcast, &state).await[0].is_error());
        assert!(target_rx.try_recv().is_err());

        let state = state_with_policy(Refuse);
        state.insert_user(test_user(42, "Bob")).await;
        assert!(session.on_send_instant_msg(&pm, &state).await[0].is_error());
        assert!(session.on_user_broadcast(&broadcast, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn send_instant_msg_to_a_user_who_is_gone_is_an_error() {
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;

        assert_eq!(replies.len(), 1);
        assert_eq!(
            error_text(&replies[0]),
            "Couldn't send your message because the client is not connected to this server."
        );
    }

    #[tokio::test]
    async fn msg_allowed_via_access_extra_msg_without_send_message() {
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.insert(AccessExtra::MSG_OVERRIDE);
        let state = Arc::<SharedState>::default();
        let mut target_rx = state.register_recipient(42).await;
        state.insert_user(test_user(42, "Bob")).await;

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;
        assert!(!replies[0].is_error());
        assert!(target_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn msg_denied_without_send_message_or_access_extra_msg() {
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.remove(AccessExtra::MSG_OVERRIDE);
        let state = Arc::<SharedState>::default();

        let request = request(
            Opcode::SendInstantMsg,
            1,
            vec![
                Field::u16(FieldId::UserID, 42),
                Field::string(FieldId::Data, "hi there"),
            ],
        );
        let replies = session.on_send_instant_msg(&request, &state).await;
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn user_broadcast_is_cut_and_an_empty_one_is_not_sent() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut rx = state.register_recipient(2).await;
        let broadcast = |data: String| {
            request(
                Opcode::UserBroadcast,
                1,
                vec![Field::string(FieldId::Data, data)],
            )
        };

        session
            .on_user_broadcast(&broadcast("b".repeat(5000)), &state)
            .await;
        let sent = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserBroadcast push");
        assert_eq!(
            sent.received(encoding).string(FieldId::Data).unwrap().len(),
            state.policy().text_limits().message.unwrap()
        );

        let replies = session
            .on_user_broadcast(&broadcast(String::new()), &state)
            .await;
        assert!(!replies[0].is_error());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn user_broadcast_reaches_recipients_with_different_encodings() {
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        let utf8 = hotline_proto::Encoding::for_label(b"utf-8").unwrap();
        let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}";
        let session = test_session("Alice", Access::all());
        session.encoding.set(utf8);
        let state = Arc::<SharedState>::default();
        let mut sjis_rx = state.register_recipient(2).await;
        let mut utf8_rx = state.register_recipient(3).await;

        let request = Transaction::request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, konnichiwa)],
        )
        .received(utf8);
        session.on_user_broadcast(&request, &state).await;

        let next = |rx: &mut crate::state::Inbox| {
            rx.try_recv()
                .ok()
                .and_then(RecipientMessage::into_push)
                .expect("expected a UserBroadcast push")
        };
        let to_sjis = next(&mut sjis_rx);
        let to_utf8 = next(&mut utf8_rx);
        assert_eq!(to_sjis, to_utf8);

        let sjis_data = to_sjis.received(sjis);
        let utf8_data = to_utf8.received(utf8);
        let bytes =
            |tx: &Transaction<Received>| tx.field(FieldId::Data).unwrap().as_bytes().to_vec();
        assert_ne!(bytes(&sjis_data), bytes(&utf8_data));
        assert_eq!(sjis_data.string(FieldId::Data).as_deref(), Some(konnichiwa));
        assert_eq!(utf8_data.string(FieldId::Data).as_deref(), Some(konnichiwa));
    }

    #[tokio::test]
    async fn message_broadcast_reaches_everyone_except_sender() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let mut sender_rx = state.register_recipient(session.user_id).await;
        let mut rx = state.register_recipient(2).await;

        let request = request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, "server going down")],
        );
        let replies = session.on_user_broadcast(&request, &state).await;
        assert!(!replies[0].is_error());

        let sent = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserBroadcast push");
        assert_eq!(sent.r#type(), TransactionType::Push(Opcode::UserBroadcast));
        assert_eq!(
            sent.received(encoding)
                .field(FieldId::Data)
                .unwrap()
                .as_string(),
            "server going down"
        );

        assert!(sender_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn message_broadcast_denied_without_privilege() {
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();

        let request = request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, "hi")],
        );
        let replies = session.on_user_broadcast(&request, &state).await;
        assert!(replies[0].is_error());
    }
}
