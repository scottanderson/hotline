use std::time::Instant;

use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::consts::AccessPrivileges;
use hotline_proto::field_id::{ClientFieldId, ServerFieldId};
use hotline_proto::opcode::{ClientOpcode, Opcode, ServerOpcode};
use hotline_proto::types::{UserFlags, UserNameWithInfo};
use hotline_proto::{ClientHello, Field, ServerHello, Transaction};

use crate::error::Result;
use crate::server::ServerConfig;
use crate::state::{SharedState, UserEntry};

/// A single client connection: handshake state plus the transaction
/// dispatch loop. Not `Clone`, one per socket.
pub struct Session {
    user_id: u16,
    name: String,
    /// Privileges granted at login.
    access: AccessPrivileges,
}

impl Session {
    /// Run the full lifecycle of one connection: handshake, then dispatch
    /// loop until the client disconnects.
    pub async fn run(
        mut stream: TcpStream,
        state: SharedState,
        config: ServerConfig,
    ) -> Result<()> {
        Self::handshake(&mut stream).await?;

        let user_id = state.next_user_id();
        let mut session = Session {
            user_id,
            name: format!("guest{user_id}"),
            // Empty until on_login succeeds.
            access: AccessPrivileges::empty(),
        };

        let (mut writer, mut reader) = Framed::new(stream, TransactionCodec::for_server()).split();
        let mut broadcast_rx = state.subscribe();
        let mut targeted_rx = state.subscribe_targeted();
        let mut announce_rx = state.subscribe_announcements();

        loop {
            tokio::select! {
                // Traffic from this client.
                frame = reader.next() => {
                    let Some(tx) = frame.transpose()? else { break }; // clean EOF
                    state.touch_user(session.user_id).await;
                    for reply in session.handle(&tx, &state, &config).await {
                        writer.send(reply).await?;
                    }
                }
                // Broadcast traffic (public chat, user list changes)
                // fanned out from every session.
                msg = broadcast_rx.recv() => {
                    match msg {
                        Ok(tx) => {
                            writer.send(tx).await?;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                // Traffic addressed to this user specifically (private
                // messages, chat-room traffic, a kick notice).
                msg = targeted_rx.recv() => {
                    match msg {
                        Ok(targeted) if targeted.target_user_id == session.user_id => {
                            let is_kick =
                                targeted.tx.opcode() == Opcode::Push(ServerOpcode::Politequit);
                            writer.send(targeted.tx).await?;
                            if is_kick {
                                break; // the whole point of Politequit is disconnection
                            }
                        }
                        Ok(_) => continue, // addressed to someone else
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                // Everyone-except-one traffic (currently only the login
                // join notification - see `SharedState::announce`).
                msg = announce_rx.recv() => {
                    match msg {
                        Ok(a) if a.exclude_user_id != session.user_id => {
                            writer.send(a.tx).await?;
                        }
                        Ok(_) => continue, // this is the join announcement about myself
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            }
        }

        state.remove_user(session.user_id).await;
        for (chat_id, remaining) in state.leave_all_chat_rooms(session.user_id).await {
            let notify = Transaction::push(
                ServerOpcode::ChatUserPart,
                vec![
                    Field::u32(ServerFieldId::ChatId, chat_id),
                    Field::u16(ServerFieldId::UserId, session.user_id),
                ],
            );
            for member in remaining {
                state.send_to(member, notify.clone());
            }
        }
        let leave = Transaction::push(
            ServerOpcode::UserPart,
            vec![Field::u16(ServerFieldId::UserId, session.user_id)],
        );
        state.publish(leave);
        Ok(())
    }

    async fn handshake(stream: &mut TcpStream) -> Result<()> {
        let mut buf = [0u8; 12];
        stream.read_exact(&mut buf).await?;
        ClientHello::decode(&buf)?;
        stream.write_all(&ServerHello::OK.encode()).await?;
        Ok(())
    }

    /// Handle one request, returning zero or more transactions to send
    /// back.
    async fn handle(
        &mut self,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        let Opcode::Request(opcode) = tx.opcode() else {
            tracing::debug!(type_ = tx.type_(), "not a recognized client request");
            return vec![tx.reply_error("not implemented")];
        };

        match opcode {
            /* 101 */ ClientOpcode::NewsfileGet => self.on_newsfile_get(tx, state).await,
            /* 103 */ ClientOpcode::NewsfilePost => self.on_newsfile_post(tx, state).await,
            /* 105 */ ClientOpcode::Chat => self.on_chat(tx, state).await,
            /* 107 */ ClientOpcode::Login => self.on_login(tx, config).await,
            /* 108 */ ClientOpcode::Msg => self.on_private_msg(tx, state).await,
            /* 110 */ ClientOpcode::UserKick => self.on_user_kick(tx, state).await,
            /* 112 */ ClientOpcode::ChatCreate => vec![self.on_chat_create(tx, state).await],
            /* 113 */ ClientOpcode::ChatInvite => vec![self.on_chat_invite(tx, state).await],
            /* 114 */ ClientOpcode::ChatDecline => self.on_chat_decline(tx, state).await,
            /* 115 */ ClientOpcode::ChatJoin => vec![self.on_chat_join(tx, state).await],
            /* 116 */ ClientOpcode::ChatPart => vec![self.on_chat_part(tx, state).await],
            /* 120 */ ClientOpcode::ChatSubject => self.on_chat_subject(tx, state).await,
            /* 121 */ ClientOpcode::Agreed => self.on_agreed(tx, state).await,
            /* 200 */ ClientOpcode::FileList => self.on_get_file_name_list(tx, state).await,
            /* 300 */ ClientOpcode::UserGetlist => self.on_user_get_list(tx, state).await,
            /* 303 */ ClientOpcode::UserGetinfo => self.on_user_get_info(tx, state).await,
            /* 304 */ ClientOpcode::UserChange => self.on_user_change(tx, state).await,
            /* 500 */ ClientOpcode::Ping => vec![tx.reply(vec![])],
            other => {
                tracing::debug!(?other, "unhandled opcode");
                vec![tx.reply_error("not implemented")]
            }
        }
    }

    /// `Err` names the missing privilege if `self.access` doesn't hold
    /// `required`.
    fn require(&self, required: AccessPrivileges) -> std::result::Result<(), String> {
        self.access.contains(required).then_some(()).ok_or_else(|| {
            let name = required.flag_name().unwrap_or("this privilege");
            format!("You are not allowed to do that ({name}).")
        })
    }

    async fn on_login(&mut self, tx: &Transaction, config: &ServerConfig) -> Vec<Transaction> {
        // Both LOGIN and PASSWORD are XOR-0xFF obfuscated on the wire
        // (Hotline's `hl_code`).
        let login = tx.login();
        let password = tx.password();

        // No account database yet: "admin"/"admin" grants every privilege.
        self.access = match (login.as_deref(), password.as_deref()) {
            (Some("admin"), Some("admin")) => AccessPrivileges::all(),
            _ => config.default_access,
        };

        // Some clients embed name/icon in Login, not just a follow-up
        // SET_CLIENT_USER_INFO (304).
        if let Some(name) = tx.name() {
            self.name = name;
        }

        let mut fields = vec![Field::u16(ServerFieldId::Serverversion, 151)];
        if !config.server_name.is_empty() {
            fields.push(Field::string(
                ServerFieldId::Servername,
                config.server_name.clone(),
            ));
        }

        // The join broadcast and privilege grant wait for Agreed (121);
        // see on_agreed. This push tells the client what to show first.
        let agreement = if config.agreement.is_empty() {
            Transaction::push(
                ServerOpcode::Agreement,
                vec![Field::bytes(ServerFieldId::Noagreement, vec![0, 1])],
            )
        } else {
            Transaction::push(
                ServerOpcode::Agreement,
                vec![Field::string(ServerFieldId::Text, config.agreement.clone())],
            )
        };

        vec![tx.reply(fields), agreement]
    }

    /// Complete login after the client accepts the agreement (`HTLC_HDR_AGREED`, 121).
    async fn on_agreed(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Some(name) = tx.name() {
            self.name = name;
        }
        let icon = tx.icon().unwrap_or(0);
        // Not echoed back to the joiner.
        self.announce_user_change(icon, state, true).await;

        let self_entry = UserNameWithInfo {
            user_id: self.user_id,
            icon,
            flags: self.user_flags(),
            name: self.name.clone(),
        };

        vec![
            tx.reply(vec![]),
            // Privileges + own user-list entry, as a UserSelfinfo push.
            Transaction::push(
                ServerOpcode::UserSelfinfo,
                vec![
                    Field::u64(ServerFieldId::Access, self.access.bits()),
                    Field::bytes(ServerFieldId::UserList, self_entry.encode()),
                ],
            ),
        ]
    }

    async fn on_user_change(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Some(name) = tx.name() {
            self.name = name;
        }
        let icon = tx.icon().unwrap_or(0);
        // Later changes are self-inclusive, unlike the join announcement.
        self.announce_user_change(icon, state, false).await;
        Vec::new()
    }

    /// Register this session's current name/icon and broadcast a
    /// `UserChange` push. `exclude_self` skips echoing it to the sender.
    async fn announce_user_change(&self, icon: u16, state: &SharedState, exclude_self: bool) {
        state
            .insert_user(UserEntry {
                user_id: self.user_id,
                name: self.name.clone(),
                icon,
                access: self.access,
                last_active: Instant::now(),
            })
            .await;

        let notify = Transaction::push(
            ServerOpcode::UserChange,
            vec![
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::u16(ServerFieldId::Icon, icon),
                Field::string(ServerFieldId::Name, self.name.clone()),
                Field::u16(ServerFieldId::Color, self.user_flags()),
            ],
        );
        if exclude_self {
            state.announce(self.user_id, notify);
        } else {
            state.publish(notify);
        }
    }

    /// This session's own admin flag; idle is tracked per `UserEntry`.
    fn user_flags(&self) -> u16 {
        if self.access.contains(AccessPrivileges::DISCONNECT_USERS) {
            UserFlags::ADMIN.bits()
        } else {
            0
        }
    }

    /// 105: public or (if `ChatId` is present) private-room chat.
    async fn on_chat(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::SEND_CHAT) {
            // Sent only to the denied client; CHAT_SEND has no reply mechanism.
            return vec![Transaction::push(
                ServerOpcode::Msg,
                vec![Field::string(ServerFieldId::Text, reason)],
            )];
        }

        let Some(message) = tx.field(ClientFieldId::Text) else {
            return Vec::new();
        };
        let chat_id = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32);

        // Default chat_format: "\r%13.13s:  %s".
        let mut fields = vec![Field::string(
            ServerFieldId::Text,
            format!("\r{:>13}:  {}", self.name, message.as_string()),
        )];
        if let Some(chat_id) = chat_id {
            fields.push(Field::u32(ServerFieldId::ChatId, chat_id));
        }
        let relay = Transaction::push(ServerOpcode::Chat, fields);

        match chat_id {
            // Room chat: only members should see it.
            Some(chat_id) => {
                if let Some(members) = state.chat_room_members(chat_id).await {
                    for member in members {
                        state.send_to(member, relay.clone());
                    }
                }
            }
            // Public chat: everyone.
            None => state.publish(relay),
        }
        Vec::new()
    }

    /// 300
    async fn on_user_get_list(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
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
            .map(|entry| Field::bytes(ServerFieldId::UserList, entry.encode()))
            .collect();
        vec![tx.reply(fields)]
    }

    /// 303: another user's info text (name, icon, and a short
    /// privilege summary listing each held flag's
    /// [`AccessPrivileges::flag_name`]).
    async fn on_user_get_info(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::GET_USER_INFO) {
            return vec![tx.reply_error(reason)];
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let Some(target) = state.user(target_id).await else {
            return vec![tx.reply_error("no such user")];
        };

        let privileges = target
            .access
            .iter()
            .flat_map(AccessPrivileges::flag_name)
            .collect::<Vec<_>>()
            .join(", ");
        let text = format!(
            "{}{}\rPrivileges: {}",
            target.name,
            if target.access.contains(AccessPrivileges::DISCONNECT_USERS) {
                " (admin)"
            } else {
                ""
            },
            if privileges.is_empty() {
                "none"
            } else {
                &privileges
            }
        );

        vec![tx.reply(vec![
            Field::string(ServerFieldId::Name, target.name),
            Field::u16(ServerFieldId::UserId, target.user_id),
            Field::string(ServerFieldId::Text, text),
        ])]
    }

    /// 110: disconnect (and, per the `Options` kick-to-ban bit, in a
    /// fuller implementation ban) another user.
    async fn on_user_kick(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::DISCONNECT_USERS) {
            return vec![tx.reply_error(reason)];
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        if state
            .user(target_id)
            .await
            .is_some_and(|u| u.access.contains(AccessPrivileges::CANT_BE_DISCONNECTED))
        {
            return vec![tx.reply_error("that user can't be disconnected")];
        }

        state.send_to(
            target_id,
            Transaction::push(
                ServerOpcode::Politequit,
                vec![Field::string(
                    ServerFieldId::Text,
                    format!("You have been disconnected by {}.", self.name),
                )],
            ),
        );
        vec![tx.reply(vec![])]
    }

    /// 108: a private instant message to one other user.
    async fn on_private_msg(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::SEND_MSGS) {
            return vec![tx.reply_error(reason)];
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let Some(message) = tx.field(ClientFieldId::Text) else {
            return vec![tx.reply_error("missing message")];
        };

        state.send_to(
            target_id,
            Transaction::push(
                ServerOpcode::Msg,
                vec![
                    Field::u16(ServerFieldId::UserId, self.user_id),
                    Field::string(ServerFieldId::Name, self.name.clone()),
                    Field::string(ServerFieldId::Text, message.as_string()),
                ],
            ),
        );
        vec![tx.reply(vec![])]
    }

    /// 200
    async fn on_get_file_name_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::DOWNLOAD_FILES) {
            return vec![tx.reply_error(reason)];
        }

        // FILE_PATH (202) is optional in the request; root is assumed
        // when absent. Subfolder navigation isn't wired up yet - see
        // SharedState::root_files.
        let fields = state
            .root_files()
            .await
            .into_iter()
            .map(|entry| Field::bytes(ServerFieldId::FileList, entry.encode()))
            .collect();
        vec![tx.reply(fields)]
    }

    /// 101: read the classic (non-threaded) news.
    async fn on_newsfile_get(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::READ_NEWS) {
            return vec![tx.reply_error(reason)];
        }
        let news = state.news().await;
        vec![tx.reply(vec![Field::string(ServerFieldId::Text, news)])]
    }

    /// 103: post to the classic (non-threaded) news.
    async fn on_newsfile_post(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::POST_NEWS) {
            return vec![tx.reply_error(reason)];
        }
        let Some(text) = tx.field(ClientFieldId::Text) else {
            return vec![tx.reply_error("missing post text")];
        };
        let entry = state.post_news(&self.name, &text.as_string()).await;

        let notify = Transaction::push(
            ServerOpcode::NewsfilePost,
            vec![Field::string(ServerFieldId::Text, entry)],
        );
        for user in state.users().await {
            if user.access.contains(AccessPrivileges::READ_NEWS) {
                state.send_to(user.user_id, notify.clone());
            }
        }
        vec![tx.reply(vec![])]
    }

    /// 112: create a private chat room and invite its target user.
    async fn on_chat_create(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        if let Err(reason) = self.require(AccessPrivileges::CREATE_PCHATS) {
            return tx.reply_error(reason);
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return tx.reply_error("missing user id");
        };
        if state.user(target_id).await.is_none() {
            return tx.reply_error("no such user");
        }

        let chat_id = state.create_chat_room(self.user_id).await;
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);
        if target_id != self.user_id {
            state.send_to(
                target_id,
                Transaction::push(
                    ServerOpcode::ChatInvite,
                    vec![
                        Field::u32(ServerFieldId::ChatId, chat_id),
                        Field::u16(ServerFieldId::UserId, self.user_id),
                        Field::string(ServerFieldId::Name, self.name.clone()),
                    ],
                ),
            );
        }
        tx.reply(vec![
            Field::u32(ServerFieldId::ChatId, chat_id),
            Field::u16(ServerFieldId::UserId, self.user_id),
            Field::u16(ServerFieldId::Icon, icon),
            Field::u16(ServerFieldId::Color, self.user_flags()),
            Field::string(ServerFieldId::Name, self.name.clone()),
        ])
    }

    /// 113: invite another user to a room this session belongs to.
    async fn on_chat_invite(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let (Some(chat_id), Some(target_id)) = (
            tx.field(ClientFieldId::ChatId).and_then(Field::as_u32),
            tx.field(ClientFieldId::Uid).and_then(Field::as_u16),
        ) else {
            return tx.reply_error("missing chat id or user id");
        };
        let Some(members) = state.chat_room_members(chat_id).await else {
            return tx.reply_error("no such chat room");
        };
        if !members.contains(&self.user_id) {
            return tx.reply_error("you are not in that chat room");
        }
        if members.contains(&target_id) {
            return tx.reply_error("that user is already in the chat");
        }
        state.invited(chat_id, target_id).await;

        state.send_to(
            target_id,
            Transaction::push(
                ServerOpcode::ChatInvite,
                vec![
                    Field::u32(ServerFieldId::ChatId, chat_id),
                    Field::u16(ServerFieldId::UserId, self.user_id),
                    Field::string(ServerFieldId::Name, self.name.clone()),
                ],
            ),
        );
        tx.reply(vec![])
    }

    /// 115: join a room, accepting an invitation.
    async fn on_chat_join(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) else {
            return tx.reply_error("missing chat id");
        };
        if !state.join_chat_room(chat_id, self.user_id).await {
            return tx.reply_error("no such chat room");
        }
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);

        let notify = Transaction::push(
            ServerOpcode::ChatUserChange,
            vec![
                Field::u32(ServerFieldId::ChatId, chat_id),
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::u16(ServerFieldId::Icon, icon),
                Field::u16(ServerFieldId::Color, self.user_flags()),
                Field::string(ServerFieldId::Name, self.name.clone()),
            ],
        );

        let room = state.chat_room(chat_id).await.unwrap_or_default();
        let mut fields = Vec::new();
        for member_id in room.members {
            if member_id != self.user_id {
                state.send_to(member_id, notify.clone());
            }
            if let Some(entry) = state.user(member_id).await {
                fields.push(Field::bytes(
                    ServerFieldId::UserList,
                    UserNameWithInfo {
                        user_id: entry.user_id,
                        icon: entry.icon,
                        flags: entry.flags(),
                        name: entry.name,
                    }
                    .encode(),
                ));
            }
        }
        fields.push(Field::string(ServerFieldId::ChatSubject, room.subject));
        tx.reply(fields)
    }

    /// 116: leave a chat room.
    async fn on_chat_part(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) else {
            return tx.reply_error("missing chat id");
        };
        let remaining = state.chat_room_members(chat_id).await.unwrap_or_default();
        state.leave_chat_room(chat_id, self.user_id).await;

        let notify = Transaction::push(
            ServerOpcode::ChatUserPart,
            vec![
                Field::u32(ServerFieldId::ChatId, chat_id),
                Field::u16(ServerFieldId::UserId, self.user_id),
            ],
        );
        for member in remaining.into_iter().filter(|&m| m != self.user_id) {
            state.send_to(member, notify.clone());
        }
        tx.reply(vec![])
    }

    /// 114: decline a chat invitation.
    async fn on_chat_decline(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) {
            state.invite_declined(chat_id, self.user_id).await;
        }
        vec![tx.reply(vec![])]
    }

    /// 120: set a chat room's subject.
    async fn on_chat_subject(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let (Some(chat_id), Some(subject)) = (
            tx.field(ClientFieldId::ChatId).and_then(Field::as_u32),
            tx.field(ClientFieldId::ChatSubject).map(Field::as_string),
        ) else {
            return vec![tx.reply_error("missing chat id or subject")];
        };

        // Chat ID 0 (main room) gates on a privilege, not membership.
        let is_global = chat_id == 0;
        if is_global {
            if let Err(reason) = self.require(AccessPrivileges::DISCONNECT_USERS) {
                return vec![tx.reply_error(reason)];
            }
        }
        if !state.set_chat_subject(chat_id, subject.clone()).await {
            return vec![tx.reply_error("no such chat room")];
        }

        let notify = Transaction::push(
            ServerOpcode::ChatSubject,
            vec![
                Field::u32(ServerFieldId::ChatId, chat_id),
                Field::string(ServerFieldId::ChatSubject, subject),
            ],
        );
        if is_global {
            state.publish(notify);
        } else if let Some(members) = state.chat_room_members(chat_id).await {
            for member in members {
                state.send_to(member, notify.clone());
            }
        }
        vec![tx.reply(vec![])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_session(name: &str, access: AccessPrivileges) -> Session {
        Session {
            user_id: 1,
            name: name.to_string(),
            access,
        }
    }

    #[tokio::test]
    async fn chat_relay_starts_with_cr_for_scrollback_clients() {
        let session = test_session("Alice", AccessPrivileges::guest_default());
        let state = SharedState::default();
        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );

        let mut broadcast_rx = state.subscribe();
        assert!(session.on_chat(&request, &state).await.is_empty());

        let relay = broadcast_rx.try_recv().unwrap();
        let text = relay.field(ServerFieldId::Text).unwrap().as_string();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn login_registers_name_and_icon_and_broadcasts_join() {
        let mut session = test_session("guest1", AccessPrivileges::guest_default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut announce_rx = state.subscribe_announcements();

        // Name/Icon can arrive directly in Login, not just SET_CLIENT_USER_INFO.
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_login(&login, &config).await;

        // The join broadcast waits for Agreed, carrying the same name/icon.
        let agreed = Transaction::request(
            ClientOpcode::Agreed,
            2,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_agreed(&agreed, &state).await;

        // The join announcement excludes the joiner itself.
        let notify = announce_rx
            .try_recv()
            .expect("login should announce a join");
        assert_eq!(notify.exclude_user_id, 1); // test_session()'s fixed user_id
        assert_eq!(
            notify.tx.field(ServerFieldId::Name).unwrap().as_string(),
            "Alice"
        );
        assert_eq!(
            notify.tx.field(ServerFieldId::Icon).and_then(Field::as_u16),
            Some(42)
        );

        let users = state.users().await;
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].name, "Alice");
        assert_eq!(users[0].icon, 42);
    }

    #[tokio::test]
    async fn agreed_selfinfo_carries_the_joiners_own_user_list_entry() {
        let mut session = test_session("guest1", AccessPrivileges::guest_default());
        let state = SharedState::default();

        let agreed = Transaction::request(
            ClientOpcode::Agreed,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let selfinfo = replies
            .iter()
            .find(|tx| tx.opcode() == Opcode::Push(ServerOpcode::UserSelfinfo))
            .expect("on_agreed should push UserSelfinfo");
        let entry = selfinfo
            .field(ServerFieldId::UserList)
            .and_then(|f| UserNameWithInfo::decode(&f.as_bytes()).ok())
            .expect("UserSelfinfo should carry the joiner's own UserList entry");
        assert_eq!(entry.user_id, 1); // test_session()'s fixed user_id
        assert_eq!(entry.name, "Alice");
        assert_eq!(entry.icon, 42);
    }

    #[tokio::test]
    async fn login_colors_by_privilege_not_by_admin_login_identity() {
        // Color derives from DISCONNECT_USERS alone, even for a non-admin
        // login, and must be correct in the join broadcast itself.
        let mut session = test_session("guest1", AccessPrivileges::empty());
        let state = SharedState::default();
        let config = ServerConfig {
            default_access: AccessPrivileges::DISCONNECT_USERS,
            ..ServerConfig::default()
        };
        let mut announce_rx = state.subscribe_announcements();

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::string(ClientFieldId::Name, "Mod")],
        );
        session.on_login(&login, &config).await;

        let agreed = Transaction::request(
            ClientOpcode::Agreed,
            2,
            vec![Field::string(ClientFieldId::Name, "Mod")],
        );
        session.on_agreed(&agreed, &state).await;

        let notify = announce_rx
            .try_recv()
            .expect("login should announce a join");
        let color = notify
            .tx
            .field(ServerFieldId::Color)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        assert!(
            UserFlags::from_bits_truncate(color).contains(UserFlags::ADMIN),
            "join broadcast should already carry the admin color for a privileged non-admin-login user"
        );
    }

    #[tokio::test]
    async fn newsfile_round_trips_through_post_and_get() {
        let session = test_session("Alice", AccessPrivileges::all());
        let state = SharedState::default();

        let post = Transaction::request(
            ClientOpcode::NewsfilePost,
            1,
            vec![Field::string(ClientFieldId::Text, "hello news")],
        );
        session.on_newsfile_post(&post, &state).await;

        let get = Transaction::request(ClientOpcode::NewsfileGet, 2, vec![]);
        let reply = session.on_newsfile_get(&get, &state).await;
        let [reply] = reply.try_into().unwrap();
        let news = reply.field(ServerFieldId::Text).unwrap().as_string();
        assert!(news.contains("hello news"));
        assert!(news.contains("From Alice"));
    }

    #[tokio::test]
    async fn newsfile_post_notifies_only_users_with_read_news() {
        let session = test_session("Alice", AccessPrivileges::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                name: "Reader".to_string(),
                icon: 0,
                access: AccessPrivileges::READ_NEWS,
                last_active: Instant::now(),
            })
            .await;
        state
            .insert_user(UserEntry {
                user_id: 3,
                name: "NoNews".to_string(),
                icon: 0,
                access: AccessPrivileges::empty(),
                last_active: Instant::now(),
            })
            .await;

        let mut targeted_rx = state.subscribe_targeted();
        let post = Transaction::request(
            ClientOpcode::NewsfilePost,
            1,
            vec![Field::string(ClientFieldId::Text, "hello news")],
        );
        session.on_newsfile_post(&post, &state).await;

        let notify = targeted_rx.try_recv().unwrap();
        assert_eq!(notify.target_user_id, 2);
        assert!(
            targeted_rx.try_recv().is_err(),
            "NoNews shouldn't be notified"
        );
    }

    #[tokio::test]
    async fn kick_denied_without_privilege() {
        let mut session = test_session("Alice", AccessPrivileges::empty());
        let state = SharedState::default();
        let request = Transaction::request(
            ClientOpcode::UserKick,
            1,
            vec![Field::u16(ClientFieldId::Uid, 2)],
        );
        let replies = session.on_user_kick(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn private_message_delivered_only_to_target() {
        let session = test_session("Alice", AccessPrivileges::all());
        let state = SharedState::default();
        let mut targeted_rx = state.subscribe_targeted();

        let request = Transaction::request(
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;
        assert!(!replies[0].is_error());

        let delivered = targeted_rx.try_recv().unwrap();
        assert_eq!(delivered.target_user_id, 42);
        assert_eq!(
            delivered.tx.field(ServerFieldId::Text).unwrap().as_string(),
            "hi there"
        );
    }

    #[tokio::test]
    async fn chat_room_create_invite_and_room_scoped_chat() {
        let creator = test_session("Alice", AccessPrivileges::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 1,
                name: "Alice".to_string(),
                icon: 0,
                access: AccessPrivileges::all(),
                last_active: Instant::now(),
            })
            .await;
        state
            .insert_user(UserEntry {
                user_id: 2,
                name: "Bob".to_string(),
                icon: 0,
                access: AccessPrivileges::all(),
                last_active: Instant::now(),
            })
            .await;

        let mut targeted_rx = state.subscribe_targeted();
        let create = Transaction::request(
            ClientOpcode::ChatCreate,
            1,
            vec![Field::u16(ClientFieldId::Uid, 2)],
        );
        let reply = creator.on_chat_create(&create, &state).await;
        let chat_id = reply
            .field(ServerFieldId::ChatId)
            .unwrap()
            .as_u32()
            .unwrap();

        let invite = targeted_rx.try_recv().unwrap();
        assert_eq!(invite.target_user_id, 2);
        assert_eq!(
            invite.tx.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );

        let bob = test_session("Bob", AccessPrivileges::all());
        let bob = Session { user_id: 2, ..bob };
        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let join_reply = bob.on_chat_join(&join, &state).await;
        let user_list_ids: Vec<u16> = join_reply
            .fields
            .iter()
            .filter(|f| f.id == u16::from(ServerFieldId::UserList))
            .map(|f| UserNameWithInfo::decode(&f.as_bytes()).unwrap().user_id)
            .collect();
        assert_eq!(
            user_list_ids,
            vec![1, 2],
            "should list members in join order, not reversed"
        );
        assert_eq!(
            join_reply
                .field(ServerFieldId::ChatSubject)
                .unwrap()
                .as_string(),
            ""
        );

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members.len(), 2);

        // A chat sent with this ChatId should go to room members via
        // targeted delivery, not the public broadcast.
        let mut public_rx = state.subscribe();
        let mut targeted_rx = state.subscribe_targeted();
        let room_chat = Transaction::request(
            ClientOpcode::Chat,
            2,
            vec![
                Field::string(ClientFieldId::Text, "room only"),
                Field::u32(ClientFieldId::ChatId, chat_id),
            ],
        );
        creator.on_chat(&room_chat, &state).await;

        assert!(
            public_rx.try_recv().is_err(),
            "must not broadcast room chat"
        );
        let delivered = targeted_rx.try_recv().unwrap();
        assert_eq!(
            delivered.tx.field(ServerFieldId::Text).unwrap().as_string(),
            "\r        Alice:  room only"
        );
    }

    /// Real clients encode small integer fields (including a chat room
    /// ID) in as few bytes as fit, so a request carrying a 2-byte
    /// `ChatId` must decode just as well as one using the full 4 bytes.
    #[tokio::test]
    async fn chat_join_accepts_a_short_encoded_chat_id() {
        let bob = test_session("Bob", AccessPrivileges::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let reply = bob.on_chat_join(&join, &state).await;

        assert!(
            !reply.is_error(),
            "a short-encoded chat id should still be found"
        );
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_requires_disconnect_users() {
        let guest = test_session("Guest", AccessPrivileges::empty());
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::ChatSubject,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, 0),
                Field::string(ClientFieldId::ChatSubject, "Nope"),
            ],
        );
        let replies = guest.on_chat_subject(&request, &state).await;
        assert!(replies[0].is_error());
        assert_eq!(state.chat_room(0).await.unwrap().subject, "");
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_broadcasts_to_everyone() {
        let admin = test_session("Admin", AccessPrivileges::DISCONNECT_USERS);
        let state = SharedState::default();
        let mut broadcast_rx = state.subscribe();

        let request = Transaction::request(
            ClientOpcode::ChatSubject,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, 0),
                Field::string(ClientFieldId::ChatSubject, "Welcome"),
            ],
        );
        let replies = admin.on_chat_subject(&request, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(state.chat_room(0).await.unwrap().subject, "Welcome");

        let notify = broadcast_rx.try_recv().unwrap();
        assert_eq!(
            notify.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(0)
        );
        assert_eq!(
            notify
                .field(ServerFieldId::ChatSubject)
                .map(Field::as_string),
            Some("Welcome".to_string())
        );
    }

    #[tokio::test]
    async fn inviting_an_existing_member_is_an_error() {
        let alice = test_session("Alice", AccessPrivileges::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.join_chat_room(chat_id, 2).await;

        let invite = Transaction::request(
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::u16(ClientFieldId::Uid, 2),
            ],
        );
        let reply = alice.on_chat_invite(&invite, &state).await;

        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn joining_clears_the_invite() {
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invited(chat_id, 2).await;
        assert!(state.is_invited(chat_id, 2).await);

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", AccessPrivileges::all())
        };
        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        bob.on_chat_join(&join, &state).await;

        assert!(!state.is_invited(chat_id, 2).await);
    }

    #[tokio::test]
    async fn declining_clears_the_invite() {
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state.invited(chat_id, 2).await;

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", AccessPrivileges::all())
        };
        let decline = Transaction::request(
            ClientOpcode::ChatDecline,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let [reply] = bob
            .on_chat_decline(&decline, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        assert!(!state.is_invited(chat_id, 2).await);
    }
}
