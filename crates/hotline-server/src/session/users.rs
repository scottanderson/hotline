//! Handlers that act on another connected user: listing, info,
//! kick, private messages, and broadcast.

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::FieldId;
use hotline_proto::{Field, Transaction};

use crate::account::AccessExtra;
use crate::flood::FloodEvent;
use crate::state::{SharedState, TransferProgress, TransferSection, UserEntry};

use super::connection::{KickBan, MsgOption, Session};

/// `lines`, or `None.` when there are none.
fn or_none(lines: String) -> String {
    if lines.is_empty() {
        "None.\r".to_string()
    } else {
        lines
    }
}

impl Session {
    /// [`myTran_GetUserNameList`] (300)
    ///
    /// [`myTran_GetUserNameList`]: hotline_proto::Opcode::GetUserNameList
    pub(super) async fn on_get_user_name_list(
        &self,
        tx: &Transaction,
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
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::GET_CLIENT_INFO) {
            return vec![tx.reply_error(reason)];
        }
        let Some(target_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let Some(target) = state.user(target_id).await else {
            return vec![tx.reply_error("no such user")];
        };

        let report = state.transfer_report(target_id).await;
        let section = |section| {
            let lines = report.active.iter().filter(|p| p.section == section);
            or_none(lines.map(TransferProgress::line).collect())
        };
        let waiting = or_none(
            report
                .waiting
                .iter()
                .map(TransferProgress::waiting_line)
                .collect(),
        );
        let online = target.connected_at.elapsed();
        let version_line = target
            .client_version
            .map(|version| format!("Version:    {version}\r"))
            .unwrap_or_default();
        let text = format!(
            "Nickname:   {name}\r\
             Name:       {account_name}\r\
             Account:    {login}\r\
             Icon:       {icon}\r\
             {version_line}\
             Address:    {addr}\r\
             Online:     {mins}m {secs}s\r\r\
             -------- File Downloads ---------\r\r{file_downloads}\r\
             ------- Folder Downloads --------\r\r{folder_downloads}\r\
             --------- File Uploads ----------\r\r{file_uploads}\r\
             -------- Folder Uploads ---------\r\r{folder_uploads}\r\
             ------- Waiting Downloads -------\r\r{waiting}",
            name = target.name,
            account_name = if target.account_name.is_empty() {
                "(none)"
            } else {
                &target.account_name
            },
            login = if target.login.is_empty() {
                "(guest)"
            } else {
                &target.login
            },
            addr = target.remote_addr,
            icon = target.icon,
            mins = online.as_secs() / 60,
            secs = online.as_secs() % 60,
            file_downloads = section(TransferSection::FileDownload),
            folder_downloads = section(TransferSection::FolderDownload),
            file_uploads = section(TransferSection::FileUpload),
            folder_uploads = section(TransferSection::FolderUpload),
        );

        vec![tx.reply(vec![
            Field::string(FieldId::Data, text),
            Field::string(FieldId::UserName, target.name),
        ])]
    }

    /// [`myTran_DisconnectUser`] (110): disconnect another user, and if the
    /// [`myField_Options`] (113) kick-to-ban bit is set, ban their address and
    /// notify them why before disconnecting them. A missing target id defaults
    /// to 0, which no session holds, so the kick is just a no-op.
    ///
    /// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    pub(super) async fn on_disconnect_user(
        &mut self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DISCON_USER) {
            return vec![tx.reply_error(reason)];
        }
        let target_id = tx
            .field(FieldId::UserID)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        if let Err(reason) = state.authorize_kick(target_id).await {
            return vec![tx.reply_error(reason)];
        }
        let ban = KickBan::from(
            tx.field(FieldId::Options)
                .and_then(Field::as_u16)
                .unwrap_or(0),
        );
        let target_ip = state.user(target_id).await.map(|u| u.remote_addr.ip());

        let notice = match (ban, target_ip) {
            (KickBan::Temporary, Some(ip)) => {
                state.temp_ban(ip).await;
                Some("You are temporarily banned on this server.")
            }
            (KickBan::Permanent, Some(ip)) => {
                if let Err(error) = state.ban(ip, None).await {
                    tracing::warn!(%ip, %error, "failed to persist ban");
                }
                Some("You are permanently banned on this server.")
            }
            (KickBan::None, _) | (_, None) => None,
        };

        match notice {
            Some(reason) => {
                state
                    .send_to(
                        target_id,
                        Transaction::push(
                            Opcode::DisconnectMsg,
                            vec![Field::string(FieldId::Data, reason)],
                        ),
                    )
                    .await;
            }
            None => state.disconnect(target_id).await,
        }

        vec![tx.reply(vec![])]
    }

    /// The repeat and message count checks for a private message or broadcast.
    async fn message_flood_banned(&self, tx: &Transaction, state: &SharedState) -> bool {
        self.flood_banned(FloodEvent::Text(&self.data_bytes(tx)), state)
            .await
            || self.flood_banned(FloodEvent::Message, state).await
    }

    /// [`myTran_SendInstantMsg`] (108): a private instant message to one other
    /// user.
    ///
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    pub(super) async fn on_send_instant_msg(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if self.message_flood_banned(tx, state).await {
            return vec![];
        }
        if let Err(reason) = self.access.require(Access::SEND_MESSAGE) {
            if !self.access_extra.contains(AccessExtra::MSG_OVERRIDE) {
                return vec![tx.reply_error(reason)];
            }
        }
        let Some(target_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let encoding = self.encoding();
        let Some(message) = tx.string(FieldId::Data, encoding) else {
            return vec![tx.reply_error("missing message")];
        };
        let options = tx
            .field(FieldId::Options)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        let is_user_message = MsgOption::from(options) == MsgOption::UserMessage;

        let target = state.user(target_id).await;
        let mut replies = Vec::new();
        if is_user_message {
            if let Some(bounce) = target
                .as_ref()
                .and_then(|target| self.refusal_bounce(target_id, target, false))
            {
                replies.push(bounce);
            }
        }
        if !target.is_some_and(|target| target.refuses_pm) {
            let mut fields = vec![
                Field::u16(FieldId::UserID, self.user_id),
                Field::string(FieldId::UserName, self.name.clone()),
                Field::u16(FieldId::Options, options),
                Field::string(FieldId::Data, message),
            ];
            if let Some(quote) = tx.field(FieldId::QuotingMsg) {
                fields.push(Field::bytes(
                    FieldId::QuotingMsg,
                    quote.as_bytes(encoding).into_owned(),
                ));
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
    /// user.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    pub(super) async fn on_user_broadcast(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::BROADCAST) {
            return vec![tx.reply_error(reason)];
        }
        if self.message_flood_banned(tx, state).await {
            return vec![tx.reply(vec![])];
        }
        let encoding = self.encoding();
        let Some(message) = tx.string(FieldId::Data, encoding) else {
            return vec![tx.reply_error("missing message")];
        };

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
        let state = SharedState::default();
        let request = Transaction::request(Opcode::GetUserNameList, 1, vec![]);

        assert!(session.on_get_user_name_list(&request, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn get_user_name_list_encodes_entries_using_the_sessions_active_encoding() {
        let session = test_session("Alice", Access::all());
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        session.encoding.set(sjis);
        let state = SharedState::default();
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
        let request = Transaction::request(Opcode::GetUserNameList, 1, vec![]);

        let reply = &session.on_get_user_name_list(&request, &state).await[0];
        let entry = reply
            .field(FieldId::UserNameWithInfo)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes(sjis), sjis).unwrap())
            .expect("expected bob's user list entry");
        assert_eq!(&*entry.name, konnichiwa);
    }

    #[tokio::test]
    async fn get_client_info_text_reports_the_reference_shaped_text_block() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
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
        let request = Transaction::request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        assert_eq!(reply.string(FieldId::UserName, encoding).unwrap(), "Bob");
        assert!(reply.field(FieldId::UserID).is_none());
        let text = reply.string(FieldId::Data, encoding).unwrap();
        assert!(text.contains("Nickname:   Bob"));
        assert!(text.contains("Name:       Bobby"));
        assert!(text.contains("Account:    bob"));
        assert!(text.contains("Address:    192.0.2.1:5500"));
        assert!(text.contains("Icon:       42"));
        assert!(text.contains("Version:    185"));
        assert!(text.contains(
            "-------- File Downloads ---------\r\rNone.\r\r\
             ------- Folder Downloads --------\r\rNone.\r\r\
             --------- File Uploads ----------\r\rNone.\r\r\
             -------- Folder Uploads ---------\r\rNone.\r\r\
             ------- Waiting Downloads -------\r\rNone.\r"
        ));
    }

    fn file_transfer(
        name: &str,
        direction: crate::state::TransferDirection,
        size: u64,
    ) -> crate::state::PendingTransfer {
        crate::state::PendingTransfer {
            kind: crate::state::TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "bob".into(),
                    dir: Default::default(),
                    name: name.into(),
                },
                raw: false,
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
        let state = SharedState::default();
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
            .register_transfer(file_transfer("up.txt", Put, 2048))
            .await;
        for name in ["first.txt", "second.txt"] {
            let reference = state.register_transfer(file_transfer(name, Get, 10)).await;
            state.admit_download(reference, 2).await;
        }
        let request = Transaction::request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        let text = reply.string(FieldId::Data, encoding).unwrap();
        assert!(text.contains(
            "-------- File Downloads ---------\r\rfirst.txt             0%   <1K\r\r\
             ------- Folder Downloads --------\r\rNone.\r\r\
             --------- File Uploads ----------\r\rup.txt                0%   2K\r\r\
             -------- Folder Uploads ---------\r\rNone.\r\r\
             ------- Waiting Downloads -------\r\rsecond.txt            (File)\r"
        ));
    }

    #[tokio::test]
    async fn get_client_info_text_defaults_login_and_account_name_when_empty() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: Box::default(),
                account_name: Box::default(),
                name: "Bob".into(),
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
        let request = Transaction::request(
            Opcode::GetClientInfoText,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );

        let reply = &session.on_get_client_info_text(&request, &state).await[0];
        let text = reply.string(FieldId::Data, encoding).unwrap();
        assert!(text.contains("Account:    (guest)"));
        assert!(text.contains("Name:       (none)"));
        assert!(!text.contains("Version:"));
    }

    #[tokio::test]
    async fn kick_denied_without_privilege() {
        let mut session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let request = Transaction::request(
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
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(2).await;
        let request = Transaction::request(
            Opcode::DisconnectUser,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());

        let msg = target_rx.try_recv().expect("expected a recipient message");
        assert!(msg.is_disconnect(), "plain kick must send no wire notice");
        assert!(target_rx.try_recv().is_err(), "only one message sent");
    }

    #[tokio::test]
    async fn kick_with_permanent_ban_option_bans_the_target_address() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target = test_user(2, "Bob");
        target.access.remove(Access::CANNOT_BE_DISCON);
        target.remote_addr = "192.0.2.9:5500".parse().unwrap();
        state.insert_user(target).await;
        let mut target_rx = state.register_recipient(2).await;

        let request = Transaction::request(
            Opcode::DisconnectUser,
            1,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::Options, 2),
            ],
        );
        let replies = session.on_disconnect_user(&request, &state).await;
        assert!(!replies[0].is_error());

        assert!(state
            .is_banned("192.0.2.9".parse().unwrap())
            .await
            .is_some());

        let notice = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a DisconnectMsg push notifying the target of the ban");
        assert_eq!(
            notice.string(FieldId::Data, encoding).unwrap(),
            "You are permanently banned on this server."
        );
        assert_eq!(
            notice.r#type(),
            TransactionType::Push(Opcode::DisconnectMsg)
        );
    }

    #[tokio::test]
    async fn kick_with_temporary_ban_option_sends_the_temporary_ban_notice() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target = test_user(2, "Bob");
        target.access.remove(Access::CANNOT_BE_DISCON);
        target.remote_addr = "192.0.2.9:5500".parse().unwrap();
        state.insert_user(target).await;
        let mut target_rx = state.register_recipient(2).await;

        let request = Transaction::request(
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

        let notice = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a DisconnectMsg push notifying the target of the ban");
        assert_eq!(
            notice.string(FieldId::Data, encoding).unwrap(),
            "You are temporarily banned on this server."
        );
    }

    #[tokio::test]
    async fn kick_with_missing_uid_replies_success() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let request = Transaction::request(Opcode::DisconnectUser, 1, vec![]);
        let replies = session.on_disconnect_user(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
    }

    #[tokio::test]
    async fn the_122nd_transaction_in_a_minute_bans_and_disconnects() {
        let state = state_with_flood();
        let mut session = test_session("Alice", Access::all());
        let mut rx = state.register_recipient(session.user_id).await;
        let ping = || Transaction::request(Opcode::KeepConnectionAlive, 1, vec![]);
        let config = ServerConfig::default();

        for _ in 0..121 {
            assert_eq!(session.handle(&ping(), &state, &config).await.len(), 1);
        }
        let replies = session.handle(&ping(), &state, &config).await;

        assert!(replies.is_empty());
        let msg = rx.try_recv().expect("expected a recipient message");
        assert!(msg.is_disconnect(), "a flood ban sends no wire notice");
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
                    &Transaction::request(Opcode::KeepConnectionAlive, 1, vec![]),
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
        let mut rx = state.register_recipient(session.user_id).await;
        let message = || {
            Transaction::request(
                Opcode::SendInstantMsg,
                1,
                vec![
                    Field::u16(FieldId::UserID, 2),
                    Field::string(FieldId::Data, "buy now"),
                ],
            )
        };

        for _ in 0..5 {
            assert!(!session
                .on_send_instant_msg(&message(), &state)
                .await
                .is_empty());
        }

        assert!(session
            .on_send_instant_msg(&message(), &state)
            .await
            .is_empty());
        assert!(rx.try_recv().expect("a disconnect").is_disconnect());
    }

    #[tokio::test]
    async fn user_broadcast_still_replies_to_the_broadcast_that_bans() {
        let state = state_with_flood();
        let session = test_session("Alice", Access::all());
        let broadcast = Transaction::request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, "buy now")],
        );
        for _ in 0..5 {
            session.on_user_broadcast(&broadcast, &state).await;
        }
        let mut rx = state.register_recipient(session.user_id).await;

        let replies = session.on_user_broadcast(&broadcast, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
        assert!(rx.try_recv().expect("a disconnect").is_disconnect());
    }

    #[tokio::test]
    async fn private_message_delivered_only_to_target() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(42).await;
        let mut sender_rx = state.register_recipient(session.user_id).await;

        let request = Transaction::request(
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
            delivered.string(FieldId::Data, encoding).unwrap(),
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
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(42).await;

        let request = Transaction::request(
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
            delivered.field(FieldId::Options).unwrap().as_u16(),
            Some(MsgOption::UserMessage.into())
        );
    }

    #[tokio::test]
    async fn private_message_bounces_and_is_not_delivered_when_target_refuses() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                refuses_pm: true,
                ..test_user(42, "Bob")
            })
            .await;
        let mut target_rx = state.register_recipient(42).await;

        let request = Transaction::request(
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
            replies[0].string(FieldId::Data, encoding).unwrap(),
            "Bob does not accept private messages."
        );
        assert_eq!(
            replies[0].field(FieldId::Options).unwrap().as_u16(),
            Some(MsgOption::RefuseMessage.into())
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
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                automatic_response: Some("At lunch, back soon.".into()),
                ..test_user(42, "Bob")
            })
            .await;
        let mut target_rx = state.register_recipient(42).await;

        let request = Transaction::request(
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
            replies[0].string(FieldId::Data, encoding).unwrap(),
            "At lunch, back soon."
        );
        assert_eq!(
            replies[0].field(FieldId::Options).unwrap().as_u16(),
            Some(MsgOption::AutomaticResponse.into())
        );
        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("automatic response still lets the message through");
        assert_eq!(
            delivered.string(FieldId::Data, encoding).unwrap(),
            "hi there"
        );
    }

    #[tokio::test]
    async fn private_message_no_bounce_when_sender_also_refuses() {
        let mut session = test_session("Alice", Access::all());
        session.refuses_pm = true;
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                refuses_pm: true,
                ..test_user(42, "Bob")
            })
            .await;

        let request = Transaction::request(
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
    async fn msg_allowed_via_access_extra_msg_without_send_message() {
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.insert(AccessExtra::MSG_OVERRIDE);
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(42).await;

        let request = Transaction::request(
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
        let state = SharedState::default();

        let request = Transaction::request(
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
    async fn message_broadcast_reaches_everyone_except_sender() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut sender_rx = state.register_recipient(session.user_id).await;
        let mut rx = state.register_recipient(2).await;

        let request = Transaction::request(
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
            sent.field(FieldId::Data).unwrap().as_string(encoding),
            "server going down"
        );

        assert!(sender_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn message_broadcast_denied_without_privilege() {
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();

        let request = Transaction::request(
            Opcode::UserBroadcast,
            1,
            vec![Field::string(FieldId::Data, "hi")],
        );
        let replies = session.on_user_broadcast(&request, &state).await;
        assert!(replies[0].is_error());
    }
}
