//! Chat relay, console commands, and private chat rooms.

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::FieldId;
use hotline_proto::{Field, Transaction};

use crate::account::AccessExtra;
use crate::console::{ChatCommandActor, ConsoleReply};
use crate::server::ServerConfig;
use crate::state::SharedState;

use super::connection::Session;

impl Session {
    /// `myTran_ChatSend` (105): public or (if `ChatId` is present) private-room
    /// chat, gated on `SEND_CHAT`. A message that starts with the console
    /// prefix runs as a console command instead.
    pub(super) async fn on_chat(
        &self,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        let encoding = self.encoding();
        let Some(message) = tx.string(FieldId::Text, encoding) else {
            return Vec::new();
        };

        if let Err(reason) = self.access.require(Access::SEND_CHAT) {
            // Sent only to the denied client; CHAT_SEND has no reply mechanism.
            return vec![Transaction::push(
                Opcode::ServerMsg,
                vec![Field::string(FieldId::Text, reason)],
            )];
        }

        let chat_id = tx.field(FieldId::ChatId).and_then(Field::as_u32);
        if let Some((prefix, command_line)) = config
            .console_prefix
            .and_then(|prefix| Some((prefix, message.strip_prefix(prefix)?)))
        {
            return self
                .on_console_command(prefix, command_line, chat_id, state)
                .await;
        }

        let style = tx.field(FieldId::Style).and_then(Field::as_u16);

        let prefix = match style == Some(1) {
            true => format!("\r *** {} ", self.name),
            false => format!("\r{:>13.13}:  ", self.name),
        };
        let text = format!("\r{message}").replace('\r', &prefix);
        let mut fields = vec![Field::string(FieldId::Text, text)];
        if let Some(chat_id) = chat_id {
            fields.push(Field::u32(FieldId::ChatId, chat_id));
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
            access_extra: self.access_extra,
            prefix,
        };
        let reply = state
            .run_console_command(&command.to_ascii_lowercase(), args.trim_start(), &actor)
            .await
            .unwrap_or_else(|| ConsoleReply::Chat(format!("command not found: {command}")));
        match reply {
            ConsoleReply::Silent => Vec::new(),
            ConsoleReply::Chat(text) => {
                let mut fields = vec![Field::string(FieldId::Text, format!("\r{text}"))];
                if let Some(chat_id) = chat_id {
                    fields.push(Field::u32(FieldId::ChatId, chat_id));
                }
                vec![Transaction::push(Opcode::ChatMsg, fields)]
            }
            ConsoleReply::Message(text) => vec![Transaction::push(
                Opcode::ServerMsg,
                vec![
                    Field::u16(FieldId::UserId, self.user_id),
                    Field::string(FieldId::Text, text),
                    Field::string(FieldId::Name, ConsoleReply::SENDER),
                ],
            )],
        }
    }

    /// 112: create a private chat room and invite its target users.
    pub(super) async fn on_chat_create(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Transaction {
        if let Err(reason) = self.access.require(Access::CREATE_CHAT) {
            return tx.reply_error(reason);
        }
        let target_ids: Vec<u16> = tx
            .fields(FieldId::UserId)
            .filter_map(Field::as_u16)
            .collect();
        if target_ids.is_empty() {
            return tx.reply_error("missing user id");
        }

        let chat_id = state.create_chat_room(self.user_id).await;
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);
        for target_id in target_ids {
            if target_id == self.user_id {
                continue;
            }
            if let Err(reason) = state.require_connected_user(target_id).await {
                tracing::debug!(target_id, reason, "skipping undeliverable chat invite");
                continue;
            }
            state.invite(chat_id, target_id).await;
            state
                .send_to(
                    target_id,
                    Transaction::push(
                        Opcode::ChatInvite,
                        vec![
                            Field::u32(FieldId::ChatId, chat_id),
                            Field::u16(FieldId::UserId, self.user_id),
                            Field::string(FieldId::Name, self.name.clone()),
                        ],
                    ),
                )
                .await;
        }
        tx.reply(vec![
            Field::u32(FieldId::ChatId, chat_id),
            Field::u16(FieldId::UserId, self.user_id),
            Field::u16(FieldId::Icon, icon),
            Field::u16(FieldId::Color, self.user_flags().bits()),
            Field::string(FieldId::Name, self.name.clone()),
        ])
    }

    /// 113: invite another user to a room this session belongs to.
    pub(super) async fn on_chat_invite(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let chat_id = tx
            .field(FieldId::ChatId)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        let target_id = tx
            .field(FieldId::UserId)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        if chat_id == 0 || target_id == 0 {
            return vec![tx.reply_error("missing chat id or user id")];
        }
        if let Err(reason) = state
            .invite_to_chat_room(chat_id, self.user_id, target_id)
            .await
        {
            return vec![tx.reply_error(reason)];
        }

        let mut replies = Vec::new();
        if let Some(target) = state.user(target_id).await {
            if let Some(bounce) = self.refusal_bounce(target_id, &target, true) {
                replies.push(bounce);
            }
            if !target.refuses_chat {
                state
                    .send_to(
                        target_id,
                        Transaction::push(
                            Opcode::ChatInvite,
                            vec![
                                Field::u32(FieldId::ChatId, chat_id),
                                Field::u16(FieldId::UserId, self.user_id),
                                Field::string(FieldId::Name, self.name.clone()),
                            ],
                        ),
                    )
                    .await;
            }
        }
        replies.push(tx.reply(vec![]));
        replies
    }

    /// 115: join a room, accepting an invitation or supplying its password.
    pub(super) async fn on_chat_join(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return tx.reply_error(reason);
        }
        let Some(chat_id) = tx.field(FieldId::ChatId).and_then(Field::as_u32) else {
            return tx.reply_error("missing chat id");
        };
        let encoding = self.encoding();
        let password = tx.string(FieldId::Password, encoding).unwrap_or_default();
        if let Err(reason) = state.join_chat_room(chat_id, self.user_id, &password).await {
            return tx.reply_error(reason);
        }
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);

        let notify = Transaction::push(
            Opcode::ChatUserChange,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, self.user_id),
                Field::u16(FieldId::Icon, icon),
                Field::u16(FieldId::Color, self.user_flags().bits()),
                Field::string(FieldId::Name, self.name.clone()),
            ],
        );

        let room = state.chat_room(chat_id).await.unwrap_or_default();
        let mut fields = Vec::new();
        for member_id in room.members {
            if member_id != self.user_id {
                state.send_to(member_id, notify.clone()).await;
            }
            if let Some(entry) = state.user(member_id).await {
                fields.push(Field::bytes(
                    FieldId::UserList,
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
        fields.push(Field::string(FieldId::ChatSubject, room.subject));
        tx.reply(fields)
    }

    /// 116: leave a chat room.
    pub(super) async fn on_chat_part(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let Some(chat_id) = tx.field(FieldId::ChatId).and_then(Field::as_u32) else {
            return vec![tx.reply_error("missing chat id")];
        };
        let remaining = state.chat_room_members(chat_id).await.unwrap_or_default();
        state.leave_chat_room(chat_id, self.user_id).await;

        let notify = Transaction::push(
            Opcode::ChatUserPart,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, self.user_id),
            ],
        );
        for member in remaining.into_iter().filter(|&m| m != self.user_id) {
            state.send_to(member, notify.clone()).await;
        }
        vec![]
    }

    /// 114: decline a chat invitation.
    pub(super) async fn on_chat_decline(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        if let Some(chat_id) = tx.field(FieldId::ChatId).and_then(Field::as_u32) {
            state.invite_declined(chat_id, self.user_id).await;
            if let Some(members) = state.chat_room_members(chat_id).await {
                let notify = Transaction::push(
                    Opcode::ChatMsg,
                    vec![
                        Field::string(
                            FieldId::Text,
                            format!("\r<<< \"{}\" declined invitation to chat >>>", self.name),
                        ),
                        Field::u32(FieldId::ChatId, chat_id),
                    ],
                );
                for member in members {
                    state.send_to(member, notify.clone()).await;
                }
            }
        }
        vec![]
    }

    /// 120: set a chat room's subject and/or its join password.
    pub(super) async fn on_chat_subject(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let Some(chat_id) = tx.field(FieldId::ChatId).and_then(Field::as_u32) else {
            return vec![tx.reply_error("missing chat id")];
        };
        let encoding = self.encoding();
        let subject = tx.string(FieldId::ChatSubject, encoding);
        let password = tx.string(FieldId::Password, encoding);
        if subject.is_none() && password.is_none() {
            return vec![tx.reply_error("missing subject or password")];
        }

        let is_global = chat_id == 0;
        if let Err(reason) = state
            .authorize_chat_subject_change(chat_id, self.user_id, is_global, self.access_extra)
            .await
        {
            return vec![tx.reply_error(reason)];
        }

        if let Some(subject) = subject {
            state.set_chat_subject(chat_id, subject.clone()).await;
            self.broadcast_chat_field(
                state,
                chat_id,
                is_global,
                Field::string(FieldId::ChatSubject, subject),
            )
            .await;
        }
        if let Some(password) = password {
            state.set_chat_password(chat_id, password.clone()).await;
            self.broadcast_chat_field(
                state,
                chat_id,
                is_global,
                Field::string(FieldId::Password, password),
            )
            .await;
        }
        vec![tx.reply(vec![])]
    }

    /// Send a `ChatSubject` push carrying one changed field (subject or
    /// password) to a room's members, or everyone for the main room.
    pub(super) async fn broadcast_chat_field(
        &self,
        state: &SharedState,
        chat_id: u32,
        is_global: bool,
        field: Field,
    ) {
        let notify = Transaction::push(
            Opcode::NotifyChatSubject,
            vec![Field::u32(FieldId::ChatId, chat_id), field],
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::connection::MsgOption;
    use crate::state::{ConsoleUpdate, RecipientMessage, UserEntry};
    use hotline_proto::opcode::TransactionType;
    use std::time::Instant;

    use crate::session::test_support::*;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn chat_relay_starts_with_cr_for_scrollback_clients() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "hi")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session
            .on_chat(&request, &state, &ServerConfig::default())
            .await
            .is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(FieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn chat_relay_uses_emote_format_for_style_1() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Text, "waves"),
                Field::u16(FieldId::Style, 1),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session
            .on_chat(&request, &state, &ServerConfig::default())
            .await
            .is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(FieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r *** Alice waves");
    }

    #[tokio::test]
    async fn chat_relay_prefixes_every_line_of_a_multiline_message() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "line one\rline two")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session
            .on_chat(&request, &state, &ServerConfig::default())
            .await
            .is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(FieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r        Alice:  line one\r        Alice:  line two");
    }

    #[tokio::test]
    async fn chat_relay_truncates_a_long_name_keeping_its_start() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("ANameThatIsWayTooLong", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "hi")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session
            .on_chat(&request, &state, &ServerConfig::default())
            .await
            .is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(FieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\rANameThatIsWa:  hi");
    }

    #[tokio::test]
    async fn chat_relay_keeps_normal_format_for_non_emote_style() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Text, "hi"),
                Field::u16(FieldId::Style, 0),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session
            .on_chat(&request, &state, &ServerConfig::default())
            .await
            .is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(FieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn chat_starting_with_slash_dispatches_as_a_console_command_not_a_relay() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "/users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session.on_chat(&request, &state, &console_config()).await;

        // `/users` answers with a private message from `hxd`, like mhxd.
        assert!(rx.try_recv().is_err());
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
        assert_eq!(
            reply[0]
                .string(FieldId::Name, text_encoding::DEFAULT)
                .as_deref(),
            Some("hxd")
        );
        assert_eq!(
            reply[0].field(FieldId::UserId).and_then(Field::as_u16),
            Some(session.user_id)
        );
    }

    /// A command typed in a private room replies in that room.
    #[tokio::test]
    async fn console_reply_in_a_private_room_carries_its_chat_id() {
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![
                Field::string(FieldId::Text, "/nope"),
                Field::u32(FieldId::ChatId, 7),
            ],
        );

        let reply = session.on_chat(&request, &state, &console_config()).await;

        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ChatMsg));
        assert_eq!(
            reply[0].field(FieldId::ChatId).and_then(Field::as_u32),
            Some(7)
        );
    }

    /// With no console prefix configured, a would-be command is just chat.
    #[tokio::test]
    async fn chat_starting_with_slash_relays_as_chat_when_console_is_off() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "/users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session
            .on_chat(&request, &state, &ServerConfig::default())
            .await;

        assert!(reply.is_empty());
        let relayed = rx.try_recv().expect("relayed as ordinary chat");
        let RecipientMessage::Push(push) = relayed else {
            panic!("expected a chat push");
        };
        let text = push
            .field(FieldId::Text)
            .unwrap()
            .as_string(text_encoding::DEFAULT);
        assert!(text.contains("/users"), "{text}");
    }

    /// A different prefix dispatches; `/` then relays as ordinary chat.
    #[tokio::test]
    async fn console_prefix_is_configurable() {
        let mut session = test_session("Alice", Access::default());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = SharedState::default();
        let config = ServerConfig {
            console_prefix: Some('!'),
            ..ServerConfig::default()
        };
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "!users")],
        );

        let mut rx = state.register_recipient(1).await;
        let reply = session.on_chat(&request, &state, &config).await;

        assert!(rx.try_recv().is_err(), "a command must not relay as chat");
        assert_eq!(reply.len(), 1);
        assert_eq!(reply[0].r#type(), TransactionType::Push(Opcode::ServerMsg));
    }

    #[tokio::test]
    async fn console_command_is_denied_without_send_chat() {
        // SEND_CHAT is required even though /users itself needs only MANAGE_USERS.
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.insert(AccessExtra::MANAGE_USERS);
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "/users")],
        );

        let reply = session.on_chat(&request, &state, &console_config()).await;
        let text = reply[0]
            .field(FieldId::Text)
            .unwrap()
            .as_string(text_encoding::DEFAULT);
        assert!(text.contains("not allowed"));
    }

    #[tokio::test]
    async fn unrecognized_console_command_replies_command_not_found() {
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "/nope")],
        );

        let reply = session.on_chat(&request, &state, &console_config()).await;
        let text = reply[0]
            .field(FieldId::Text)
            .unwrap()
            .as_string(text_encoding::DEFAULT);
        assert_eq!(text, "\rcommand not found: nope");
    }

    #[tokio::test]
    async fn console_access_update_is_applied_live_and_broadcast() {
        let mut session = test_session("Bob", Access::default());
        session.user_id = 2;
        let state = SharedState::default();
        let mut rx = state.register_recipient(2).await;

        session
            .apply_console_update(
                ConsoleUpdate {
                    access: Some(Access::BROADCAST),
                    access_extra: None,
                    icon: Some(7),
                    name: Some("Bobby".to_string()),
                },
                &state,
            )
            .await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(session.name, "Bobby");

        let push = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserChange push");
        assert_eq!(
            push.r#type(),
            TransactionType::Push(Opcode::NotifyChangeUser)
        );
        assert_eq!(push.field(FieldId::Icon).unwrap().as_u16(), Some(7));
        assert_eq!(
            push.string(FieldId::Name, text_encoding::DEFAULT),
            Some("Bobby".to_string())
        );
    }

    #[tokio::test]
    async fn chat_membership_opcodes_denied_without_chat_private() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::CHAT_PRIVATE);
        let state = SharedState::default();

        let invite = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![
                Field::u32(FieldId::ChatId, 1),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        assert!(session.on_chat_invite(&invite, &state).await[0].is_error());

        let join = Transaction::request(Opcode::ChatJoin, 1, vec![Field::u32(FieldId::ChatId, 1)]);
        assert!(session.on_chat_join(&join, &state).await.is_error());

        let part = Transaction::request(Opcode::ChatPart, 1, vec![Field::u32(FieldId::ChatId, 1)]);
        assert!(session.on_chat_part(&part, &state).await[0].is_error());

        let decline =
            Transaction::request(Opcode::ChatDecline, 1, vec![Field::u32(FieldId::ChatId, 1)]);
        assert!(session.on_chat_decline(&decline, &state).await[0].is_error());

        let subject = Transaction::request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatId, 1),
                Field::string(FieldId::ChatSubject, "Topic"),
            ],
        );
        assert!(session.on_chat_subject(&subject, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn chat_room_create_invite_and_room_scoped_chat() {
        let encoding = text_encoding::DEFAULT;
        let creator = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 1,
                login: String::new(),
                name: "Alice".to_string(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: String::new(),
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
                login: String::new(),
                name: "Bob".to_string(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: String::new(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;

        let mut bob_rx = state.register_recipient(2).await;
        let create =
            Transaction::request(Opcode::ChatCreate, 1, vec![Field::u16(FieldId::UserId, 2)]);
        let reply = creator.on_chat_create(&create, &state).await;
        let chat_id = reply.field(FieldId::ChatId).unwrap().as_u32().unwrap();

        let invite = bob_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatInvite push");
        assert_eq!(
            invite.field(FieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );

        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let join = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let join_reply = bob.on_chat_join(&join, &state).await;
        let user_list_ids: Vec<u16> = join_reply
            .fields(FieldId::UserList)
            .map(|f| {
                UserNameWithInfo::decode(&f.as_bytes(encoding), encoding)
                    .unwrap()
                    .user_id
            })
            .collect();
        assert_eq!(
            user_list_ids,
            vec![1, 2],
            "should list members in join order, not reversed"
        );
        assert_eq!(
            join_reply.string(FieldId::ChatSubject, encoding).unwrap(),
            ""
        );

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members.len(), 2);

        // Room chat goes via targeted delivery, not the public broadcast.
        let mut creator_rx = state.register_recipient(1).await;
        let room_chat = Transaction::request(
            Opcode::ChatSend,
            2,
            vec![
                Field::string(FieldId::Text, "room only"),
                Field::u32(FieldId::ChatId, chat_id),
            ],
        );
        creator
            .on_chat(&room_chat, &state, &ServerConfig::default())
            .await;

        let delivered = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a room-chat push");
        assert_eq!(
            delivered.string(FieldId::Text, encoding).unwrap(),
            "\r        Alice:  room only"
        );
        assert!(
            creator_rx.try_recv().is_err(),
            "must not also broadcast room chat publicly"
        );
    }

    #[tokio::test]
    async fn chat_part_sends_no_reply_but_still_notifies_remaining_members() {
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.add_chat_member(chat_id, 2).await;

        let mut creator_rx = state.register_recipient(1).await;
        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let part = Transaction::request(
            Opcode::ChatPart,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let replies = bob.on_chat_part(&part, &state).await;

        assert!(replies.is_empty(), "leaving a chat room gets no reply");

        let notify = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatUserPart push");
        assert_eq!(
            notify.field(FieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );
        assert_eq!(notify.field(FieldId::UserId).unwrap().as_u16(), Some(2));

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members, vec![1]);
    }

    #[tokio::test]
    async fn joining_an_unprotected_room_without_an_invite_is_an_error() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let join = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let reply = bob.on_chat_join(&join, &state).await;

        assert!(
            reply.is_error(),
            "a password isn't the only gate - no password set still requires an invite"
        );
    }

    /// Real clients encode small integer fields (including a chat room
    /// ID) in as few bytes as fit, so a request carrying a 2-byte
    /// `ChatId` must decode just as well as one using the full 4 bytes.
    #[tokio::test]
    async fn chat_join_accepts_a_short_encoded_chat_id() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, bob.user_id).await;

        let join = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let reply = bob.on_chat_join(&join, &state).await;

        assert!(
            !reply.is_error(),
            "a short-encoded chat id should still be found"
        );
    }

    #[tokio::test]
    async fn joining_a_password_protected_room_requires_the_right_password() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state
            .set_chat_password(chat_id, "hunter2".to_string())
            .await;

        let no_password = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        assert!(bob.on_chat_join(&no_password, &state).await.is_error());

        let wrong_password = Transaction::request(
            Opcode::ChatJoin,
            2,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::string(FieldId::Password, "nope"),
            ],
        );
        assert!(bob.on_chat_join(&wrong_password, &state).await.is_error());

        let right_password = Transaction::request(
            Opcode::ChatJoin,
            3,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::string(FieldId::Password, "hunter2"),
            ],
        );
        assert!(!bob.on_chat_join(&right_password, &state).await.is_error());
    }

    #[tokio::test]
    async fn a_pending_invite_bypasses_a_rooms_password() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state
            .set_chat_password(chat_id, "hunter2".to_string())
            .await;
        state.invite(chat_id, bob.user_id).await;

        let join = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        assert!(!bob.on_chat_join(&join, &state).await.is_error());
    }

    #[tokio::test]
    async fn setting_a_private_rooms_subject_requires_membership() {
        let outsider = test_session("Eve", Access::all());
        let outsider = Session {
            user_id: 2,
            ..outsider
        };
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let request = Transaction::request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::string(FieldId::ChatSubject, "Nope"),
            ],
        );
        let replies = outsider.on_chat_subject(&request, &state).await;
        assert!(replies[0].is_error());
        assert_eq!(state.chat_room(chat_id).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn a_member_can_set_a_private_rooms_password() {
        let encoding = text_encoding::DEFAULT;
        let member = test_session("Alice", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(member.user_id).await;
        let mut rx = state.register_recipient(member.user_id).await;

        let request = Transaction::request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::string(FieldId::Password, "hunter2"),
            ],
        );
        let replies = member.on_chat_subject(&request, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(state.chat_room(chat_id).await.unwrap().password, "hunter2");

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatSubject push");
        assert_eq!(
            notify.string(FieldId::Password, encoding),
            Some("hunter2".to_string())
        );
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_requires_set_subject() {
        let guest = test_session("Guest", Access::empty());
        let state = SharedState::default();

        let request = Transaction::request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatId, 0),
                Field::string(FieldId::ChatSubject, "Nope"),
            ],
        );
        let replies = guest.on_chat_subject(&request, &state).await;
        assert!(replies[0].is_error());
        assert_eq!(state.chat_room(0).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_broadcasts_to_everyone() {
        let encoding = text_encoding::DEFAULT;
        let mut admin = test_session("Admin", Access::empty());
        admin.access_extra.insert(AccessExtra::SET_SUBJECT);
        let state = SharedState::default();
        let mut rx = state.register_recipient(admin.user_id).await;

        let request = Transaction::request(
            Opcode::SetChatSubject,
            1,
            vec![
                Field::u32(FieldId::ChatId, 0),
                Field::string(FieldId::ChatSubject, "Welcome"),
            ],
        );
        let replies = admin.on_chat_subject(&request, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(state.chat_room(0).await.unwrap().subject, "Welcome");

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatSubject push");
        assert_eq!(notify.field(FieldId::ChatId).unwrap().as_u32(), Some(0));
        assert_eq!(notify.field(FieldId::ChatId).unwrap().as_u32(), Some(0));
        assert_eq!(
            notify.string(FieldId::ChatSubject, encoding),
            Some("Welcome".to_string())
        );
    }

    #[tokio::test]
    async fn inviting_a_disconnected_user_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let invite = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        let reply = alice.on_chat_invite(&invite, &state).await;

        assert!(reply[0].is_error());
    }

    #[tokio::test]
    async fn inviting_with_a_zero_chat_id_or_user_id_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let missing_uid = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        assert!(alice.on_chat_invite(&missing_uid, &state).await[0].is_error());

        let zero_chat_id = Transaction::request(
            Opcode::ChatInvite,
            2,
            vec![
                Field::u32(FieldId::ChatId, 0),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        assert!(alice.on_chat_invite(&zero_chat_id, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn inviting_an_existing_member_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: String::new(),
                name: "Bob".to_string(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: String::new(),
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

        let invite = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        let reply = alice.on_chat_invite(&invite, &state).await;

        assert!(reply[0].is_error());
    }

    #[tokio::test]
    async fn inviting_an_already_invited_user_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: String::new(),
                name: "Bob".to_string(),
                icon: 0,
                client_version: None,
                access: Access::all(),
                access_extra: AccessExtra::default(),
                account_name: String::new(),
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

        let invite = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        let reply = alice.on_chat_invite(&invite, &state).await;

        assert!(reply[0].is_error());
    }

    #[tokio::test]
    async fn chat_invite_bounces_and_is_not_delivered_when_target_refuses() {
        let encoding = text_encoding::DEFAULT;
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                refuses_chat: true,
                ..test_user(2, "Bob")
            })
            .await;
        let mut bob_rx = state.register_recipient(2).await;
        let chat_id = state.create_chat_room(1).await;

        let invite = Transaction::request(
            Opcode::ChatInvite,
            1,
            vec![
                Field::u32(FieldId::ChatId, chat_id),
                Field::u16(FieldId::UserId, 2),
            ],
        );
        let replies = alice.on_chat_invite(&invite, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].string(FieldId::Text, encoding).unwrap(),
            "Bob does not accept private chats."
        );
        assert_eq!(
            replies[0].field(FieldId::Options).unwrap().as_u16(),
            Some(MsgOption::RefuseChat.into())
        );
        assert!(!replies[1].is_error());
        assert!(
            bob_rx.try_recv().is_err(),
            "a refusing target must not receive the invite"
        );
    }

    #[tokio::test]
    async fn joining_clears_the_invite() {
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;
        assert!(state.is_invited(chat_id, 2).await);

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let join = Transaction::request(
            Opcode::ChatJoin,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        bob.on_chat_join(&join, &state).await;

        assert!(!state.is_invited(chat_id, 2).await);
    }

    #[tokio::test]
    async fn declining_clears_the_invite() {
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let decline = Transaction::request(
            Opcode::ChatDecline,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let replies = bob.on_chat_decline(&decline, &state).await;

        assert!(replies.is_empty());
        assert!(!state.is_invited(chat_id, 2).await);
    }

    #[tokio::test]
    async fn declining_notifies_the_room() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invite(chat_id, 2).await;

        let mut alice_rx = state.register_recipient(1).await;
        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let decline = Transaction::request(
            Opcode::ChatDecline,
            1,
            vec![Field::u32(FieldId::ChatId, chat_id)],
        );
        let replies = bob.on_chat_decline(&decline, &state).await;
        assert!(replies.is_empty());

        let notify = alice_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push notifying the room");
        assert_eq!(notify.r#type(), TransactionType::Push(Opcode::ChatMsg));
        assert_eq!(
            notify.string(FieldId::Text, encoding).unwrap(),
            "\r<<< \"Bob\" declined invitation to chat >>>"
        );
        assert_eq!(
            notify.field(FieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );
    }
}
