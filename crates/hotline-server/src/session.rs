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
    /// Privileges granted at login. Checked directly here (rather than
    /// re-reading `SharedState`'s copy on every request) since this is
    /// the one place in the process that's guaranteed not to change
    /// concurrently out from under a single connection's handler calls.
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
            // Nothing is granted until `on_login` succeeds - a session
            // that tries to, say, send chat before logging in gets
            // correctly denied by the same privilege check a logged-in
            // user without SEND_CHAT would hit, rather than needing a
            // separate "have they logged in yet" gate.
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

    /// Handle one request transaction, returning zero or more
    /// transactions to send back (a plain reply, an unsolicited push
    /// like the post-login `USER_ACCESS` grant, a permission-denied
    /// notice, or nothing at all for fire-and-forget transactions).
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
            /* 107 */ ClientOpcode::Login => self.on_login(tx, state, config).await,
            /* 108 */ ClientOpcode::Msg => self.on_private_msg(tx, state).await,
            /* 110 */ ClientOpcode::UserKick => self.on_user_kick(tx, state).await,
            /* 112 */ ClientOpcode::ChatCreate => vec![self.on_chat_create(tx, state).await],
            /* 113 */ ClientOpcode::ChatInvite => vec![self.on_chat_invite(tx, state).await],
            /* 115 */ ClientOpcode::ChatJoin => vec![self.on_chat_join(tx, state).await],
            /* 116 */ ClientOpcode::ChatPart => vec![self.on_chat_part(tx, state).await],
            /* 120 */ ClientOpcode::ChatSubject => self.on_chat_subject(tx, state).await,
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

    /// `Err` names the missing privilege (via [`AccessPrivileges::flag_name`]
    /// where the bit has one) if `self.access` doesn't hold `required`.
    /// Returns a reason string rather than a ready `Transaction`, since
    /// callers need different delivery shapes: a reply-expecting
    /// transaction must get a proper [`Transaction::reply_error`] (the
    /// client's `request()` is awaiting exactly that trans ID), while a
    /// fire-and-forget one (chat) has nothing to award a reply to and
    /// needs an unsolicited push instead. Handing back a ready
    /// `Transaction` here would force one shape on every caller and
    /// silently hang whichever kind it guessed wrong for.
    fn require(&self, required: AccessPrivileges) -> std::result::Result<(), String> {
        if self.access.contains(required) {
            return Ok(());
        }
        let name = required.flag_name().unwrap_or("this privilege");
        Err(format!("You are not allowed to do that ({name})."))
    }

    async fn on_login(
        &mut self,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        // Both LOGIN and PASSWORD are XOR-0xFF obfuscated on the wire
        // (Hotline's `hl_code`).
        let login = tx.login();
        let password = tx.password();

        // No real account database yet - "admin"/"admin" is a hardcoded
        // stand-in for one, granting every privilege; everyone else gets
        // the server's configured flat guest grant.
        self.access = match (login.as_deref(), password.as_deref()) {
            (Some("admin"), Some("admin")) => AccessPrivileges::all(),
            _ => config.default_access,
        };

        // Real clients commonly embed the display name and icon directly
        // in the Login transaction rather than (or in addition to)
        // following up with a separate `SET_CLIENT_USER_INFO` (304) - so
        // read them here the same way `on_user_change` does. Registering
        // the user and broadcasting the join now (instead of waiting on
        // a 304 that may never come) is what lets already-connected
        // clients see this user appear, and lets this client's own entry
        // show its real name/icon from the start rather than the
        // `guestN`/icon-0 placeholder.
        if let Some(name) = tx.name() {
            self.name = name;
        }
        let icon = tx.icon().unwrap_or(0);
        // The join notification isn't echoed back to the joiner itself -
        // unlike later nick/icon changes, which are self-inclusive. This
        // client already learns its own name/icon/color from the
        // `GetUserNameList` reply it sends right after login.
        self.announce_user_change(icon, state, true).await;

        let mut fields = vec![Field::u16(ServerFieldId::Serverversion, 151)];
        if !config.server_name.is_empty() {
            fields.push(Field::string(
                ServerFieldId::Servername,
                config.server_name.clone(),
            ));
        }

        vec![
            tx.reply(fields),
            // Granted privileges go out as a separate push
            // (`ServerOpcode::UserSelfinfo`) rather than folding
            // `USER_ACCESS` into the login reply itself - real clients
            // gray out UI (upload buttons, admin menus, ...) based on
            // this transaction.
            Transaction::push(
                ServerOpcode::UserSelfinfo,
                vec![Field::u64(ServerFieldId::Access, self.access.bits())],
            ),
        ]
    }

    async fn on_user_change(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Some(name) = tx.name() {
            self.name = name;
        }
        let icon = tx.icon().unwrap_or(0);
        // Later changes are self-inclusive, unlike the join-only
        // exclusion in `announce_user_change`'s `exclude_self` path.
        self.announce_user_change(icon, state, false).await;
        Vec::new()
    }

    /// Register this session's current name/icon in the shared user
    /// table and broadcast a `UserChange` push.
    ///
    /// Shared by [`Session::on_login`] (first announcement,
    /// `exclude_self: true` since the joiner doesn't need an echo of its
    /// own join) and [`Session::on_user_change`] (later nick/icon
    /// changes, `exclude_self: false` since those are genuine updates
    /// every client including the sender should see reflected).
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

    /// This session's current [`UserNameWithInfo::flags`] value: admin
    /// (derived from the `DISCONNECT_USERS` privilege, same as mhxd's
    /// `disconnect_users`-driven color assignment, not a separately
    /// tracked identity) OR'd with idle (recomputed from the wall clock
    /// every time this is called, since idleness isn't an event, just a
    /// property of "how long since the last request").
    fn user_flags(&self) -> u16 {
        // Idle state can't be read from `self` alone (it needs the
        // shared last-active timestamp), so callers building a full
        // user-list snapshot compute it from `UserEntry::is_idle`
        // instead; this is only the admin half, used where a single
        // session pushes its own flags.
        if self.access.contains(AccessPrivileges::DISCONNECT_USERS) {
            UserFlags::ADMIN.bits()
        } else {
            0
        }
    }

    /// 105: public or (if `ChatId` is present) private-room chat.
    async fn on_chat(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.require(AccessPrivileges::SEND_CHAT) {
            // Per the older protocol guide: "Server transaction #104
            // 'Error' is used for when client sends a non-request that
            // fails, such as trying to send public chat when they don't
            // have chat privs." Sent straight back to the denied client
            // only - never broadcast. CHAT_SEND has no reply mechanism
            // at all, so this can't be a `Transaction::reply_error`
            // either - there's no pending request on the client side to
            // resolve.
            return vec![Transaction::push(
                ServerOpcode::Msg,
                vec![Field::string(ServerFieldId::Text, reason)],
            )];
        }

        let Some(message) = tx.field(ClientFieldId::Text) else {
            return Vec::new();
        };
        let chat_id = tx.field(ClientFieldId::ChatId).and_then(Field::as_u16);

        // mhxd's default chat_format is "\r%13.13s:  %s": the leading
        // \r starts the line fresh in a client that appends chat into
        // one continuous scrollback rather than rendering discrete rows.
        let mut fields = vec![Field::string(
            ServerFieldId::Text,
            format!("\r{:>13}:  {}", self.name, message.as_string()),
        )];
        if let Some(chat_id) = chat_id {
            fields.push(Field::u16(ServerFieldId::ChatId, chat_id));
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
            .map(|u| {
                let mut bits = if u.access.contains(AccessPrivileges::DISCONNECT_USERS) {
                    UserFlags::ADMIN.bits()
                } else {
                    0
                };
                if u.is_idle() {
                    bits |= UserFlags::AWAY.bits();
                }
                UserNameWithInfo {
                    user_id: u.user_id,
                    icon: u.icon,
                    flags: bits,
                    name: u.name,
                }
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
        state.post_news(&self.name, &text.as_string()).await;
        vec![tx.reply(vec![])]
    }

    /// 112: create a private chat room.
    async fn on_chat_create(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        if let Err(reason) = self.require(AccessPrivileges::CREATE_PCHATS) {
            return tx.reply_error(reason);
        }
        let chat_id = state.create_chat_room(self.user_id).await;
        tx.reply(vec![Field::u16(ServerFieldId::ChatId, chat_id)])
    }

    /// 113: invite another user to a room this session belongs to.
    async fn on_chat_invite(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let (Some(chat_id), Some(target_id)) = (
            tx.field(ClientFieldId::ChatId).and_then(Field::as_u16),
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

        state.send_to(
            target_id,
            Transaction::push(
                ServerOpcode::ChatInvite,
                vec![
                    Field::u16(ServerFieldId::ChatId, chat_id),
                    Field::string(ServerFieldId::Name, self.name.clone()),
                ],
            ),
        );
        tx.reply(vec![])
    }

    /// 115: join a room, accepting an invitation.
    async fn on_chat_join(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u16) else {
            return tx.reply_error("missing chat id");
        };
        if !state.join_chat_room(chat_id, self.user_id).await {
            return tx.reply_error("no such chat room");
        }

        let notify = Transaction::push(
            ServerOpcode::ChatUserChange,
            vec![
                Field::u16(ServerFieldId::ChatId, chat_id),
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::string(ServerFieldId::Name, self.name.clone()),
            ],
        );
        if let Some(members) = state.chat_room_members(chat_id).await {
            for member in members.into_iter().filter(|&m| m != self.user_id) {
                state.send_to(member, notify.clone());
            }
        }
        tx.reply(vec![])
    }

    /// 116: leave a chat room.
    async fn on_chat_part(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u16) else {
            return tx.reply_error("missing chat id");
        };
        let remaining = state.chat_room_members(chat_id).await.unwrap_or_default();
        state.leave_chat_room(chat_id, self.user_id).await;

        let notify = Transaction::push(
            ServerOpcode::ChatUserPart,
            vec![
                Field::u16(ServerFieldId::ChatId, chat_id),
                Field::u16(ServerFieldId::UserId, self.user_id),
            ],
        );
        for member in remaining.into_iter().filter(|&m| m != self.user_id) {
            state.send_to(member, notify.clone());
        }
        tx.reply(vec![])
    }

    /// 120: set a chat room's subject.
    async fn on_chat_subject(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let (Some(chat_id), Some(subject)) = (
            tx.field(ClientFieldId::ChatId).and_then(Field::as_u16),
            tx.field(ClientFieldId::ChatSubject).map(Field::as_string),
        ) else {
            return vec![tx.reply_error("missing chat id or subject")];
        };
        if !state.set_chat_subject(chat_id, subject.clone()).await {
            return vec![tx.reply_error("no such chat room")];
        }

        let notify = Transaction::push(
            ServerOpcode::ChatSubject,
            vec![
                Field::u16(ServerFieldId::ChatId, chat_id),
                Field::string(ServerFieldId::ChatSubject, subject),
            ],
        );
        if let Some(members) = state.chat_room_members(chat_id).await {
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

        // Real clients commonly send Name/Icon right in the Login
        // transaction rather than waiting to follow up with a separate
        // SET_CLIENT_USER_INFO.
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_login(&login, &state, &config).await;

        // Already-connected clients should see the join immediately - as
        // an announcement excluding the joiner itself; `Session::run`'s
        // announce-stream arm is what actually applies that exclusion
        // at delivery time.
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
    async fn login_colors_by_privilege_not_by_admin_login_identity() {
        // mhxd derives color purely from the `disconnect_users` privilege
        // bit (`rcv_login`'s `htlc->color = htlc->access.disconnect_users
        // ? 2 : 0`) - there's no separate "is admin" identity. A regular
        // (non-"admin"/"admin") login whose granted access happens to
        // include `DISCONNECT_USERS` should still show up admin-colored,
        // and the color should already be correct in the *join*
        // broadcast, not just after a later change.
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
        session.on_login(&login, &state, &config).await;

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

        let create = Transaction::request(ClientOpcode::ChatCreate, 1, vec![]);
        let reply = creator.on_chat_create(&create, &state).await;
        let chat_id = reply
            .field(ServerFieldId::ChatId)
            .unwrap()
            .as_u16()
            .unwrap();

        // Bob joins directly (skipping the invite round trip, which just
        // notifies - join is what actually adds a member).
        let bob_joined = state.join_chat_room(chat_id, 2).await;
        assert!(bob_joined);

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
                Field::u16(ClientFieldId::ChatId, chat_id),
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
}
