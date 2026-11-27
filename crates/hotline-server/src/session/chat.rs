//! Chat relay, console commands, and private chat rooms.

use std::borrow::Cow;

use hotline_proto::FieldId;
use hotline_proto::consts::Access;
use hotline_proto::opcode::Opcode;
use hotline_proto::text_encoding;
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::{Field, Received, Transaction};

use crate::account::AccessExtra;
use crate::console::{ChatCommandActor, ConsoleReply};
use crate::flood::{FloodEvent, MAX_CHAT_LINE};
use crate::policy::{ChatRooms, Silent};
use crate::server::ServerConfig;
use crate::state::SharedState;

use super::connection::Session;

impl Session {
    /// [`myTran_ChatSend`] (105): public or (if [`myField_ChatID`] (114) is
    /// present) private-room chat, gated on `SEND_CHAT`. A message that
    /// starts with the console prefix runs as a console command instead.
    ///
    /// [`myTran_ChatSend`]: hotline_proto::Opcode::ChatSend
    /// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
    pub(super) async fn on_chat_send(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        if self.flood_banned(FloodEvent::Chat, state).await {
            return Vec::new();
        }
        let encoding = self.encoding();
        if !self.access.contains(Access::SEND_CHAT) {
            let reason = "You are not allowed to participate in chat.";
            // Sent only to the denied client; `myTran_ChatSend` (105) gets no reply.
            return vec![Transaction::push(
                Opcode::ServerMsg,
                vec![
                    Field::u16(FieldId::ChatOptions, 1),
                    Field::string(FieldId::Data, reason),
                ],
            )];
        }

        let message = tx.string(FieldId::Data).unwrap_or_default();
        let chat_id = tx.field(FieldId::ChatID).map(Field::as_u32);
        if let Some((prefix, command_line)) = config
            .console_prefix
            .and_then(|prefix| Some((prefix, message.strip_prefix(prefix)?)))
        {
            return self
                .on_console_command(prefix, command_line, chat_id, state)
                .await;
        }

        let style = tx.field(FieldId::ChatOptions).map(Field::as_u16);

        let prefix = match style == Some(1) {
            true => format!("\r *** {} ", self.name),
            false => format!("\r{:>13.13}:  ", self.name),
        };
        let limits = state.policy().text_limits();
        let message = self.cut_text(limits.chat_line, &message);
        let text = format!("\r{message}").replace('\r', &prefix);
        let text = self.cut_text(limits.chat_line, &text).to_owned();
        let mut sent = self.data_bytes(tx).to_vec();
        sent.truncate(MAX_CHAT_LINE);
        let relayed = text_encoding::encode(encoding, &text).len();
        if self.flood_banned(FloodEvent::Text(&sent), state).await
            || self
                .flood_banned(FloodEvent::ChatBytes(relayed), state)
                .await
        {
            return Vec::new();
        }
        let mut fields = vec![Field::string(FieldId::Data, text)];
        if let Some(chat_id) = chat_id {
            fields.push(Field::u32(FieldId::ChatID, chat_id));
        }
        let relay = Transaction::push(Opcode::ChatMsg, fields);

        match chat_id {
            // Room chat: only members should see it.
            Some(chat_id) => {
                if let Some(members) = state.chat_room_members(chat_id).await {
                    for member in members {
                        state.send_to(member, relay.clone()).await;
                    }
                }
            }
            // Public chat: everyone.
            None => state.publish(relay).await,
        }
        Vec::new()
    }

    /// Runs a console command line. Any reply goes only to this session.
    pub(super) async fn on_console_command(
        &self,
        prefix: char,
        line: &str,
        chat_id: Option<u32>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if line.is_empty() {
            return Vec::new();
        }
        let (command, args) = line.split_once(' ').unwrap_or((line, ""));
        let actor = ChatCommandActor {
            user_id: self.user_id,
            login: self.login.clone(),
            access: self.access,
            access_extra: self.access_extra,
            prefix,
        };
        let reply = state
            .run_console_command(command, args.trim_start(), &actor)
            .await;
        match reply {
            ConsoleReply::Silent => Vec::new(),
            ConsoleReply::Chat(text) => {
                let mut fields = vec![Field::string(FieldId::Data, format!("\r{text}"))];
                if let Some(chat_id) = chat_id {
                    fields.push(Field::u32(FieldId::ChatID, chat_id));
                }
                vec![Transaction::push(Opcode::ChatMsg, fields)]
            }
            ConsoleReply::Message(text) => vec![Transaction::push(
                Opcode::ServerMsg,
                vec![
                    Field::u16(FieldId::UserID, self.user_id),
                    Field::string(FieldId::Data, text),
                    Field::string(FieldId::UserName, ConsoleReply::SENDER),
                ],
            )],
        }
    }

    /// [`myTran_InviteNewChat`] (112): create a private chat room and invite its
    /// target users.
    ///
    /// [`myTran_InviteNewChat`]: hotline_proto::Opcode::InviteNewChat
    pub(super) async fn on_invite_new_chat(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if self.flood_banned(FloodEvent::Message, state).await {
            return vec![];
        }
        if !self.access.contains(Access::CREATE_CHAT) {
            let reason = "You are not allowed to create a new chat.";
            return vec![tx.reply_error(reason)];
        }
        let chat_id = state.create_chat_room(self.user_id).await;
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);
        let mut replies = Vec::new();
        for target_id in tx.fields(FieldId::UserID).map(Field::as_u16) {
            if target_id == self.user_id {
                continue;
            }
            let Some(target) = state.user(target_id).await else {
                tracing::debug!(target_id, "skipping undeliverable chat invite");
                continue;
            };
            replies.extend(self.refusal_bounce(target_id, &target, true));
            if target.refuses_chat {
                continue;
            }
            state.invite(chat_id, target_id).await;
            state
                .send_to(
                    target_id,
                    Transaction::push(
                        Opcode::InviteToChat,
                        vec![
                            Field::u32(FieldId::ChatID, chat_id),
                            Field::u16(FieldId::UserID, self.user_id),
                            Field::string(FieldId::UserName, self.name.to_string()),
                        ],
                    ),
                )
                .await;
        }
        replies.push(tx.reply(vec![
            Field::u32(FieldId::ChatID, chat_id),
            Field::u16(FieldId::UserID, self.user_id),
            Field::u16(FieldId::UserIconID, icon),
            Field::u16(FieldId::UserFlags, self.user_flags().bits()),
            Field::string(FieldId::UserName, self.name.to_string()),
        ]));
        replies
    }

    /// [`myTran_InviteToChat`] (113): invite each listed user to a room this
    /// session belongs to.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    pub(super) async fn on_invite_to_chat(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        chat_request_replies(tx, self.try_invite_to_chat(tx, state).await, state)
    }

    /// The work of [`Self::on_invite_to_chat`]. `Ok` has the refusal
    /// bounces. `Err` only when no user is invited.
    async fn try_invite_to_chat(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Result<Vec<Transaction>, Cow<'static, str>> {
        if self.flood_banned(FloodEvent::Message, state).await {
            return Ok(vec![]);
        }
        if !self.access_extra.contains(AccessExtra::CHAT_PRIVATE) {
            Err("You are not allowed to open or create a private chat.")?;
        }
        let chat_id = tx.field(FieldId::ChatID).map(Field::as_u32).unwrap_or(0);
        let target_ids: Vec<u16> = tx
            .fields(FieldId::UserID)
            .map(Field::as_u16)
            .filter(|&id| id != 0)
            .collect();
        if chat_id == 0 || target_ids.is_empty() {
            Err("missing chat id or user id")?;
        }

        let mut replies = Vec::new();
        let mut invited = false;
        let mut refusal = None;
        for target_id in target_ids {
            if let Err(reason) = state
                .invite_to_chat_room(chat_id, self.user_id, target_id)
                .await
            {
                refusal = Some(reason);
                continue;
            }
            invited = true;
            let Some(target) = state.user(target_id).await else {
                continue;
            };
            replies.extend(self.refusal_bounce(target_id, &target, true));
            if !target.refuses_chat {
                state
                    .send_to(
                        target_id,
                        Transaction::push(
                            Opcode::InviteToChat,
                            vec![
                                Field::u32(FieldId::ChatID, chat_id),
                                Field::u16(FieldId::UserID, self.user_id),
                                Field::string(FieldId::UserName, self.name.to_string()),
                            ],
                        ),
                    )
                    .await;
            }
        }
        if let Some(reason) = refusal.filter(|_| !invited) {
            Err(reason)?;
        }
        Ok(replies)
    }

    /// [`myTran_JoinChat`] (115): join a room, as the policy's
    /// [`ChatRooms`] lets it. The reply has the room's subject, if it has
    /// one, then each member with a name. The other members get a
    /// [`myTran_NotifyChatChangeUser`] (117) push for a new member.
    ///
    /// [`myTran_NotifyChatChangeUser`]: hotline_proto::Opcode::NotifyChatChangeUser
    ///
    /// [`myTran_JoinChat`]: hotline_proto::Opcode::JoinChat
    pub(super) async fn on_join_chat(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Transaction {
        if !self.access_extra.contains(AccessExtra::CHAT_PRIVATE) {
            let reason = "You are not allowed to open or create a private chat.";
            return tx.reply_error(reason);
        }
        let chat_id = tx.field(FieldId::ChatID).map(Field::as_u32).unwrap_or(0);
        let encoding = self.encoding();
        let password = tx.string(FieldId::UserPassword).unwrap_or_default();
        let new = match state.join_chat_room(chat_id, self.user_id, &password).await {
            Ok(new) => new,
            Err(reason) => return tx.reply_error(reason),
        };
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);

        let notify = Transaction::push(
            Opcode::NotifyChatChangeUser,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, self.user_id),
                Field::u16(FieldId::UserIconID, icon),
                Field::u16(FieldId::UserFlags, self.user_flags().bits()),
                Field::string(FieldId::UserName, self.name.to_string()),
            ],
        );

        let room = state.chat_room(chat_id).await.unwrap_or_default();
        let mut fields = Vec::new();
        if !room.subject.is_empty() {
            fields.push(Field::string(
                FieldId::ChatSubject,
                room.subject.into_string(),
            ));
        }
        for member_id in room.members {
            if new && member_id != self.user_id {
                state.send_to(member_id, notify.clone()).await;
            }
            if let Some(entry) = state
                .user(member_id)
                .await
                .filter(|entry| !entry.name.is_empty())
            {
                fields.push(Field::bytes(
                    FieldId::UserNameWithInfo,
                    UserNameWithInfo {
                        user_id: entry.user_id,
                        icon: entry.icon,
                        flags: entry.flags(),
                        name: entry.name,
                    }
                    .encode(encoding),
                ));
            }
        }
        tx.reply(fields)
    }

    /// [`myTran_LeaveChat`] (116): leave a chat room. Chat ID 0, the main
    /// room, cannot be left.
    ///
    /// [`myTran_LeaveChat`]: hotline_proto::Opcode::LeaveChat
    pub(super) async fn on_leave_chat(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if !self.access_extra.contains(AccessExtra::CHAT_PRIVATE) {
            let reason = "You are not allowed to open or create a private chat.";
            return vec![tx.reply_error(reason)];
        }
        let chat_id = tx.field(FieldId::ChatID).map(Field::as_u32).unwrap_or(0);
        if chat_id == 0 {
            return vec![];
        }
        let Some(remaining) = state.leave_chat_room(chat_id, self.user_id).await else {
            return vec![];
        };

        let notify = Transaction::push(
            Opcode::NotifyChatDeleteUser,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, self.user_id),
            ],
        );
        for member in remaining {
            state.send_to(member, notify.clone()).await;
        }
        vec![]
    }

    /// [`myTran_RejectChatInvite`] (114): decline a chat invitation.
    ///
    /// [`myTran_RejectChatInvite`]: hotline_proto::Opcode::RejectChatInvite
    pub(super) async fn on_reject_chat_invite(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if !self.access_extra.contains(AccessExtra::CHAT_PRIVATE) {
            let reason = "You are not allowed to open or create a private chat.";
            return vec![tx.reply_error(reason)];
        }
        if let Some(chat_id) = tx.field(FieldId::ChatID).map(Field::as_u32) {
            state.invite_declined(chat_id, self.user_id).await;
            if let Some(members) = state.chat_room_members(chat_id).await {
                let notify = Transaction::push(
                    Opcode::ChatMsg,
                    vec![
                        Field::string(
                            FieldId::Data,
                            format!("\r<<< \"{}\" declined invitation to chat >>>", self.name),
                        ),
                        Field::u32(FieldId::ChatID, chat_id),
                    ],
                );
                for member in members {
                    state.send_to(member, notify.clone()).await;
                }
            }
        }
        vec![]
    }

    /// [`myTran_SetChatSubject`] (120): set a chat room's subject and/or its join
    /// password, as the policy's [`ChatRooms`] lets it. The subject is cut
    /// to [`TextLimits::chat_subject`].
    ///
    /// [`myTran_SetChatSubject`]: hotline_proto::Opcode::SetChatSubject
    /// [`TextLimits::chat_subject`]: crate::TextLimits::chat_subject
    pub(super) async fn on_set_chat_subject(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        chat_request_replies(tx, self.try_set_chat_subject(tx, state).await, state)
    }

    /// The work of [`Self::on_set_chat_subject`].
    async fn try_set_chat_subject(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Result<Vec<Transaction>, Cow<'static, str>> {
        if !self.access_extra.contains(AccessExtra::CHAT_PRIVATE) {
            Err("You are not allowed to open or create a private chat.")?;
        }
        let chat_id = tx
            .field(FieldId::ChatID)
            .map(Field::as_u32)
            .ok_or("missing chat id")?;
        let mut subject = tx.string(FieldId::ChatSubject).map(|subject| {
            self.cut_text(state.policy().text_limits().chat_subject, &subject)
                .to_owned()
        });
        let mut password = tx.string(FieldId::UserPassword);
        if state.policy().chat_rooms() == ChatRooms::Open {
            subject = Some(subject.unwrap_or_default());
            password = None;
        }
        if subject.is_none() && password.is_none() {
            Err("missing subject or password")?;
        }

        let is_global = chat_id == 0;
        state
            .authorize_chat_subject_change(chat_id, self.user_id, is_global, self.access_extra)
            .await?;

        if let Some(subject) = subject {
            state.set_chat_subject(chat_id, &subject).await;
            self.broadcast_chat_field(
                state,
                chat_id,
                is_global,
                Field::string(FieldId::ChatSubject, subject),
            )
            .await;
        }
        if let Some(password) = password {
            state.set_chat_password(chat_id, &password).await;
            self.broadcast_chat_field(
                state,
                chat_id,
                is_global,
                Field::string(FieldId::UserPassword, password),
            )
            .await;
        }
        Ok(vec![])
    }

    /// Send a [`myTran_NotifyChatSubject`] (119) push carrying one changed
    /// field (subject or password) to a room's members, or everyone for the
    /// main room.
    ///
    /// [`myTran_NotifyChatSubject`]: hotline_proto::Opcode::NotifyChatSubject
    pub(super) async fn broadcast_chat_field(
        &self,
        state: &SharedState,
        chat_id: u32,
        is_global: bool,
        field: Field,
    ) {
        let notify = Transaction::push(
            Opcode::NotifyChatSubject,
            vec![Field::u32(FieldId::ChatID, chat_id), field],
        );
        if is_global {
            state.publish(notify).await;
        } else if let Some(members) = state.chat_room_members(chat_id).await {
            for member in members {
                state.send_to(member, notify.clone()).await;
            }
        }
    }
}

/// The replies to a chat request: none for the request itself on success,
/// and only the policy's error on failure.
fn chat_request_replies(
    tx: &Transaction<Received>,
    result: Result<Vec<Transaction>, Cow<'static, str>>,
    state: &SharedState,
) -> Vec<Transaction> {
    result.unwrap_or_else(|reason| {
        state
            .policy()
            .error(Silent::ChatRequest { reason })
            .map(|error| tx.reply_error(error))
            .into_iter()
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::session::connection::MsgOption;
    use crate::state::{ConsoleUpdate, RecipientMessage, UserEntry};
    use hotline_proto::opcode::TransactionType;
    use std::time::Instant;

    use crate::session::test_support::*;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn chat_send_past_the_byte_limit_bans_before_the_access_check() {
        let state = state_with_flood();
        let mut session = test_session("Alice", Access::all());
        let rx = state.register_recipient(session.user_id).await;
        let config = ServerConfig::default();
        let line = |i: usize| {
            request(
                Opcode::ChatSend,
                1,
                vec![Field::string(FieldId::Data, format!("line {i}"))],
            )
        };
        // 114 short lines count 90 bytes each, 10260 in total.
        for i in 0..114 {
            session.on_chat_send(&line(i), &state, &config).await;
        }
        session.access.remove(Access::SEND_CHAT);

        assert!(
            session
                .on_chat_send(&line(114), &state, &config)
                .await
                .is_empty()
        );
        assert!(rx.is_ended());
        assert!(state.is_banned(session.remote_addr.ip()).await.is_some());
    }

    #[tokio::test]
    async fn chat_relay_starts_with_cr_for_scrollback_clients() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "hi")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(
            session
                .on_chat_send(&request, &state, &ServerConfig::default())
                .await
                .is_empty()
        );

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(encoding)
            .string(FieldId::Data)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn chat_relay_uses_emote_format_for_style_1() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Data, "waves"),
                Field::u16(FieldId::ChatOptions, 1),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(
            session
                .on_chat_send(&request, &state, &ServerConfig::default())
                .await
                .is_empty()
        );

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(encoding)
            .string(FieldId::Data)
            .unwrap();
        assert_eq!(text, "\r *** Alice waves");
    }

    #[tokio::test]
    async fn chat_send_relay_is_cut_to_8192_bytes() {
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "a".repeat(10_000))],
        );

        let mut rx = state.register_recipient(1).await;
        session
            .on_chat_send(&request, &state, &ServerConfig::default())
            .await;

        let relay = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(text_encoding::DEFAULT);
        let text = relay.string(FieldId::Data).unwrap();
        assert_eq!(text.len(), MAX_CHAT_LINE);
        assert!(text.starts_with("\r        Alice:  aaa"));
    }

    #[tokio::test]
    async fn chat_relay_prefixes_every_line_of_a_multiline_message() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "line one\rline two")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(
            session
                .on_chat_send(&request, &state, &ServerConfig::default())
                .await
                .is_empty()
        );

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(encoding)
            .string(FieldId::Data)
            .unwrap();
        assert_eq!(text, "\r        Alice:  line one\r        Alice:  line two");
    }

    #[tokio::test]
    async fn chat_relay_truncates_a_long_name_keeping_its_start() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("ANameThatIsWayTooLong", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "hi")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(
            session
                .on_chat_send(&request, &state, &ServerConfig::default())
                .await
                .is_empty()
        );

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(encoding)
            .string(FieldId::Data)
            .unwrap();
        assert_eq!(text, "\rANameThatIsWa:  hi");
    }

    #[tokio::test]
    async fn chat_relay_keeps_normal_format_for_non_emote_style() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Data, "hi"),
                Field::u16(FieldId::ChatOptions, 0),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(
            session
                .on_chat_send(&request, &state, &ServerConfig::default())
                .await
                .is_empty()
        );

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(encoding)
            .string(FieldId::Data)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn chat_starting_with_slash_dispatches_as_a_console_command_not_a_relay() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "/users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session
            .on_chat_send(&request, &state, &console_config())
            .await;

        // `/users` answers with a private message from `hxd`.
        assert!(rx.try_recv().is_err());
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
        assert_eq!(
            reply[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::UserName)
                .as_deref(),
            Some("hxd")
        );
        assert_eq!(
            reply[0]
                .received(text_encoding::DEFAULT)
                .field(FieldId::UserID)
                .map(Field::as_u16),
            Some(session.user_id)
        );
    }

    /// A command typed in a private room replies in that room.
    #[tokio::test]
    async fn console_reply_in_a_private_room_carries_its_chat_id() {
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Data, "/nope"),
                Field::u32(FieldId::ChatID, 7),
            ],
        );

        let reply = session
            .on_chat_send(&request, &state, &console_config())
            .await;

        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ChatMsg));
        assert_eq!(
            reply[0]
                .received(text_encoding::DEFAULT)
                .field(FieldId::ChatID)
                .map(Field::as_u32),
            Some(7)
        );
    }

    /// With no console prefix configured, a would-be command is just chat.
    #[tokio::test]
    async fn chat_starting_with_slash_relays_as_chat_when_console_is_off() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "/users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session
            .on_chat_send(&request, &state, &ServerConfig::default())
            .await;

        assert!(reply.is_empty());
        let relayed = rx.try_recv().expect("relayed as ordinary chat");
        let Some(push) = relayed.into_push() else {
            panic!("expected a chat push");
        };
        let text = push
            .received(text_encoding::DEFAULT)
            .field(FieldId::Data)
            .unwrap()
            .as_string();
        assert!(text.contains("/users"), "{text}");
    }

    /// A different prefix dispatches; `/` then relays as ordinary chat.
    #[tokio::test]
    async fn console_prefix_is_configurable() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = Arc::<SharedState>::default();
        let config = ServerConfig {
            console_prefix: Some('!'),
            ..ServerConfig::default()
        };
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "!users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session.on_chat_send(&request, &state, &config).await;

        assert!(rx.try_recv().is_err(), "a command must not relay as chat");
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
    }

    #[tokio::test]
    async fn console_command_is_denied_without_send_chat() {
        // SEND_CHAT is required even though /users itself needs only MANAGE_USERS.
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "/users")],
        );

        let reply = session
            .on_chat_send(&request, &state, &console_config())
            .await;
        let text = reply[0]
            .received(text_encoding::DEFAULT)
            .field(FieldId::Data)
            .unwrap()
            .as_string();
        assert!(text.contains("not allowed"));
    }

    #[tokio::test]
    async fn chat_send_without_data_relays_an_empty_line() {
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(Opcode::ChatSend, 1, vec![]);

        let mut rx = state.register_recipient(1).await;
        session
            .on_chat_send(&request, &state, &ServerConfig::default())
            .await;

        let relay = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .received(text_encoding::DEFAULT);
        assert_eq!(
            relay.string(FieldId::Data).as_deref(),
            Some("\r        Alice:  ")
        );
    }

    #[tokio::test]
    async fn chat_send_without_send_chat_or_data_sends_a_server_notice() {
        let session = test_session("Alice", Access::empty());
        let state = Arc::<SharedState>::default();
        let request = request(Opcode::ChatSend, 1, vec![]);

        let reply = session
            .on_chat_send(&request, &state, &ServerConfig::default())
            .await;

        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
    }

    #[tokio::test]
    async fn chat_send_without_send_chat_sends_a_server_notice() {
        let session = test_session("Alice", Access::empty());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "hi")],
        );

        let reply = session
            .on_chat_send(&request, &state, &ServerConfig::default())
            .await;

        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
        assert_eq!(
            reply[0]
                .received(text_encoding::DEFAULT)
                .field(FieldId::ChatOptions)
                .map(Field::as_u16),
            Some(1)
        );
    }

    #[tokio::test]
    async fn unrecognized_console_command_replies_command_not_found() {
        let session = test_session("Alice", Access::default());
        let state = Arc::<SharedState>::default();
        let request = request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "/Nope")],
        );

        let reply = session
            .on_chat_send(&request, &state, &console_config())
            .await;
        let text = reply[0]
            .received(text_encoding::DEFAULT)
            .field(FieldId::Data)
            .unwrap()
            .as_string();
        assert_eq!(text, "\rcommand not found: Nope");
    }

    #[tokio::test]
    async fn console_access_update_is_applied_live_and_broadcast() {
        let mut session = test_session("Bob", Access::default());
        session.user_id = 2;
        let state = Arc::<SharedState>::default();
        let mut rx = state.register_recipient(2).await;

        session
            .apply_console_update(
                ConsoleUpdate {
                    access: Some(Access::BROADCAST),
                    access_extra: None,
                    icon: Some(7),
                    name: Some("Bobby".into()),
                },
                &state,
            )
            .await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(&*session.name, "Bobby");

        let push = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserChange push");
        assert_eq!(
            push.r#type(),
            TransactionType::Push(Opcode::NotifyChangeUser)
        );
        assert_eq!(
            push.received(text_encoding::DEFAULT)
                .field(FieldId::UserIconID)
                .unwrap()
                .as_u16(),
            7
        );
        assert_eq!(
            push.received(text_encoding::DEFAULT)
                .string(FieldId::UserName),
            Some("Bobby".to_string())
        );
    }

    #[tokio::test]
    async fn chat_membership_opcodes_denied_without_chat_private() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::CHAT_PRIVATE);
        let state = Arc::<SharedState>::default();

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, 1),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        assert!(session.try_invite_to_chat(&invite, &state).await.is_err());

        let join = request(Opcode::JoinChat, 1, vec![Field::u32(FieldId::ChatID, 1)]);
        assert!(session.on_join_chat(&join, &state).await.is_error());

        let part = request(Opcode::LeaveChat, 1, vec![Field::u32(FieldId::ChatID, 1)]);
        assert!(session.on_leave_chat(&part, &state).await[0].is_error());

        let decline = request(
            Opcode::RejectChatInvite,
            1,
            vec![Field::u32(FieldId::ChatID, 1)],
        );
        assert!(session.on_reject_chat_invite(&decline, &state).await[0].is_error());

        let subject = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, 1),
                Field::string(FieldId::ChatSubject, "Topic"),
            ],
        );
        assert!(
            session
                .try_set_chat_subject(&subject, &state)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn chat_room_create_invite_and_room_scoped_chat() {
        let encoding = text_encoding::DEFAULT;
        let creator = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 1,
                login: Box::default(),
                name: "Alice".into(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: Box::default(),
                name: "Bob".into(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;

        let mut bob_rx = state.register_recipient(2).await;
        let create = request(
            Opcode::InviteNewChat,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let reply = creator.on_invite_new_chat(&create, &state).await.remove(0);
        let chat_id = reply
            .received(encoding)
            .field(FieldId::ChatID)
            .unwrap()
            .as_u32();

        let invite = bob_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected an InviteToChat push");
        assert_eq!(
            invite
                .received(encoding)
                .field(FieldId::ChatID)
                .unwrap()
                .as_u32(),
            chat_id
        );

        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let join_reply = bob.on_join_chat(&join, &state).await;
        let user_list_ids: Vec<u16> = join_reply
            .received(encoding)
            .fields(FieldId::UserNameWithInfo)
            .map(|f| {
                UserNameWithInfo::decode(f.as_bytes(), encoding)
                    .unwrap()
                    .user_id
            })
            .collect();
        assert_eq!(
            user_list_ids,
            vec![1, 2],
            "should list members in join order, not reversed"
        );
        assert!(join_reply.field(FieldId::ChatSubject).is_none());

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members.len(), 2);

        // Room chat goes via targeted delivery, not the public broadcast.
        let mut creator_rx = state.register_recipient(1).await;
        let room_chat = request(
            Opcode::ChatSend,
            2,
            vec![
                Field::string(FieldId::Data, "room only"),
                Field::u32(FieldId::ChatID, chat_id),
            ],
        );
        creator
            .on_chat_send(&room_chat, &state, &ServerConfig::default())
            .await;

        let delivered = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a room-chat push");
        assert_eq!(
            delivered.received(encoding).string(FieldId::Data).unwrap(),
            "\r        Alice:  room only"
        );
        assert!(
            creator_rx.try_recv().is_err(),
            "must not also broadcast room chat publicly"
        );
    }

    #[tokio::test]
    async fn leave_chat_of_the_main_room_does_nothing() {
        let state = Arc::<SharedState>::default();
        state.add_chat_member(0, 1).await;
        state.add_chat_member(0, 2).await;
        let mut creator_rx = state.register_recipient(1).await;
        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };

        let part = request(Opcode::LeaveChat, 1, vec![Field::u32(FieldId::ChatID, 0)]);
        assert!(bob.on_leave_chat(&part, &state).await.is_empty());
        assert!(creator_rx.try_recv().is_err());
        assert!(state.chat_room_members(0).await.unwrap().contains(&2));
    }

    #[tokio::test]
    async fn leave_chat_by_a_non_member_notifies_no_one() {
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;

        let mut creator_rx = state.register_recipient(1).await;
        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let part = request(
            Opcode::LeaveChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        assert!(bob.on_leave_chat(&part, &state).await.is_empty());
        assert!(creator_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn leave_chat_sends_no_reply_but_still_notifies_remaining_members() {
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.add_chat_member(chat_id, 2).await;

        let mut creator_rx = state.register_recipient(1).await;
        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let part = request(
            Opcode::LeaveChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let replies = bob.on_leave_chat(&part, &state).await;

        assert!(replies.is_empty(), "leaving a chat room gets no reply");

        let notify = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a NotifyChatDeleteUser push");
        assert_eq!(
            notify
                .received(text_encoding::DEFAULT)
                .field(FieldId::ChatID)
                .unwrap()
                .as_u32(),
            chat_id
        );
        assert_eq!(
            notify
                .received(text_encoding::DEFAULT)
                .field(FieldId::UserID)
                .unwrap()
                .as_u16(),
            2
        );

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members, vec![1]);
    }

    #[tokio::test]
    async fn join_chat_lets_each_user_into_an_open_room_and_notifies_once() {
        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.set_chat_password(chat_id, "hunter2").await;
        let mut creator_rx = state.register_recipient(1).await;
        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );

        assert!(!bob.on_join_chat(&join, &state).await.is_error());
        assert!(creator_rx.try_recv().is_ok());
        assert!(!bob.on_join_chat(&join, &state).await.is_error());
        assert!(creator_rx.try_recv().is_err());
        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members, vec![1, 2]);
    }

    #[tokio::test]
    async fn join_chat_of_the_public_chat_or_a_closed_room_fails_when_open() {
        let bob = test_session("Bob", Access::all());
        let state = Arc::<SharedState>::default();
        for chat_id in [0, 999] {
            let join = request(
                Opcode::JoinChat,
                1,
                vec![Field::u32(FieldId::ChatID, chat_id)],
            );
            let reply = bob.on_join_chat(&join, &state).await;
            assert_eq!(
                reply
                    .received(text_encoding::DEFAULT)
                    .string(FieldId::ErrorText)
                    .as_deref(),
                Some("Cannot join private chat because it has been closed already.")
            );
        }
    }

    #[tokio::test]
    async fn joining_an_unprotected_room_without_an_invite_is_an_error() {
        let bob = test_session("Bob", Access::all());
        let state = state_with_policy(InvitedChatRooms);
        let chat_id = state.create_chat_room(1).await;

        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let reply = bob.on_join_chat(&join, &state).await;

        assert!(
            reply.is_error(),
            "a password isn't the only gate - no password set still requires an invite"
        );
    }

    #[tokio::test]
    async fn join_chat_sends_the_subject_only_when_the_room_has_one() {
        let bob = test_session("Bob", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );

        state.invite(chat_id, bob.user_id).await;
        let reply = bob.on_join_chat(&join, &state).await;
        assert!(reply.field(FieldId::ChatSubject).is_none());

        state.set_chat_subject(chat_id, "Topic").await;
        state.invite(chat_id, bob.user_id).await;
        let reply = bob.on_join_chat(&join, &state).await;
        assert_eq!(
            reply
                .received(text_encoding::DEFAULT)
                .string(FieldId::ChatSubject)
                .as_deref(),
            Some("Topic")
        );
    }

    /// Real clients encode small integer fields (including a chat room
    /// ID) in as few bytes as fit, so a request carrying a 2-byte
    /// [`myField_ChatID`] (114) must decode just as well as one using the
    /// full 4 bytes.
    ///
    /// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
    #[tokio::test]
    async fn join_chat_accepts_a_short_encoded_chat_id() {
        let bob = test_session("Bob", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, bob.user_id).await;

        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let reply = bob.on_join_chat(&join, &state).await;

        assert!(
            !reply.is_error(),
            "a short-encoded chat id should still be found"
        );
    }

    #[tokio::test]
    async fn joining_a_password_protected_room_requires_the_right_password() {
        let bob = test_session("Bob", Access::all());
        let state = state_with_policy(InvitedChatRooms);
        let chat_id = state.create_chat_room(1).await;
        state.set_chat_password(chat_id, "hunter2").await;

        let no_password = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        assert!(bob.on_join_chat(&no_password, &state).await.is_error());

        let wrong_password = request(
            Opcode::JoinChat,
            2,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::UserPassword, "nope"),
            ],
        );
        assert!(bob.on_join_chat(&wrong_password, &state).await.is_error());

        let right_password = request(
            Opcode::JoinChat,
            3,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::UserPassword, "hunter2"),
            ],
        );
        assert!(!bob.on_join_chat(&right_password, &state).await.is_error());
    }

    #[tokio::test]
    async fn a_pending_invite_bypasses_a_rooms_password() {
        let bob = test_session("Bob", Access::all());
        let state = state_with_policy(InvitedChatRooms);
        let chat_id = state.create_chat_room(1).await;
        state.set_chat_password(chat_id, "hunter2").await;
        state.invite(chat_id, bob.user_id).await;

        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        assert!(!bob.on_join_chat(&join, &state).await.is_error());
    }

    #[tokio::test]
    async fn set_chat_subject_lets_each_user_set_or_clear_an_open_rooms_subject() {
        let outsider = Session {
            user_id: 2,
            ..test_session("Eve", Access::all())
        };
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        let subject = |fields| request(Opcode::SetChatSubject, 1, fields);

        let set = subject(vec![
            Field::u32(FieldId::ChatID, chat_id),
            Field::string(FieldId::ChatSubject, "Topic"),
        ]);
        assert!(outsider.try_set_chat_subject(&set, &state).await.is_ok());
        assert_eq!(&*state.chat_room(chat_id).await.unwrap().subject, "Topic");

        let clear = subject(vec![
            Field::u32(FieldId::ChatID, chat_id),
            Field::string(FieldId::UserPassword, "hunter2"),
        ]);
        assert!(outsider.try_set_chat_subject(&clear, &state).await.is_ok());
        let room = state.chat_room(chat_id).await.unwrap();
        assert_eq!((&*room.subject, &*room.password), ("", ""));
    }

    #[tokio::test]
    async fn set_chat_subject_of_the_public_chat_fails_when_open() {
        let mut admin = test_session("Admin", Access::all());
        admin.access_extra.insert(AccessExtra::SET_SUBJECT);
        let state = Arc::<SharedState>::default();

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, 0),
                Field::string(FieldId::ChatSubject, "Nope"),
            ],
        );
        assert!(admin.try_set_chat_subject(&request, &state).await.is_err());
        assert_eq!(&*state.chat_room(0).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn setting_a_private_rooms_subject_requires_membership() {
        let outsider = test_session("Eve", Access::all());
        let outsider = Session {
            user_id: 2,
            ..outsider
        };
        let state = state_with_policy(InvitedChatRooms);
        let chat_id = state.create_chat_room(1).await;

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::ChatSubject, "Nope"),
            ],
        );
        let replies = outsider.try_set_chat_subject(&request, &state).await;
        assert!(replies.is_err());
        assert_eq!(&*state.chat_room(chat_id).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn a_member_can_set_a_private_rooms_password() {
        let encoding = text_encoding::DEFAULT;
        let member = test_session("Alice", Access::all());
        let state = state_with_policy(InvitedChatRooms);
        let chat_id = state.create_chat_room(member.user_id).await;
        let mut rx = state.register_recipient(member.user_id).await;

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::UserPassword, "hunter2"),
            ],
        );
        let replies = member.try_set_chat_subject(&request, &state).await;
        assert!(replies.is_ok());
        assert_eq!(
            &*state.chat_room(chat_id).await.unwrap().password,
            "hunter2"
        );

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatSubject push");
        assert_eq!(
            notify.received(encoding).string(FieldId::UserPassword),
            Some("hunter2".to_string())
        );
    }

    #[tokio::test]
    async fn set_chat_subject_cuts_the_subject_to_255_bytes() {
        let member = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(member.user_id).await;

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::ChatSubject, "s".repeat(300)),
            ],
        );
        let replies = member.try_set_chat_subject(&request, &state).await;
        assert!(replies.is_ok());
        assert_eq!(
            state.chat_room(chat_id).await.unwrap().subject.len(),
            state.policy().text_limits().chat_subject.unwrap()
        );
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_requires_set_subject() {
        let guest = test_session("Guest", Access::empty());
        let state = state_with_policy(InvitedChatRooms);

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, 0),
                Field::string(FieldId::ChatSubject, "Nope"),
            ],
        );
        let replies = guest.try_set_chat_subject(&request, &state).await;
        assert!(replies.is_err());
        assert_eq!(&*state.chat_room(0).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_broadcasts_to_everyone() {
        let encoding = text_encoding::DEFAULT;
        let mut admin = test_session("Admin", Access::empty());
        admin.access_extra.insert(AccessExtra::SET_SUBJECT);
        let state = state_with_policy(InvitedChatRooms);
        let mut rx = state.register_recipient(admin.user_id).await;

        let request = request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatID, 0),
                Field::string(FieldId::ChatSubject, "Welcome"),
            ],
        );
        let replies = admin.try_set_chat_subject(&request, &state).await;
        assert!(replies.is_ok());
        assert_eq!(&*state.chat_room(0).await.unwrap().subject, "Welcome");

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatSubject push");
        assert_eq!(
            notify
                .received(encoding)
                .field(FieldId::ChatID)
                .unwrap()
                .as_u32(),
            0
        );
        assert_eq!(
            notify
                .received(encoding)
                .field(FieldId::ChatID)
                .unwrap()
                .as_u32(),
            0
        );
        assert_eq!(
            notify.received(encoding).string(FieldId::ChatSubject),
            Some("Welcome".to_string())
        );
    }

    #[tokio::test]
    async fn invite_to_chat_invites_every_listed_user_and_skips_a_failed_one() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(alice.user_id).await;
        state.insert_user(test_user(2, "Bob")).await;
        state.insert_user(test_user(3, "Carol")).await;
        let mut bob_rx = state.register_recipient(2).await;
        let mut carol_rx = state.register_recipient(3).await;

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserID, 99),
                Field::u16(FieldId::UserID, 3),
            ],
        );
        let replies = alice.try_invite_to_chat(&invite, &state).await;

        assert!(replies.is_ok());
        for rx in [&mut bob_rx, &mut carol_rx] {
            let push = rx
                .try_recv()
                .ok()
                .and_then(RecipientMessage::into_push)
                .expect("expected an InviteToChat push");
            assert_eq!(push.r#type(), TransactionType::Push(Opcode::InviteToChat));
        }
    }

    #[tokio::test]
    async fn inviting_a_disconnected_user_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        let reply = alice.try_invite_to_chat(&invite, &state).await;

        assert!(reply.is_err());
    }

    #[tokio::test]
    async fn inviting_with_a_zero_chat_id_or_user_id_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;

        let missing_uid = request(
            Opcode::InviteToChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        assert!(
            alice
                .try_invite_to_chat(&missing_uid, &state)
                .await
                .is_err()
        );

        let zero_chat_id = request(
            Opcode::InviteToChat,
            2,
            vec![
                Field::u32(FieldId::ChatID, 0),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        assert!(
            alice
                .try_invite_to_chat(&zero_chat_id, &state)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn inviting_an_existing_member_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: Box::default(),
                name: "Bob".into(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let chat_id = state.create_chat_room(1).await;
        state.add_chat_member(chat_id, 2).await;

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        let reply = alice.try_invite_to_chat(&invite, &state).await;

        assert!(reply.is_err());
    }

    #[tokio::test]
    async fn inviting_an_already_invited_user_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: Box::default(),
                name: "Bob".into(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: Box::default(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        let reply = alice.try_invite_to_chat(&invite, &state).await;

        assert!(reply.is_err());
    }

    #[tokio::test]
    async fn chat_invite_bounces_and_is_not_delivered_when_target_refuses() {
        let encoding = text_encoding::DEFAULT;
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                refuses_chat: true,
                ..test_user(2, "Bob")
            })
            .await;
        let mut bob_rx = state.register_recipient(2).await;
        let chat_id = state.create_chat_room(1).await;

        let invite = request(
            Opcode::InviteToChat,
            1,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, 2),
            ],
        );
        let replies = alice.try_invite_to_chat(&invite, &state).await.unwrap();

        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0].received(encoding).string(FieldId::Data).unwrap(),
            "Bob does not accept private chats."
        );
        assert_eq!(
            replies[0]
                .received(encoding)
                .field(FieldId::Options)
                .unwrap()
                .as_u16(),
            u16::from(MsgOption::RefuseChat)
        );
        assert!(
            bob_rx.try_recv().is_err(),
            "a refusing target must not receive the invite"
        );
    }

    #[tokio::test]
    async fn invite_new_chat_with_no_user_creates_a_room_for_the_creator() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();

        let invite = request(Opcode::InviteNewChat, 1, vec![]);
        let [reply] = alice
            .on_invite_new_chat(&invite, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        let chat_id = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::ChatID)
            .map(Field::as_u32)
            .unwrap();
        let room = state.chat_room(chat_id).await.unwrap();
        assert_eq!(room.members, [alice.user_id]);
    }

    #[tokio::test]
    async fn invite_new_chat_bounces_and_is_not_delivered_when_target_refuses() {
        let alice = test_session("Alice", Access::all());
        let state = Arc::<SharedState>::default();
        state
            .insert_user(UserEntry {
                refuses_chat: true,
                ..test_user(2, "Bob")
            })
            .await;
        let mut bob_rx = state.register_recipient(2).await;

        let invite = request(
            Opcode::InviteNewChat,
            1,
            vec![Field::u16(FieldId::UserID, 2)],
        );
        let replies = alice.on_invite_new_chat(&invite, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::Data)
                .unwrap(),
            "Bob does not accept private chats."
        );
        assert!(!replies[1].is_error());
        let chat_id = replies[1]
            .received(text_encoding::DEFAULT)
            .field(FieldId::ChatID)
            .map(Field::as_u32)
            .unwrap();
        assert!(!state.is_invited(chat_id, 2).await);
        assert!(
            bob_rx.try_recv().is_err(),
            "a refusing target must not receive the invite"
        );
    }

    #[tokio::test]
    async fn joining_clears_the_invite() {
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;
        assert!(state.is_invited(chat_id, 2).await);

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let join = request(
            Opcode::JoinChat,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        bob.on_join_chat(&join, &state).await;

        assert!(!state.is_invited(chat_id, 2).await);
    }

    #[tokio::test]
    async fn declining_clears_the_invite() {
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let decline = request(
            Opcode::RejectChatInvite,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let replies = bob.on_reject_chat_invite(&decline, &state).await;

        assert!(replies.is_empty());
        assert!(!state.is_invited(chat_id, 2).await);
    }

    #[tokio::test]
    async fn declining_notifies_the_room() {
        let encoding = text_encoding::DEFAULT;
        let state = Arc::<SharedState>::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;

        let mut alice_rx = state.register_recipient(1).await;
        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let decline = request(
            Opcode::RejectChatInvite,
            1,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        );
        let replies = bob.on_reject_chat_invite(&decline, &state).await;
        assert!(replies.is_empty());

        let notify = alice_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push notifying the room");
        assert_eq!(notify.r#type(), TransactionType::Push(Opcode::ChatMsg));
        assert_eq!(
            notify.received(encoding).string(FieldId::Data).unwrap(),
            "\r<<< \"Bob\" declined invitation to chat >>>"
        );
        assert_eq!(
            notify
                .received(encoding)
                .field(FieldId::ChatID)
                .unwrap()
                .as_u32(),
            chat_id
        );
    }
}
