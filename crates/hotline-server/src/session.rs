use std::net::SocketAddr;
use std::time::{Duration, Instant, SystemTime};

use futures::{SinkExt, StreamExt};
use num_enum::{FromPrimitive, IntoPrimitive};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::field_id::{ClientFieldId, ServerFieldId};
use hotline_proto::opcode::{ClientOpcode, ServerOpcode, TransactionType};
use hotline_proto::text_encoding::{Encoding, EncodingHandle};
use hotline_proto::types::{HlDate, HlPath, UserFlags, UserNameWithInfo};
use hotline_proto::{ClientHandshake, Field, FieldList, ServerHandshake, Transaction};

use crate::accounts::{AccessExtra, Account};
use crate::error::{Result, ServerError};
use crate::files::FileRequester;
use crate::flood::FloodDecision;
use crate::server::ServerConfig;
use crate::state::{
    AccessUpdate, PendingTransfer, RecipientMessage, SharedState, TransferDirection, UserEntry,
    IDLE_THRESHOLD,
};

bitflags::bitflags! {
    /// `Options` (113) bit meanings when `HTLC_HDR_USER_CHANGE` (304) sends
    /// a persisted preference bitmap, distinct from the sequential values
    /// carried in `HTLC_HDR_MSG` (108)/`HTLS_HDR_MSG` (104) (see [`MsgOption`]).
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    struct UserChangeOptions: u16 {
        const REFUSE_PM       = 0x01;
        const REFUSE_CHAT     = 0x02;
        const AUTO_RESPONSE   = 0x04;
    }
}

/// `Options` (113) values on `HTLC_HDR_MSG` (108)/`HTLS_HDR_MSG` (104):
/// which kind of instant message this is, or which kind of bounce a
/// refusal/automatic-response reply carries.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
enum MsgOption {
    UserMessage = 1,
    RefuseMessage = 2,
    RefuseChat = 3,
    AutomaticResponse = 4,

    /// Not recognized by this build.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// `Options` (113) values on `HTLC_HDR_USER_KICK` (110): whether the kick
/// also imposes an address ban, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KickBan {
    None,
    Temporary,
    Permanent,
}

impl From<u16> for KickBan {
    fn from(value: u16) -> Self {
        match value {
            1 => Self::Temporary,
            2 => Self::Permanent,
            _ => Self::None,
        }
    }
}

/// Duration of a temporary kick-to-ban (`Options` (113) value 1). Not yet
/// configurable; a fixed default until this gets a `ServerConfig` knob.
const TEMP_BAN_DURATION: Duration = Duration::from_secs(30 * 60);

/// Which opcodes a session may currently send.
enum SessionState {
    /// Before `HTLC_HDR_LOGIN` (107) or `HTLC_HDR_AGREED` (121) complete login.
    Agreement,
    /// Login has completed.
    Joined,
}

/// A single client connection: handshake state plus the transaction
/// dispatch loop. Not `Clone`, one per socket.
pub struct Session {
    user_id: u16,
    name: String,
    /// Account login this session authenticated as, empty until login.
    login: String,
    /// The account's own registered display name, empty until login. See
    /// [`crate::state::UserEntry::account_name`].
    account_name: String,
    /// Privileges granted at login.
    access: Access,
    /// Server-side-only account settings; no client opcode reads or sets them.
    access_extra: AccessExtra,
    /// Client-reported protocol version (`HTLC_DATA_CLIENTVERSION`, field
    /// 160) from login, `None` if it never sent one.
    client_version: Option<u16>,
    state: SessionState,
    /// Whether the last broadcast `UserChange` marked this session away.
    is_away: bool,
    /// Refuses incoming private messages, set via `HTLC_HDR_USER_CHANGE` (304).
    refuses_pm: bool,
    /// Refuses incoming private chat invites, set via `HTLC_HDR_USER_CHANGE` (304).
    refuses_chat: bool,
    /// Custom automatic-response text sent to private-message senders, `None` when off.
    automatic_response: Option<String>,
    /// Remote socket address this connection was accepted from.
    remote_addr: SocketAddr,
    /// When this connection was accepted.
    connected_at: Instant,
    /// This connection's active text encoding, shared with the write half's `TransactionCodec`.
    encoding: EncodingHandle,
}

impl Session {
    /// Run the full lifecycle of one connection: handshake, then dispatch
    /// loop until the client disconnects.
    pub async fn run(
        mut stream: TcpStream,
        state: SharedState,
        config: ServerConfig,
    ) -> Result<()> {
        let remote_addr = stream.peer_addr()?;
        Self::handshake(&mut stream).await?;

        let encoding = EncodingHandle::default();
        let codec = TransactionCodec::for_server(encoding.clone());

        let user_id = state.next_user_id();
        let mut session = Session {
            user_id,
            name: format!("guest{user_id}"),
            // Empty until on_login succeeds.
            login: String::new(),
            account_name: String::new(),
            access: Access::empty(),
            access_extra: AccessExtra::default(),
            client_version: None,
            state: SessionState::Agreement,
            is_away: false,
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
            remote_addr,
            connected_at: Instant::now(),
            encoding,
        };

        let (mut writer, mut reader) = Framed::new(stream, codec).split();
        let mut rx = state.register_recipient(user_id).await;
        let idle_sleep = tokio::time::sleep(IDLE_THRESHOLD);
        tokio::pin!(idle_sleep);

        loop {
            tokio::select! {
                // Traffic from this client.
                frame = reader.next() => {
                    let Some(tx) = frame.transpose()? else { break }; // clean EOF
                    state.touch_user(session.user_id).await;
                    idle_sleep.as_mut().reset(tokio::time::Instant::now() + IDLE_THRESHOLD);
                    if session.is_away {
                        session.set_away(false, &state).await;
                    }
                    for reply in session.handle(&tx, &state, &config).await {
                        writer.send(reply).await?;
                    }
                }
                // Fires after IDLE_THRESHOLD with no request from this client.
                () = &mut idle_sleep, if !session.is_away && matches!(session.state, SessionState::Joined) => {
                    session.set_away(true, &state).await;
                }
                // Everything server-initiated for this session.
                msg = rx.recv() => {
                    match msg {
                        Some(RecipientMessage::Push(tx)) => {
                            let opcode = tx.r#type();
                            let is_kick = opcode == TransactionType::Push(ServerOpcode::Politequit);
                            if session.may_receive(opcode) {
                                writer.send(tx).await?;
                            }
                            if is_kick {
                                break; // Politequit disconnects the user
                            }
                        }
                        Some(RecipientMessage::AccessUpdate(update)) => {
                            session.apply_access_update(update, &state).await;
                        }
                        Some(RecipientMessage::Disconnect) => break,
                        None => break,
                    }
                }
            }
        }

        state.remove_recipient(session.user_id).await;
        state.remove_user(session.user_id).await;
        state.reset_flood(session.user_id).await;
        for (chat_id, remaining) in state.leave_all_chat_rooms(session.user_id).await {
            let notify = Transaction::push(
                ServerOpcode::ChatUserPart,
                vec![
                    Field::u32(ServerFieldId::ChatId, chat_id),
                    Field::u16(ServerFieldId::UserId, session.user_id),
                ],
            );
            for member in remaining {
                state.send_to(member, notify.clone()).await;
            }
        }
        let leave = Transaction::push(
            ServerOpcode::UserPart,
            vec![Field::u16(ServerFieldId::UserId, session.user_id)],
        );
        state.publish(leave).await;
        Ok(())
    }

    /// Checks the client's opening 12 bytes as one fixed byte string;
    /// closes the connection without a reply on any mismatch.
    async fn handshake(stream: &mut TcpStream) -> Result<()> {
        let mut buf = [0u8; ClientHandshake::LEN];
        stream.read_exact(&mut buf).await?;
        if buf != ClientHandshake::default().encode() {
            return Err(ServerError::HandshakeRejected(
                "unrecognized client handshake",
            ));
        }
        stream.write_all(&ServerHandshake::OK.encode()).await?;
        Ok(())
    }

    /// Whether this session should receive a broadcast/announce push, based
    /// on its own privileges. Pushes not listed here are unfiltered.
    fn may_receive(&self, opcode: TransactionType) -> bool {
        match opcode {
            TransactionType::Push(ServerOpcode::UserChange)
            | TransactionType::Push(ServerOpcode::UserPart) => {
                self.access_extra.contains(AccessExtra::USER_GETLIST)
            }
            TransactionType::Push(ServerOpcode::Chat) => self.access.contains(Access::READ_CHAT),
            TransactionType::Push(ServerOpcode::NewsfilePost) => {
                self.access.contains(Access::NEWS_READ_ART)
            }
            _ => true,
        }
    }

    /// Handle one request, returning zero or more transactions in reply.
    async fn handle(
        &mut self,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        let TransactionType::Request(opcode) = tx.r#type() else {
            tracing::debug!(opcode = ?tx.r#type(), "not a recognized client request");
            return vec![tx.reply_error("not implemented")];
        };

        match state.check_flood(self.user_id, opcode).await {
            FloodDecision::Kick => {
                if !self.access_extra.contains(AccessExtra::CAN_SPAM) {
                    state.disconnect(self.user_id).await;
                    return vec![];
                }
            }
            FloodDecision::Throttle => {
                if let Err(reason) = self.access_extra.require(AccessExtra::CAN_SPAM) {
                    return vec![Transaction::push(
                        ServerOpcode::Msg,
                        vec![Field::string(ServerFieldId::Text, reason)],
                    )];
                }
            }
            FloodDecision::Allow => {}
        }

        match self.state {
            SessionState::Agreement => self.handle_agreement(opcode, tx, state, config).await,
            SessionState::Joined => self.handle_joined(opcode, tx, state).await,
        }
    }

    /// Opcodes valid before login completes.
    async fn handle_agreement(
        &mut self,
        opcode: ClientOpcode,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        match opcode {
            /* 107 */ ClientOpcode::Login => self.on_login(tx, state, config).await,
            /* 121 */ ClientOpcode::Agreed => self.on_agreed(tx, state).await,
            /* 500 */ ClientOpcode::Ping => vec![tx.reply(vec![])],
            other => {
                tracing::debug!(?other, "opcode not allowed before login completes");
                vec![tx.reply_error("please finish logging in first")]
            }
        }
    }

    /// Opcodes valid once a session has joined.
    async fn handle_joined(
        &mut self,
        opcode: ClientOpcode,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        match opcode {
            /* 101 */ ClientOpcode::NewsfileGet => self.on_newsfile_get(tx, state).await,
            /* 103 */ ClientOpcode::NewsfilePost => self.on_newsfile_post(tx, state).await,
            /* 105 */ ClientOpcode::Chat => self.on_chat(tx, state).await,
            /* 107 */ ClientOpcode::Login => vec![tx.reply_error("already logged in")],
            /* 108 */ ClientOpcode::Msg => self.on_private_msg(tx, state).await,
            /* 110 */ ClientOpcode::UserKick => self.on_user_kick(tx, state).await,
            /* 112 */ ClientOpcode::ChatCreate => vec![self.on_chat_create(tx, state).await],
            /* 113 */ ClientOpcode::ChatInvite => self.on_chat_invite(tx, state).await,
            /* 114 */ ClientOpcode::ChatDecline => self.on_chat_decline(tx, state).await,
            /* 115 */ ClientOpcode::ChatJoin => vec![self.on_chat_join(tx, state).await],
            /* 116 */ ClientOpcode::ChatPart => self.on_chat_part(tx, state).await,
            /* 120 */ ClientOpcode::ChatSubject => self.on_chat_subject(tx, state).await,
            /* 121 */ ClientOpcode::Agreed => vec![tx.reply_error("already logged in")],
            /* 200 */ ClientOpcode::FileList => self.on_get_file_name_list(tx, state).await,
            /* 202 */ ClientOpcode::FileGet => self.on_file_get(tx, state).await,
            /* 203 */ ClientOpcode::FilePut => self.on_file_put(tx, state).await,
            /* 204 */ ClientOpcode::FileDelete => self.on_file_delete(tx, state).await,
            /* 205 */ ClientOpcode::FileMkdir => self.on_file_mkdir(tx, state).await,
            /* 206 */ ClientOpcode::FileGetinfo => self.on_file_getinfo(tx, state).await,
            /* 207 */ ClientOpcode::FileSetinfo => self.on_file_setinfo(tx, state).await,
            /* 208 */ ClientOpcode::FileMove => self.on_file_move(tx, state).await,
            /* 209 */ ClientOpcode::FileSymlink => self.on_file_symlink(tx, state).await,
            /* 300 */ ClientOpcode::UserGetlist => self.on_user_get_list(tx, state).await,
            /* 370 */ ClientOpcode::NewsListdir => self.on_news_listdir(tx, state).await,
            /* 371 */
            ClientOpcode::NewsListcategory => self.on_news_listcategory(tx, state).await,
            /* 380 */ ClientOpcode::NewsDelete => self.on_news_delete(tx, state).await,
            /* 381 */ ClientOpcode::NewsMkdir => self.on_news_mkdir(tx, state).await,
            /* 382 */
            ClientOpcode::NewsMkcategory => self.on_news_mkcategory(tx, state).await,
            /* 400 */ ClientOpcode::NewsGetthread => self.on_news_getthread(tx, state).await,
            /* 410 */
            ClientOpcode::NewsPostthread => self.on_news_postthread(tx, state).await,
            /* 411 */
            ClientOpcode::NewsDeletethread => self.on_news_deletethread(tx, state).await,
            /* 303 */ ClientOpcode::UserGetinfo => self.on_user_get_info(tx, state).await,
            /* 304 */ ClientOpcode::UserChange => self.on_user_change(tx, state).await,
            /* 348 */ ClientOpcode::GetUserList => self.on_get_user_list(tx, state).await,
            /* 349 */ ClientOpcode::SetUserList => self.on_set_user_list(tx, state).await,
            /* 350 */ ClientOpcode::AccountCreate => self.on_account_create(tx, state).await,
            /* 351 */ ClientOpcode::AccountDelete => self.on_account_delete(tx, state).await,
            /* 352 */ ClientOpcode::AccountRead => self.on_account_read(tx, state).await,
            /* 353 */ ClientOpcode::AccountModify => self.on_account_modify(tx, state).await,
            /* 355 */ ClientOpcode::MsgBroadcast => self.on_msg_broadcast(tx, state).await,
            /* 500 */ ClientOpcode::Ping => vec![tx.reply(vec![])],
            other => {
                tracing::debug!(?other, "unhandled opcode");
                vec![tx.reply_error("not implemented")]
            }
        }
    }

    async fn on_login(
        &mut self,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        let encoding = self.encoding();
        let login = tx.login(encoding);
        let password = tx.password(encoding);

        // An empty login field means "guest", same as the client omitting it.
        let login = login
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("guest");
        let password = password.as_deref().unwrap_or_default();

        self.login = login.to_lowercase();
        let account = state.account(&self.login).await;
        self.client_version = tx.client_version();
        self.access = match account {
            Some(account) if account.password == password => {
                self.account_name = account.name.clone();
                if !account.name.is_empty() {
                    self.name = account.name;
                }
                self.access_extra = account.access_extra;
                account.access
            }
            Some(_) => return self.reject_login(tx, state, "incorrect password").await,
            None => return self.reject_login(tx, state, "no such account").await,
        };

        let fields = vec![
            Field::u16(ServerFieldId::Serverversion, 190),
            Field::u16(ServerFieldId::Bannerid, 0),
            Field::string(ServerFieldId::Servername, config.server_name.clone()),
        ];
        let mut replies = vec![tx.reply(fields)];

        // No HTLC_DATA_CLIENTVERSION (160) counts as pre-150.
        let modern = self.client_version.unwrap_or(0) >= 150;

        // DONT_SHOW_AGREEMENT skips the real text, not just its display.
        let show_agreement = !self.access.contains(Access::NO_AGREEMENT);
        let have_agreement = !config.agreement.is_empty();
        if show_agreement && have_agreement {
            replies.push(Transaction::push(
                ServerOpcode::Agreement,
                vec![Field::string(ServerFieldId::Text, config.agreement.clone())],
            ));
        } else if modern {
            replies.push(Transaction::push(
                ServerOpcode::Agreement,
                vec![Field::bytes(ServerFieldId::Noagreement, vec![0, 1])],
            ));
        }

        // A name plus non-zero icon in Login, or a pre-150 client, joins right away.
        let icon = tx.icon().unwrap_or(0);
        let name = tx.name(encoding);
        if (name.is_some() && icon != 0) || !modern {
            let selfinfo = self.complete_join(name, icon, state).await;
            replies.push(selfinfo);
        }

        replies
    }

    /// Reply with `reason`, then disconnect: an unknown login or wrong
    /// password drops the connection rather than leaving it stuck
    /// unauthenticated.
    async fn reject_login(
        &self,
        tx: &Transaction,
        state: &SharedState,
        reason: &str,
    ) -> Vec<Transaction> {
        state
            .send_to(
                self.user_id,
                Transaction::push(
                    ServerOpcode::Politequit,
                    vec![Field::string(ServerFieldId::Text, reason)],
                ),
            )
            .await;
        vec![tx.reply_error(reason)]
    }

    /// Complete login after the client accepts the agreement (`HTLC_HDR_AGREED`, 121).
    async fn on_agreed(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let icon = tx.icon().unwrap_or(0);
        let encoding = self.encoding();
        let name = tx.name(encoding);
        let selfinfo = self.complete_join(name, icon, state).await;
        vec![tx.reply(vec![]), selfinfo]
    }

    /// Register name/icon, broadcast the join excluding self, mark this
    /// session `Joined`, and build its own `UserSelfinfo` push.
    async fn complete_join(
        &mut self,
        name: Option<String>,
        icon: u16,
        state: &SharedState,
    ) -> Transaction {
        self.state = SessionState::Joined;
        if let Some(name) = name {
            self.name = name;
        }
        // Not echoed back to the joiner.
        self.announce_user_change(icon, state, true).await;

        let self_entry = UserNameWithInfo {
            user_id: self.user_id,
            icon,
            flags: self.user_flags().bits(),
            name: self.name.clone(),
        };

        Transaction::push(
            ServerOpcode::UserSelfinfo,
            vec![
                Field::u64(ServerFieldId::Access, self.access.bits()),
                Field::bytes(ServerFieldId::UserList, self_entry.encode(self.encoding())),
            ],
        )
    }

    async fn on_user_change(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let encoding = self.encoding();
        if let Some(name) = tx.name(encoding) {
            self.name = name;
        }
        let icon = tx.icon().unwrap_or(0);
        let options = UserChangeOptions::from_bits_truncate(
            tx.field(ClientFieldId::Options)
                .and_then(Field::as_u16)
                .unwrap_or(0),
        );
        self.refuses_pm = options.contains(UserChangeOptions::REFUSE_PM);
        self.refuses_chat = options.contains(UserChangeOptions::REFUSE_CHAT);
        self.automatic_response = options.contains(UserChangeOptions::AUTO_RESPONSE).then(|| {
            tx.string(ClientFieldId::AutomaticResponse, encoding)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| {
                    "I'm not able to respond right now. Please try later!".to_string()
                })
        });
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
                login: self.login.clone(),
                account_name: self.account_name.clone(),
                name: self.name.clone(),
                icon,
                client_version: self.client_version,
                access: self.access,
                access_extra: self.access_extra,
                remote_addr: self.remote_addr,
                connected_at: self.connected_at,
                last_active: Instant::now(),
                refuses_pm: self.refuses_pm,
                refuses_chat: self.refuses_chat,
                automatic_response: self.automatic_response.clone(),
            })
            .await;

        let notify = Transaction::push(
            ServerOpcode::UserChange,
            vec![
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::u16(ServerFieldId::Icon, icon),
                Field::string(ServerFieldId::Name, self.name.clone()),
                Field::u16(ServerFieldId::Color, self.user_flags().bits()),
            ],
        );
        if exclude_self {
            state.announce(self.user_id, notify).await;
        } else {
            state.publish(notify).await;
        }
    }

    /// Applies a live account-modify change for this session's login.
    async fn apply_access_update(&mut self, update: AccessUpdate, state: &SharedState) {
        if self.login.is_empty() || update.login != self.login {
            return;
        }
        // Only force the account's name when this modify revokes USE_ANY_NAME.
        let losing_use_any_name =
            self.access.contains(Access::ANY_NAME) && !update.access.contains(Access::ANY_NAME);
        self.access = update.access;
        if losing_use_any_name {
            if let Some(name) = update.name {
                if name != self.name {
                    self.name = name;
                }
            }
        }
        let icon = state.user(self.user_id).await.map(|e| e.icon).unwrap_or(0);
        self.announce_user_change(icon, state, false).await;
    }

    /// Broadcast this session's away status changing. Reads name/icon back
    /// from `SharedState` rather than caching them on `Session` too.
    async fn set_away(&mut self, away: bool, state: &SharedState) {
        self.is_away = away;
        let Some(entry) = state.user(self.user_id).await else {
            return;
        };
        state
            .publish(Transaction::push(
                ServerOpcode::UserChange,
                vec![
                    Field::u16(ServerFieldId::UserId, self.user_id),
                    Field::u16(ServerFieldId::Icon, entry.icon),
                    Field::string(ServerFieldId::Name, entry.name),
                    Field::u16(ServerFieldId::Color, self.user_flags().bits()),
                ],
            ))
            .await;
    }

    /// This connection's active text encoding.
    fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }

    /// This session's own admin + away flags, for `UserChange`/`UserSelfinfo`.
    fn user_flags(&self) -> UserFlags {
        let is_admin = self.access.contains(Access::DISCON_USER);
        let mut flags = UserFlags::empty();
        flags.set(UserFlags::ADMIN, is_admin);
        flags.set(UserFlags::AWAY, self.is_away);
        flags.set(
            UserFlags::ALL_PRIVS,
            self.access == Access::all() - Access::NO_AGREEMENT,
        );
        flags.set(UserFlags::NO_PRIVS, self.access.is_empty());
        flags.set(UserFlags::REFUSES_PM, self.refuses_pm);
        flags.set(UserFlags::REFUSES_CHAT, self.refuses_chat);
        flags
    }

    /// 105: public or (if `ChatId` is present) private-room chat.
    async fn on_chat(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::SEND_CHAT) {
            // Sent only to the denied client; CHAT_SEND has no reply mechanism.
            return vec![Transaction::push(
                ServerOpcode::Msg,
                vec![Field::string(ServerFieldId::Text, reason)],
            )];
        }

        let chat_id = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32);
        let style = tx.field(ClientFieldId::Style).and_then(Field::as_u16);
        let encoding = self.encoding();
        let Some(message) = tx.string(ClientFieldId::Text, encoding) else {
            return Vec::new();
        };

        // Default chat_format: "\r%13.13s:  %s"; style 1 is the emote/action format.
        let text = if style == Some(1) {
            format!("\r *** {} {message}", self.name)
        } else {
            format!("\r{:>13}:  {message}", self.name)
        };
        let mut fields = vec![Field::string(ServerFieldId::Text, text)];
        if let Some(chat_id) = chat_id {
            fields.push(Field::u32(ServerFieldId::ChatId, chat_id));
        }
        let relay = Transaction::push(ServerOpcode::Chat, fields);

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

    /// 300
    async fn on_user_get_list(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
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
            .map(|entry| Field::bytes(ServerFieldId::UserList, entry.encode(encoding)))
            .collect();
        vec![tx.reply(fields)]
    }

    /// 303: renders as a single scrollable text block in the client's "Get
    /// Info" window, so its shape follows the reference's own labels and
    /// section order rather than this server's own report design.
    async fn on_user_get_info(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::GET_CLIENT_INFO) {
            return vec![tx.reply_error(reason)];
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let Some(target) = state.user(target_id).await else {
            return vec![tx.reply_error("no such user")];
        };

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
             -------- File Downloads ---------\r\rNone.\r\r\
             ------- Folder Downloads --------\r\rNone.\r\r\
             --------- File Uploads ----------\r\rNone.\r\r\
             -------- Folder Uploads ---------\r\rNone.\r\r\
             ------- Waiting Downloads -------\r\rNone.\r",
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
        );

        vec![tx.reply(vec![
            Field::string(ServerFieldId::Text, text),
            Field::string(ServerFieldId::Name, target.name),
        ])]
    }

    /// 110: disconnect another user, and if the `Options` (113) kick-to-ban
    /// bit is set, ban their address and notify them why before
    /// disconnecting them. A missing target id defaults to 0, which no
    /// session holds, so the kick is just a no-op.
    async fn on_user_kick(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DISCON_USER) {
            return vec![tx.reply_error(reason)];
        }
        let target_id = tx
            .field(ClientFieldId::Uid)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        if let Err(reason) = state.authorize_kick(target_id).await {
            return vec![tx.reply_error(reason)];
        }
        let ban = KickBan::from(
            tx.field(ClientFieldId::Options)
                .and_then(Field::as_u16)
                .unwrap_or(0),
        );
        let target_ip = state.user(target_id).await.map(|u| u.remote_addr.ip());

        let notice = match (ban, target_ip) {
            (KickBan::Temporary, Some(ip)) => {
                if let Err(error) = state
                    .ban(ip, Some(SystemTime::now() + TEMP_BAN_DURATION))
                    .await
                {
                    tracing::warn!(%ip, error, "failed to persist ban");
                }
                Some("You are temporarily banned on this server.")
            }
            (KickBan::Permanent, Some(ip)) => {
                if let Err(error) = state.ban(ip, None).await {
                    tracing::warn!(%ip, error, "failed to persist ban");
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
                            ServerOpcode::Politequit,
                            vec![Field::string(ServerFieldId::Text, reason)],
                        ),
                    )
                    .await;
            }
            None => state.disconnect(target_id).await,
        }

        vec![tx.reply(vec![])]
    }

    /// 108: a private instant message to one other user.
    async fn on_private_msg(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::SEND_MESSAGE) {
            if !self.access_extra.contains(AccessExtra::MSG) {
                return vec![tx.reply_error(reason)];
            }
        }
        let Some(target_id) = tx.field(ClientFieldId::Uid).and_then(Field::as_u16) else {
            return vec![tx.reply_error("missing user id")];
        };
        let encoding = self.encoding();
        let Some(message) = tx.string(ClientFieldId::Text, encoding) else {
            return vec![tx.reply_error("missing message")];
        };
        let options = tx
            .field(ClientFieldId::Options)
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
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::string(ServerFieldId::Name, self.name.clone()),
                Field::u16(ServerFieldId::Options, options),
                Field::string(ServerFieldId::Text, message),
            ];
            if let Some(quote) = tx.field(ClientFieldId::QuotingMsg) {
                fields.push(Field::bytes(
                    ServerFieldId::QuotingMsg,
                    quote.as_bytes(encoding).into_owned(),
                ));
            }
            state
                .send_to(target_id, Transaction::push(ServerOpcode::Msg, fields))
                .await;
        }
        replies.push(tx.reply(vec![]));
        replies
    }

    /// Builds the bounce sent back to this session when `target` refuses
    /// private messages/chat invites or has an automatic response
    /// configured, or `None` when delivery should proceed normally. Gated
    /// on this session's own PM-refusal setting for both message and
    /// chat-invite bounces, matching the reference's own reuse of that
    /// one flag.
    fn refusal_bounce(
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
                format!("{} does not accept private chats.", target.name),
            ),
            None => (
                MsgOption::RefuseMessage,
                format!("{} does not accept private messages.", target.name),
            ),
        };
        Some(Transaction::push(
            ServerOpcode::Msg,
            vec![
                Field::u16(ServerFieldId::UserId, target_id),
                Field::string(ServerFieldId::Name, target.name.clone()),
                Field::u16(ServerFieldId::Options, option.into()),
                Field::string(ServerFieldId::Text, text),
            ],
        ))
    }

    /// 355: a message broadcast to every connected user.
    async fn on_msg_broadcast(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::BROADCAST) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(message) = tx.string(ClientFieldId::Text, encoding) else {
            return vec![tx.reply_error("missing message")];
        };

        state
            .announce(
                self.user_id,
                Transaction::push(
                    ServerOpcode::MsgBroadcast,
                    vec![
                        Field::u16(ServerFieldId::UserId, self.user_id),
                        Field::string(ServerFieldId::Text, message),
                        Field::string(ServerFieldId::Name, self.name.clone()),
                    ],
                ),
            )
            .await;
        vec![tx.reply(vec![])]
    }

    /// 200
    async fn on_get_file_name_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::FILE_LIST) {
            return vec![tx.reply_error(reason)];
        }

        // FILE_PATH (202) is optional in the request; root is assumed
        // when absent.
        let encoding = self.encoding();
        let dir = match tx.file_path(ClientFieldId::FilePath, encoding) {
            Some(Ok(dir)) => dir,
            Some(Err(_)) => return vec![tx.reply_error("bad path")],
            None => HlPath::default(),
        };

        let requester = FileRequester { login: &self.login };
        let Some(entries) = state.list_files(requester, &dir, self.access).await else {
            return vec![tx.reply_error("file or folder not found.")];
        };
        let fields = entries
            .into_iter()
            .map(|entry| Field::bytes(ServerFieldId::FileList, entry.encode(encoding)))
            .collect();
        vec![tx.reply(fields)]
    }

    /// Decode a `FILE_PATH`-shaped field (`FilePath` or `FileNewPath`).
    /// An absent field is the root directory.
    fn path_field(
        &self,
        tx: &Transaction,
        id: ClientFieldId,
    ) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx
            .field(id)
            .map(|field| HlPath::decode(&field.as_bytes(encoding), encoding))
        {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("malformed file path"),
            None => Ok(HlPath::default()),
        }
    }

    /// A request's `FILE_NAME` (201) and `FILE_PATH` (202) fields.
    fn file_name_and_dir(
        &self,
        tx: &Transaction,
    ) -> std::result::Result<(String, HlPath), &'static str> {
        let encoding = self.encoding();
        let name = tx
            .string(ClientFieldId::FileName, encoding)
            .ok_or("missing file name")?;
        let dir = self.path_field(tx, ClientFieldId::FilePath)?;
        Ok((name, dir))
    }

    /// Like `file_name_and_dir`, but when `FILE_NAME` (201) is absent,
    /// derives the target name and containing directory from `FILE_PATH`
    /// (202)'s last component instead of erroring.
    fn file_name_and_dir_from_path_tail(
        &self,
        tx: &Transaction,
    ) -> std::result::Result<(String, HlPath), &'static str> {
        if tx.field(ClientFieldId::FileName).is_some() {
            return self.file_name_and_dir(tx);
        }
        let path = self.path_field(tx, ClientFieldId::FilePath)?;
        let (name, dir) = path
            .components
            .split_last()
            .ok_or("missing file name or path")?;
        Ok((name.clone(), HlPath::new(dir.to_vec())))
    }

    /// 202: start a download. Allocates a transfer reference and tells
    /// the client the total byte count (flattened-object overhead plus
    /// the file's own bytes) to expect on the data connection. A client
    /// resuming an interrupted download sends `HTLC_DATA_RFLT` (203) with
    /// the byte count it already has; that portion of the data fork is
    /// skipped, though the flattened-object header is always resent.
    async fn on_file_get(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DOWNLOAD_FILE) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let data_size = match state.file_size(requester, &dir, &name, self.access).await {
            Ok(size) => size,
            Err(reason) => return vec![tx.reply_error(reason)],
        };

        let encoding = self.encoding();
        let resume_offset = match tx.field(ClientFieldId::ResumeFileTransfer) {
            Some(field) => match hotline_proto::htxf::decode_resume_data(&field.as_bytes(encoding))
            {
                Ok(already_have) => (already_have as u64).min(data_size),
                Err(_) => return vec![tx.reply_error("Resume data is invalid.")],
            },
            None => 0,
        };
        let remaining = data_size - resume_offset;

        let total_size = remaining + hotline_proto::htxf::FLAT_FILE_HEADER_LEN as u64;
        let reference = state
            .register_transfer(PendingTransfer {
                direction: TransferDirection::Get,
                login: self.login.clone(),
                dir,
                name,
                size: data_size,
                resume_offset,
                client_ip: self.remote_addr.ip(),
            })
            .await;

        vec![tx.reply(vec![
            Field::u32(ServerFieldId::TransferId, reference),
            Field::u32(ServerFieldId::TransferSize, total_size as u32),
            Field::u32(ServerFieldId::FileSize, data_size as u32),
        ])]
    }

    /// 203: start an upload. The destination must be under a folder
    /// named "Upload" or "Drop Box" unless the account can upload
    /// anywhere; an existing file at the destination is rejected (resume
    /// isn't supported yet).
    async fn on_file_put(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::UPLOAD_FILE) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .authorize_upload(requester, &dir, &name, self.access)
            .await
        {
            return vec![tx.reply_error(reason)];
        }

        let size = tx
            .field(ClientFieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        let reference = state
            .register_transfer(PendingTransfer {
                direction: TransferDirection::Put,
                login: self.login.clone(),
                dir,
                name,
                size: size as u64,
                resume_offset: 0,
                client_ip: self.remote_addr.ip(),
            })
            .await;

        vec![tx.reply(vec![Field::u32(ServerFieldId::TransferId, reference)])]
    }

    /// 204: delete a file, or a folder and everything under it.
    async fn on_file_delete(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state.delete_file(requester, &dir, &name, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 205: create a folder.
    async fn on_file_mkdir(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_FOLDER) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir_from_path_tail(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state.mkdir(requester, &dir, &name, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 206: fetch a file or folder's metadata. Type/creator/creation date
    /// are omitted; nothing in this server persists real HFS metadata for
    /// them (see `HTXF`'s `INFO` fork). Comment is sent when one is stored.
    async fn on_file_getinfo(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::FILE_GETINFO) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir_from_path_tail(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let info = match state.file_info(requester, &dir, &name, self.access).await {
            Ok(info) => info,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let comment = match state
            .file_comment(requester, &dir, &name, self.access)
            .await
        {
            Ok(comment) => comment,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let zero_date = HlDate {
            year: 0,
            milliseconds: 0,
            seconds: 0,
        }
        .encode()
        .to_vec();
        let mut fields = vec![
            Field::string(ServerFieldId::FileName, info.name),
            Field::u32(ServerFieldId::FileSize, info.size),
            Field::bytes(ServerFieldId::FileDateModify, zero_date),
        ];
        if let Some(comment) = comment {
            fields.push(Field::string(ServerFieldId::FileComment, comment));
        }
        vec![tx.reply(fields)]
    }

    /// 207: rename a file or folder, and/or set its `FILE_COMMENT`. A
    /// rename, if present, applies before the comment, so a comment sent
    /// alongside a rename lands on the renamed entry.
    async fn on_file_setinfo(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let new_name = tx.string(ClientFieldId::FileNewName, encoding);
        let comment = tx.string(ClientFieldId::FileComment, encoding);
        if new_name.is_none() && comment.is_none() {
            return vec![tx.reply(vec![])];
        }
        let requester = FileRequester { login: &self.login };
        let mut name = name;
        if let Some(new_name) = &new_name {
            if let Err(reason) = state
                .rename_file(requester, &dir, &name, new_name, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
            name = new_name.clone();
        }
        if let Some(comment) = &comment {
            if let Err(reason) = state
                .set_file_comment(requester, &dir, &name, comment, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
        }
        vec![tx.reply(vec![])]
    }

    /// 208: move a file or folder into another folder.
    async fn on_file_move(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let to_dir = match self.path_field(tx, ClientFieldId::FileNewPath) {
            Ok(dir) => dir,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .move_file(requester, &dir, &name, &to_dir, self.access)
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 209: create an alias pointing at another file.
    async fn on_file_symlink(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::MAKE_ALIAS) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let link_dir = match self.path_field(tx, ClientFieldId::FileNewPath) {
            Ok(dir) => dir,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let link_name = tx
            .string(ClientFieldId::FileNewName, encoding)
            .unwrap_or_else(|| name.clone());
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .symlink_file(requester, &dir, &name, &link_dir, &link_name)
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 101: read the classic (non-threaded) news.
    async fn on_newsfile_get(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_READ_ART) {
            return vec![tx.reply_error(reason)];
        }
        let news = state.news().await;
        vec![tx.reply(vec![Field::string(ServerFieldId::Text, news)])]
    }

    /// 103: post to the classic (non-threaded) news.
    async fn on_newsfile_post(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::POST_NEWS_ART) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some((text, len)) = tx.string_len(ClientFieldId::Text, encoding) else {
            return vec![tx.reply_error("missing post text")];
        };
        if len > 8192 {
            return vec![tx.reply_error("Your news post was not accepted because it was too big.")];
        }
        let entry = state.post_news(&self.name, &text).await;

        state
            .publish(Transaction::push(
                ServerOpcode::NewsfilePost,
                vec![Field::string(ServerFieldId::Text, entry)],
            ))
            .await;
        vec![tx.reply(vec![])]
    }

    /// A `HTLC_DATA_NEWS_DIR` (325) field, decoded, or root if absent.
    fn news_path(&self, tx: &Transaction) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx.news_path(encoding) {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("bad path"),
            None => Ok(HlPath::default()),
        }
    }

    /// 370: list a threaded news bundle's own contents.
    async fn on_news_listdir(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let entries = match state.news_list_dir(&path).await {
            Ok(entries) => entries,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let mut fields = Vec::with_capacity(entries.len());
        for entry in entries {
            match entry.encode(encoding) {
                Ok(bytes) => fields.push(Field::bytes(ServerFieldId::NewsDirlistExtended, bytes)),
                Err(_) => return vec![tx.reply_error("news listing too large to encode")],
            }
        }
        vec![tx.reply(fields)]
    }

    /// 371: list a threaded news category's articles.
    async fn on_news_listcategory(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let listing = match state.news_list_articles(&path).await {
            Ok(listing) => listing,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let bytes = match listing.encode(self.encoding()) {
            Ok(bytes) => bytes,
            Err(_) => return vec![tx.reply_error("news listing too large to encode")],
        };
        vec![tx.reply(vec![Field::bytes(ServerFieldId::NewsCatlist, bytes)])]
    }

    /// 380: delete a threaded news bundle or category.
    async fn on_news_delete(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_delete(&path, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 381: create a threaded news bundle (folder).
    async fn on_news_mkdir(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_CREATE_FLDR) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(name) = tx.string(ClientFieldId::FileName, encoding) else {
            return vec![tx.reply_error("missing name")];
        };
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_mkdir(&path, &name).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 382: create a threaded news category.
    async fn on_news_mkcategory(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_CREATE_CAT) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(name) = tx.news_category_name(encoding) else {
            return vec![tx.reply_error("missing name")];
        };
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_mkcategory(&path, &name).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 400: fetch one threaded news article's data.
    async fn on_news_getthread(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_READ_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let Some(id) = tx.news_article_id() else {
            return vec![tx.reply_error("missing article id")];
        };
        let detail = match state.news_article(&path, id).await {
            Ok(detail) => detail,
            Err(reason) => return vec![tx.reply_error(reason)],
        };

        let mut fields = vec![
            Field::string(ServerFieldId::NewsSubject, detail.title),
            Field::string(ServerFieldId::NewsPoster, detail.poster),
            Field::bytes(ServerFieldId::NewsDate, detail.date.encode().to_vec()),
            Field::u32(ServerFieldId::NewsPrevthreadid, detail.prev_id),
            Field::u32(ServerFieldId::NewsNextthreadid, detail.next_id),
            Field::u32(ServerFieldId::NewsParentthreadid, detail.parent_id),
            Field::u32(ServerFieldId::NewsNextsubthreadid, detail.first_child_id),
        ];
        // The reply always echoes back the requested flavor, but only
        // includes the article body when it matches what's stored.
        let encoding = self.encoding();
        let requested_flavor = tx.news_article_flavor(encoding).unwrap_or_default();
        fields.push(Field::string(
            ServerFieldId::NewsMimetype,
            requested_flavor.clone(),
        ));
        if requested_flavor == detail.flavor {
            fields.push(Field::bytes(ServerFieldId::NewsPost, detail.data));
        }
        vec![tx.reply(fields)]
    }

    /// 410: post a threaded news article.
    async fn on_news_postthread(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::POST_NEWS_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let Some(title) = tx.news_article_title(encoding) else {
            return vec![tx.reply_error("missing title")];
        };
        let Some(data) = tx.news_article_data(encoding) else {
            return vec![tx.reply_error("missing article data")];
        };
        let parent_id = tx.news_article_id().unwrap_or(0);
        let flags = tx.news_article_flags().unwrap_or(0);
        // Openline stores an empty-string flavor when the client omits
        // this field; a real client always sends it, so defaulting to a
        // real MIME type here instead is a deliberate divergence.
        let flavor = tx
            .news_article_flavor(encoding)
            .unwrap_or_else(|| "text/plain".to_string());

        if let Err(reason) = state
            .news_post_article(
                &path,
                parent_id,
                title,
                self.name.clone(),
                flags,
                flavor,
                data,
            )
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 411: delete a threaded news article.
    async fn on_news_deletethread(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_DELETE_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let Some(id) = tx.news_article_id() else {
            return vec![tx.reply_error("missing article id")];
        };
        let recursive = tx.news_article_recurse_delete();
        if let Err(reason) = state.news_delete_article(&path, id, recursive).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 112: create a private chat room and invite its target users.
    async fn on_chat_create(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        if let Err(reason) = self.access.require(Access::CREATE_CHAT) {
            return tx.reply_error(reason);
        }
        let target_ids: Vec<u16> = tx
            .fields(ClientFieldId::Uid)
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
                        ServerOpcode::ChatInvite,
                        vec![
                            Field::u32(ServerFieldId::ChatId, chat_id),
                            Field::u16(ServerFieldId::UserId, self.user_id),
                            Field::string(ServerFieldId::Name, self.name.clone()),
                        ],
                    ),
                )
                .await;
        }
        tx.reply(vec![
            Field::u32(ServerFieldId::ChatId, chat_id),
            Field::u16(ServerFieldId::UserId, self.user_id),
            Field::u16(ServerFieldId::Icon, icon),
            Field::u16(ServerFieldId::Color, self.user_flags().bits()),
            Field::string(ServerFieldId::Name, self.name.clone()),
        ])
    }

    /// 113: invite another user to a room this session belongs to.
    async fn on_chat_invite(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let chat_id = tx
            .field(ClientFieldId::ChatId)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        let target_id = tx
            .field(ClientFieldId::Uid)
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
                            ServerOpcode::ChatInvite,
                            vec![
                                Field::u32(ServerFieldId::ChatId, chat_id),
                                Field::u16(ServerFieldId::UserId, self.user_id),
                                Field::string(ServerFieldId::Name, self.name.clone()),
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
    async fn on_chat_join(&self, tx: &Transaction, state: &SharedState) -> Transaction {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return tx.reply_error(reason);
        }
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) else {
            return tx.reply_error("missing chat id");
        };
        let encoding = self.encoding();
        let password = tx
            .string(ClientFieldId::Password, encoding)
            .unwrap_or_default();
        if let Err(reason) = state.join_chat_room(chat_id, self.user_id, &password).await {
            return tx.reply_error(reason);
        }
        let icon = state.user(self.user_id).await.map_or(0, |u| u.icon);

        let notify = Transaction::push(
            ServerOpcode::ChatUserChange,
            vec![
                Field::u32(ServerFieldId::ChatId, chat_id),
                Field::u16(ServerFieldId::UserId, self.user_id),
                Field::u16(ServerFieldId::Icon, icon),
                Field::u16(ServerFieldId::Color, self.user_flags().bits()),
                Field::string(ServerFieldId::Name, self.name.clone()),
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
                    ServerFieldId::UserList,
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
        fields.push(Field::string(ServerFieldId::ChatSubject, room.subject));
        tx.reply(fields)
    }

    /// 116: leave a chat room.
    async fn on_chat_part(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) else {
            return vec![tx.reply_error("missing chat id")];
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
            state.send_to(member, notify.clone()).await;
        }
        vec![]
    }

    /// 114: decline a chat invitation.
    async fn on_chat_decline(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        if let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) {
            state.invite_declined(chat_id, self.user_id).await;
            if let Some(members) = state.chat_room_members(chat_id).await {
                let notify = Transaction::push(
                    ServerOpcode::Chat,
                    vec![
                        Field::string(
                            ServerFieldId::Text,
                            format!("\r<<< \"{}\" declined invitation to chat >>>", self.name),
                        ),
                        Field::u32(ServerFieldId::ChatId, chat_id),
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
    async fn on_chat_subject(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::CHAT_PRIVATE) {
            return vec![tx.reply_error(reason)];
        }
        let Some(chat_id) = tx.field(ClientFieldId::ChatId).and_then(Field::as_u32) else {
            return vec![tx.reply_error("missing chat id")];
        };
        let encoding = self.encoding();
        let subject = tx.string(ClientFieldId::ChatSubject, encoding);
        let password = tx.string(ClientFieldId::Password, encoding);
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
                Field::string(ServerFieldId::ChatSubject, subject),
            )
            .await;
        }
        if let Some(password) = password {
            state.set_chat_password(chat_id, password.clone()).await;
            self.broadcast_chat_field(
                state,
                chat_id,
                is_global,
                Field::string(ServerFieldId::Password, password),
            )
            .await;
        }
        vec![tx.reply(vec![])]
    }

    /// Send a `ChatSubject` push carrying one changed field (subject or
    /// password) to a room's members, or everyone for the main room.
    async fn broadcast_chat_field(
        &self,
        state: &SharedState,
        chat_id: u32,
        is_global: bool,
        field: Field,
    ) {
        let notify = Transaction::push(
            ServerOpcode::ChatSubject,
            vec![Field::u32(ServerFieldId::ChatId, chat_id), field],
        );
        if is_global {
            state.publish(notify).await;
        } else if let Some(members) = state.chat_room_members(chat_id).await {
            for member in members {
                state.send_to(member, notify.clone()).await;
            }
        }
    }

    /// `HTLC_HDR_ACCOUNT_CREATE` (350)
    async fn on_account_create(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_USER) {
            return vec![tx.reply_error(reason)];
        }
        self.on_account_create_or_modify(tx, state).await
    }

    /// `HTLC_HDR_ACCOUNT_DELETE` (351)
    async fn on_account_delete(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
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
    /// the reference's own message text for an account deleted out from
    /// under it.
    async fn disconnect_account_sessions(login: &str, state: &SharedState) {
        for target in state.user_ids_for_login(login).await {
            state
                .send_to(
                    target,
                    Transaction::push(
                        ServerOpcode::Politequit,
                        vec![Field::string(
                            ServerFieldId::Text,
                            "You are logged in with an account which was deleted.",
                        )],
                    ),
                )
                .await;
        }
    }

    /// `HTLC_HDR_ACCOUNT_READ` (352)
    async fn on_account_read(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::OPEN_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(login) = tx.string(ClientFieldId::Login, encoding) else {
            return vec![tx.reply_error("missing login")];
        };
        let login = login.to_lowercase();
        let Some(account) = state.account(&login).await else {
            return vec![tx.reply_error("no such account")];
        };
        vec![tx.reply(vec![
            Field::string(ServerFieldId::Name, account.name),
            Field::obfuscated_string(ServerFieldId::Login, login),
            Self::redacted_password_field(&account.password),
            Field::u64(ServerFieldId::Access, account.access.bits()),
        ])]
    }

    /// `HTLC_HDR_ACCOUNT_MODIFY` (353)
    async fn on_account_modify(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::MODIFY_USER) {
            return vec![tx.reply_error(reason)];
        }
        self.on_account_create_or_modify(tx, state).await
    }

    /// Implementation for
    /// `HTLC_HDR_ACCOUNT_CREATE` (350) and
    /// `HTLC_HDR_ACCOUNT_MODIFY` (353).
    async fn on_account_create_or_modify(
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
        let password = match tx.field(ClientFieldId::Password) {
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

    /// The password field as sent back for `HTLC_HDR_ACCOUNT_READ` (352)
    /// and each record of `HTLC_HDR_GET_USER_LIST` (348): empty if unset,
    /// a single `x` otherwise, so the real password is never echoed.
    fn redacted_password_field(password: &str) -> Field {
        Field::string(
            ServerFieldId::Password,
            if password.is_empty() { "" } else { "x" },
        )
    }

    /// `HTLC_HDR_GET_USER_LIST` (348): every account at once, for the
    /// bulk account editor. Each account is one `Text` (101) field
    /// wrapping its own nested `Name`/`Login`/`Password`/`Access` field
    /// list, the same `count`-prefixed shape a transaction body uses.
    async fn on_get_user_list(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::OPEN_USER) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let fields = state
            .list_accounts()
            .await
            .into_iter()
            .filter_map(|(login, account)| {
                Transaction::encode_field_list(
                    &[
                        Field::string(ServerFieldId::Name, account.name),
                        Field::obfuscated_string(ServerFieldId::Login, login),
                        Self::redacted_password_field(&account.password),
                        Field::u64(ServerFieldId::Access, account.access.bits()),
                    ],
                    encoding,
                )
                .ok()
            })
            .map(|record| Field::bytes(ServerFieldId::Text, record))
            .collect();
        vec![tx.reply(fields)]
    }

    /// `HTLC_HDR_SET_USER_LIST` (349): save a bulk-edited account list.
    /// Each `Text` (101) field is one nested account record in the same
    /// shape `HTLC_HDR_GET_USER_LIST` sent; see
    /// [`Session::apply_user_list_record`] for the per-record rules.
    /// Processes every record even after a soft (storage) failure, but
    /// stops and reports immediately on a hard (permission) failure,
    /// matching the reference.
    async fn on_set_user_list(&self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let encoding = self.encoding();
        let mut save_error = false;
        for field in tx.fields(ClientFieldId::Text) {
            let Ok(record) = Transaction::decode_field_list(&field.as_bytes(encoding)) else {
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

    /// Apply one `HTLC_HDR_SET_USER_LIST` record: `Text` (101), if
    /// present, is the account's login before this edit (deobfuscated,
    /// scrambled the same way as `Login`), absent for a brand new record.
    /// An empty name and login means delete; otherwise this is a rename
    /// (login changed), a plain modify, or a create, depending on whether
    /// the old login was given and whether the new login already exists.
    ///
    /// `Ok(true)` if applied, `Ok(false)` for a soft (storage) failure
    /// that shouldn't stop the rest of the list from applying, `Err` for
    /// a hard (permission) failure that aborts the whole request.
    async fn apply_user_list_record(
        &self,
        record: &FieldList,
        state: &SharedState,
    ) -> std::result::Result<bool, String> {
        let encoding = self.encoding();
        let as_deobfuscated_string = |f: &Field| f.as_deobfuscated_string(encoding);
        let old_login = record
            .field(ClientFieldId::Text)
            .map(as_deobfuscated_string)
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase());
        let name = record
            .string(ClientFieldId::Name, encoding)
            .unwrap_or_default();
        let login = record
            .field(ClientFieldId::Login)
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

        let password = match record.field(ClientFieldId::Password) {
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
            .field(ClientFieldId::Access)
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

        // No old login: a brand new record, unless its login collides
        // with an existing account, which is a non-fatal notice, not an
        // error (matches the reference).
        if state.account(&login).await.is_some() {
            state
                .send_to(
                    self.user_id,
                    Transaction::push(
                        ServerOpcode::Msg,
                        vec![Field::string(
                            ServerFieldId::Text,
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
            return Err("Cannot create account with more access than yourself.".to_string());
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
    use hotline_proto::text_encoding;
    use tokio::sync::mpsc;

    fn test_session(name: &str, access: Access) -> Session {
        Session {
            user_id: 1,
            name: name.to_string(),
            login: String::new(),
            account_name: String::new(),
            access,
            access_extra: AccessExtra::default(),
            client_version: None,
            state: SessionState::Joined,
            is_away: false,
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            connected_at: Instant::now(),
            encoding: EncodingHandle::default(),
        }
    }

    /// A minimal connected [`UserEntry`], for tests exercising
    /// private-message/chat-invite refusal against another user.
    fn test_user(user_id: u16, name: &str) -> UserEntry {
        UserEntry {
            user_id,
            login: String::new(),
            account_name: String::new(),
            name: name.to_string(),
            icon: 0,
            client_version: None,
            access: Access::all(),
            access_extra: AccessExtra::default(),
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            connected_at: Instant::now(),
            last_active: Instant::now(),
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
        }
    }

    #[test]
    fn may_receive_gates_presence_pushes_on_user_getlist_not_other_traffic() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::USER_GETLIST);

        assert!(!session.may_receive(TransactionType::Push(ServerOpcode::UserChange)));
        assert!(!session.may_receive(TransactionType::Push(ServerOpcode::UserPart)));
        assert!(session.may_receive(TransactionType::Push(ServerOpcode::Chat)));

        session.access_extra.insert(AccessExtra::USER_GETLIST);
        assert!(session.may_receive(TransactionType::Push(ServerOpcode::UserChange)));
        assert!(session.may_receive(TransactionType::Push(ServerOpcode::UserPart)));
    }

    #[test]
    fn may_receive_gates_chat_on_read_chat() {
        let mut session = test_session("Alice", Access::all());
        session.access.remove(Access::READ_CHAT);
        assert!(!session.may_receive(TransactionType::Push(ServerOpcode::Chat)));

        session.access.insert(Access::READ_CHAT);
        assert!(session.may_receive(TransactionType::Push(ServerOpcode::Chat)));
    }

    #[test]
    fn may_receive_gates_newsfile_post_on_read_news() {
        let mut session = test_session("Alice", Access::all());
        session.access.remove(Access::NEWS_READ_ART);
        assert!(!session.may_receive(TransactionType::Push(ServerOpcode::NewsfilePost)));

        session.access.insert(Access::NEWS_READ_ART);
        assert!(session.may_receive(TransactionType::Push(ServerOpcode::NewsfilePost)));
    }

    /// Drains `rx` until an `AccessUpdate` arrives, skipping the
    /// `UserChange` push a prior `apply_access_update` already queued.
    fn next_access_update(rx: &mut mpsc::Receiver<RecipientMessage>) -> AccessUpdate {
        loop {
            match rx.try_recv().expect("expected an access update") {
                RecipientMessage::AccessUpdate(update) => return update,
                RecipientMessage::Push(tx) => {
                    assert_eq!(tx.r#type(), TransactionType::Push(ServerOpcode::UserChange));
                }
                RecipientMessage::Disconnect => panic!("unexpected disconnect"),
            }
        }
    }

    #[tokio::test]
    async fn user_get_list_denied_without_user_getlist() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::USER_GETLIST);
        let state = SharedState::default();
        let request = Transaction::request(ClientOpcode::UserGetlist, 1, vec![]);

        assert!(session.on_user_get_list(&request, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn user_get_list_encodes_entries_using_the_sessions_active_encoding() {
        let session = test_session("Alice", Access::all());
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        session.encoding.set(sjis);
        let state = SharedState::default();
        let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"; // "こんにちは"
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: "bob".to_string(),
                account_name: konnichiwa.to_string(),
                name: konnichiwa.to_string(),
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
        let request = Transaction::request(ClientOpcode::UserGetlist, 1, vec![]);

        let reply = &session.on_user_get_list(&request, &state).await[0];
        let entry = reply
            .field(ServerFieldId::UserList)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes(sjis), sjis).unwrap())
            .expect("expected bob's user list entry");
        assert_eq!(entry.name, konnichiwa);
    }

    #[tokio::test]
    async fn user_get_info_reports_the_reference_shaped_text_block() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: "bob".to_string(),
                account_name: "Bobby".to_string(),
                name: "Bob".to_string(),
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
            ClientOpcode::UserGetinfo,
            1,
            vec![Field::u16(ClientFieldId::Uid, 2)],
        );

        let reply = &session.on_user_get_info(&request, &state).await[0];
        assert_eq!(reply.string(ServerFieldId::Name, encoding).unwrap(), "Bob");
        assert!(reply.field(ServerFieldId::UserId).is_none());
        let text = reply.string(ServerFieldId::Text, encoding).unwrap();
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

    #[tokio::test]
    async fn user_get_info_defaults_login_and_account_name_when_empty() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: 2,
                login: String::new(),
                account_name: String::new(),
                name: "Bob".to_string(),
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
            ClientOpcode::UserGetinfo,
            1,
            vec![Field::u16(ClientFieldId::Uid, 2)],
        );

        let reply = &session.on_user_get_info(&request, &state).await[0];
        let text = reply.string(ServerFieldId::Text, encoding).unwrap();
        assert!(text.contains("Account:    (guest)"));
        assert!(text.contains("Name:       (none)"));
        assert!(!text.contains("Version:"));
    }

    #[tokio::test]
    async fn chat_relay_starts_with_cr_for_scrollback_clients() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session.on_chat(&request, &state).await.is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(ServerFieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn chat_relay_uses_emote_format_for_style_1() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![
                Field::string(ClientFieldId::Text, "waves"),
                Field::u16(ClientFieldId::Style, 1),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session.on_chat(&request, &state).await.is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(ServerFieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r *** Alice waves");
    }

    #[tokio::test]
    async fn chat_relay_keeps_normal_format_for_non_emote_style() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();
        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![
                Field::string(ClientFieldId::Text, "hi"),
                Field::u16(ClientFieldId::Style, 0),
            ],
        );

        let mut rx = state.register_recipient(1).await;
        assert!(session.on_chat(&request, &state).await.is_empty());

        let text = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push")
            .string(ServerFieldId::Text, encoding)
            .unwrap();
        assert_eq!(text, "\r        Alice:  hi");
    }

    #[tokio::test]
    async fn login_reply_advertises_server_version_190() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .field(ServerFieldId::Serverversion)
                .and_then(Field::as_u16),
            Some(190)
        );
    }

    #[tokio::test]
    async fn login_reply_sends_community_banner_id_zero() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .field(ServerFieldId::Bannerid)
                .and_then(Field::as_u16),
            Some(0)
        );
    }

    #[tokio::test]
    async fn login_reply_sends_empty_server_name_when_unconfigured() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            server_name: String::new(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0].string(ServerFieldId::Servername, encoding),
            Some(String::new())
        );
    }

    #[tokio::test]
    async fn login_with_name_registers_and_broadcasts_join_immediately() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut observer = state.register_recipient(2).await;
        let mut joiner_rx = state.register_recipient(1).await;

        // Name/Icon can arrive directly in Login, not just SET_CLIENT_USER_INFO.
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_login(&login, &state, &config).await;

        let notify = observer
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        assert_eq!(
            notify.string(ServerFieldId::Name, encoding).unwrap(),
            "Alice"
        );
        assert_eq!(
            notify.field(ServerFieldId::Icon).and_then(Field::as_u16),
            Some(42)
        );
        assert!(
            joiner_rx.try_recv().is_err(),
            "the joiner itself must not receive its own join announcement"
        );

        let joiner = state.user(1).await.expect("joiner should be registered");
        assert_eq!(joiner.name, "Alice");
        assert_eq!(joiner.icon, 42);
        assert!(matches!(session.state, SessionState::Joined));
    }

    #[tokio::test]
    async fn login_grants_the_matching_account_privileges() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "hunter2".to_string(),
                    name: "Alice".to_string(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "alice"),
                Field::obfuscated_string(ClientFieldId::Password, "hunter2"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(session.name, "Alice");
    }

    #[tokio::test]
    async fn login_matches_the_account_regardless_of_case() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "hunter2".to_string(),
                    name: "Alice".to_string(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "ALICE"),
                Field::obfuscated_string(ClientFieldId::Password, "hunter2"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(session.login, "alice");
    }

    #[tokio::test]
    async fn login_grants_the_account_user_getlist_setting() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::empty(),
                    password: String::new(),
                    name: "Alice".to_string(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::obfuscated_string(ClientFieldId::Login, "alice")],
        );
        session.on_login(&login, &state, &config).await;

        assert!(!session.access_extra.contains(AccessExtra::USER_GETLIST));
    }

    #[tokio::test]
    async fn login_with_wrong_password_is_rejected_and_disconnected() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "hunter2".to_string(),
                    name: "Alice".to_string(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();
        let mut rx = state.register_recipient(session.user_id).await;

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "alice"),
                Field::obfuscated_string(ClientFieldId::Password, "wrong"),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
        let kick = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Politequit push");
        assert_eq!(
            kick.r#type(),
            TransactionType::Push(ServerOpcode::Politequit)
        );
    }

    #[tokio::test]
    async fn login_with_a_nonempty_password_against_a_blank_stored_one_is_rejected() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Alice".to_string(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "alice"),
                Field::obfuscated_string(ClientFieldId::Password, "anything"),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn login_with_unknown_account_is_rejected_and_disconnected() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut rx = state.register_recipient(session.user_id).await;

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::obfuscated_string(ClientFieldId::Login, "nobody")],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn login_without_a_guest_account_is_rejected() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        state.delete_account("guest").await.unwrap();
        let config = ServerConfig::default();
        let mut rx = state.register_recipient(session.user_id).await;

        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn login_without_name_defers_join_until_agreed() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        session.state = SessionState::Agreement;
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::u16(ClientFieldId::Version, 151)],
        );
        session.on_login(&login, &state, &config).await;

        assert!(bystander.try_recv().is_err());
        assert!(matches!(session.state, SessionState::Agreement));

        let agreed = Transaction::request(
            ClientOpcode::Agreed,
            2,
            vec![
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_agreed(&agreed, &state).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("agreed should announce a join");
        assert_eq!(
            notify.string(ServerFieldId::Name, encoding).unwrap(),
            "Alice"
        );
        assert!(matches!(session.state, SessionState::Joined));
    }

    #[tokio::test]
    async fn pre_150_client_joins_immediately_without_a_name() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        session.state = SessionState::Agreement;
        // No Version field: counts as pre-150.
        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        session.on_login(&login, &state, &config).await;

        assert!(
            bystander.try_recv().is_ok(),
            "pre-150 clients join immediately even without a name"
        );
        assert!(matches!(session.state, SessionState::Joined));
    }

    #[tokio::test]
    async fn modern_client_with_a_name_but_no_icon_defers_join_until_agreed() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        session.state = SessionState::Agreement;
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::u16(ClientFieldId::Version, 151),
                Field::string(ClientFieldId::Name, "Alice"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert!(
            bystander.try_recv().is_err(),
            "a modern client with no (or zero) icon must not join until Agreed"
        );
        assert!(matches!(session.state, SessionState::Agreement));
    }

    #[tokio::test]
    async fn modern_client_with_a_name_and_icon_joins_immediately() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        session.state = SessionState::Agreement;
        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::u16(ClientFieldId::Version, 151),
                Field::string(ClientFieldId::Name, "Alice"),
                Field::u16(ClientFieldId::Icon, 42),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert!(
            bystander.try_recv().is_ok(),
            "a modern client with a name and a non-zero icon joins immediately"
        );
        assert!(matches!(session.state, SessionState::Joined));
    }

    #[tokio::test]
    async fn modern_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: String::new(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::u16(ClientFieldId::Version, 151)],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::Agreement))
            .expect("modern client should get a Noagreement marker");
        assert!(agreement.field(ServerFieldId::Noagreement).is_some());
    }

    #[tokio::test]
    async fn pre_150_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: String::new(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(ClientOpcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert!(
            !replies
                .iter()
                .any(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::Agreement)),
            "pre-150 clients don't understand the agreement handshake at all"
        );
    }

    #[tokio::test]
    async fn dont_show_agreement_skips_the_configured_text_but_still_marks_modern_clients_done() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        state
            .put_account(
                "skipper".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Skipper".to_string(),
                    access: Access::NO_AGREEMENT,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig {
            agreement: "please behave".to_string(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "skipper"),
                Field::u16(ClientFieldId::Version, 151),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::Agreement))
            .expect("modern client still gets a no-agreement marker");
        assert!(
            agreement.field(ServerFieldId::Noagreement).is_some(),
            "DONT_SHOW_AGREEMENT must skip the real text, not just its display"
        );
        assert!(agreement.field(ServerFieldId::Text).is_none());
    }

    #[tokio::test]
    async fn dont_show_agreement_sends_nothing_to_a_pre_150_client() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        state
            .put_account(
                "skipper".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: "Skipper".to_string(),
                    access: Access::NO_AGREEMENT,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig {
            agreement: "please behave".to_string(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::obfuscated_string(ClientFieldId::Login, "skipper")],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::Agreement)),);
    }

    #[tokio::test]
    async fn opcodes_other_than_login_agreed_ping_are_rejected_before_joining() {
        let mut session = Session {
            user_id: 1,
            name: "guest1".to_string(),
            login: String::new(),
            account_name: String::new(),
            access: Access::empty(),
            access_extra: AccessExtra::default(),
            client_version: None,
            state: SessionState::Agreement,
            is_away: false,
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            connected_at: Instant::now(),
            encoding: EncodingHandle::default(),
        };
        let state = SharedState::default();
        let config = ServerConfig::default();

        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );
        let replies = session.handle(&request, &state, &config).await;
        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn agreed_selfinfo_carries_the_joiners_own_user_list_entry() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
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
            .find(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::UserSelfinfo))
            .expect("on_agreed should push UserSelfinfo");
        let entry = selfinfo
            .field(ServerFieldId::UserList)
            .and_then(|f| UserNameWithInfo::decode(&f.as_bytes(encoding), encoding).ok())
            .expect("UserSelfinfo should carry the joiner's own UserList entry");
        assert_eq!(entry.user_id, 1); // test_session()'s fixed user_id
        assert_eq!(entry.name, "Alice");
        assert_eq!(entry.icon, 42);
    }

    #[tokio::test]
    async fn agreed_selfinfo_sends_the_session_real_access_bitmap() {
        let mut session = test_session("guest1", Access::DOWNLOAD_FILE);
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
            .find(|tx| tx.r#type() == TransactionType::Push(ServerOpcode::UserSelfinfo))
            .expect("on_agreed should push UserSelfinfo");
        assert_eq!(
            selfinfo
                .field(ServerFieldId::Access)
                .and_then(Field::as_u64),
            Some(Access::DOWNLOAD_FILE.bits())
        );
    }

    #[tokio::test]
    async fn login_colors_by_privilege_not_by_admin_login_identity() {
        // Color derives from DISCONNECT_USERS alone, even for a non-admin
        // login, and must be correct in the join broadcast itself.
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        state
            .put_account(
                "guest".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: String::new(),
                    name: String::new(),
                    access: Access::DISCON_USER,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        let login = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![Field::string(ClientFieldId::Name, "Mod")],
        );
        session.on_login(&login, &state, &config).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        let color = notify
            .field(ServerFieldId::Color)
            .and_then(Field::as_u16)
            .unwrap_or(0);
        assert!(
            UserFlags::from_bits_truncate(color).contains(UserFlags::ADMIN),
            "join broadcast should already carry the admin color for a privileged non-admin-login user"
        );
    }

    #[tokio::test]
    async fn set_away_broadcasts_the_away_bit_and_clears_it() {
        let mut session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        state
            .insert_user(UserEntry {
                user_id: session.user_id,
                login: String::new(),
                name: session.name.clone(),
                icon: 0,
                client_version: None,
                access: session.access,
                access_extra: AccessExtra::USER_GETLIST,
                account_name: String::new(),
                remote_addr: "127.0.0.1:0".parse().unwrap(),
                connected_at: Instant::now(),
                last_active: Instant::now(),
                refuses_pm: false,
                refuses_chat: false,
                automatic_response: None,
            })
            .await;
        let mut rx = state.register_recipient(session.user_id).await;

        session.set_away(true, &state).await;
        let color = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserChange push")
            .field(ServerFieldId::Color)
            .and_then(Field::as_u16)
            .map(UserFlags::from_bits_truncate)
            .unwrap_or_else(UserFlags::empty);
        assert!(color.contains(UserFlags::AWAY));

        session.set_away(false, &state).await;
        let color = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a UserChange push")
            .field(ServerFieldId::Color)
            .and_then(Field::as_u16)
            .map(UserFlags::from_bits_truncate)
            .unwrap_or_else(UserFlags::empty);
        assert!(!color.contains(UserFlags::AWAY));
    }

    #[test]
    fn user_flags_reports_all_privs_and_no_privs() {
        let all_privs = test_session("Alice", Access::all() - Access::NO_AGREEMENT);
        assert!(all_privs.user_flags().contains(UserFlags::ALL_PRIVS));
        assert!(!all_privs.user_flags().contains(UserFlags::NO_PRIVS));

        let no_privs = test_session("Bob", Access::empty());
        assert!(no_privs.user_flags().contains(UserFlags::NO_PRIVS));
        assert!(!no_privs.user_flags().contains(UserFlags::ALL_PRIVS));

        let partial = test_session("Carol", Access::READ_CHAT);
        assert!(!partial.user_flags().contains(UserFlags::ALL_PRIVS));
        assert!(!partial.user_flags().contains(UserFlags::NO_PRIVS));
    }

    #[tokio::test]
    async fn user_change_reads_the_refusal_bits_out_of_options() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::UserChange,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice".to_string()),
                Field::u16(ClientFieldId::Icon, 0),
                Field::u16(
                    ClientFieldId::Options,
                    (UserChangeOptions::REFUSE_PM | UserChangeOptions::REFUSE_CHAT).bits(),
                ),
            ],
        );
        session.on_user_change(&request, &state).await;

        assert!(session.refuses_pm);
        assert!(session.refuses_chat);
        assert!(session.automatic_response.is_none());
        assert!(session.user_flags().contains(UserFlags::REFUSES_PM));
        assert!(session.user_flags().contains(UserFlags::REFUSES_CHAT));
    }

    #[tokio::test]
    async fn user_change_reads_a_custom_automatic_response() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::UserChange,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice".to_string()),
                Field::u16(ClientFieldId::Icon, 0),
                Field::u16(
                    ClientFieldId::Options,
                    UserChangeOptions::AUTO_RESPONSE.bits(),
                ),
                Field::string(ClientFieldId::AutomaticResponse, "Back in five."),
            ],
        );
        session.on_user_change(&request, &state).await;

        assert_eq!(session.automatic_response.as_deref(), Some("Back in five."));
    }

    #[tokio::test]
    async fn user_change_falls_back_to_a_default_automatic_response_text() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::UserChange,
            1,
            vec![
                Field::string(ClientFieldId::Name, "Alice".to_string()),
                Field::u16(ClientFieldId::Icon, 0),
                Field::u16(
                    ClientFieldId::Options,
                    UserChangeOptions::AUTO_RESPONSE.bits(),
                ),
            ],
        );
        session.on_user_change(&request, &state).await;

        assert_eq!(
            session.automatic_response.as_deref(),
            Some("I'm not able to respond right now. Please try later!")
        );
    }

    #[tokio::test]
    async fn newsfile_round_trips_through_post_and_get() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
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
        let news = reply.string(ServerFieldId::Text, encoding).unwrap();
        assert!(news.contains("hello news"));
        assert!(news.contains("From Alice"));
    }

    #[tokio::test]
    async fn newsfile_post_publishes_once() {
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            ClientOpcode::NewsfilePost,
            1,
            vec![Field::string(ClientFieldId::Text, "hello news")],
        );
        session.on_newsfile_post(&post, &state).await;

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a NewsfilePost push");
        assert_eq!(
            notify.r#type(),
            TransactionType::Push(ServerOpcode::NewsfilePost)
        );
        assert!(rx.try_recv().is_err(), "published only once");
    }

    #[tokio::test]
    async fn newsfile_post_rejects_oversized_text() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            ClientOpcode::NewsfilePost,
            1,
            vec![Field::string(ClientFieldId::Text, "a".repeat(8193))],
        );
        let replies = session.on_newsfile_post(&post, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_error());
        assert_eq!(
            replies[0].string(ServerFieldId::ErrorText, encoding),
            Some("Your news post was not accepted because it was too big.".to_string())
        );
        assert!(state.news().await.is_empty(), "oversized post not stored");
        assert!(rx.try_recv().is_err(), "oversized post not published");
    }

    #[tokio::test]
    async fn kick_denied_without_privilege() {
        let mut session = test_session("Alice", Access::empty());
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
    async fn kick_disconnects_target_with_no_wire_notice() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(2).await;
        let request = Transaction::request(
            ClientOpcode::UserKick,
            1,
            vec![Field::u16(ClientFieldId::Uid, 2)],
        );
        let replies = session.on_user_kick(&request, &state).await;
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
            ClientOpcode::UserKick,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 2),
                Field::u16(ClientFieldId::Options, 2),
            ],
        );
        let replies = session.on_user_kick(&request, &state).await;
        assert!(!replies[0].is_error());

        assert!(state
            .is_banned("192.0.2.9".parse().unwrap())
            .await
            .is_some());

        let notice = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Politequit push notifying the target of the ban");
        assert_eq!(
            notice.string(ServerFieldId::Text, encoding).unwrap(),
            "You are permanently banned on this server."
        );
        assert_eq!(
            notice.r#type(),
            TransactionType::Push(ServerOpcode::Politequit)
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
            ClientOpcode::UserKick,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 2),
                Field::u16(ClientFieldId::Options, 1),
            ],
        );
        let replies = session.on_user_kick(&request, &state).await;
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
            .expect("expected a Politequit push notifying the target of the ban");
        assert_eq!(
            notice.string(ServerFieldId::Text, encoding).unwrap(),
            "You are temporarily banned on this server."
        );
    }

    #[tokio::test]
    async fn kick_with_missing_uid_replies_success() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let request = Transaction::request(ClientOpcode::UserKick, 1, vec![]);
        let replies = session.on_user_kick(&request, &state).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
    }

    fn state_with_flood(config: crate::flood::FloodConfig) -> SharedState {
        SharedState::new(
            std::sync::Arc::new(crate::accounts::InMemoryAccountProvider::default()),
            std::sync::Arc::new(crate::files::InMemoryFileProvider::default()),
            std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
            std::sync::Arc::new(crate::bans::InMemoryBanProvider::default()),
            std::sync::Arc::new(crate::flood::InMemoryFloodPolicy::new(config)),
        )
    }

    #[tokio::test]
    async fn hammering_a_session_past_the_flood_threshold_disconnects_it() {
        let state = state_with_flood(crate::flood::FloodConfig {
            default_points: 10,
            chat_points: 10,
            decay: Duration::from_secs(60),
            throttle_threshold: 15,
            kick_threshold: 25,
        });
        let mut session = test_session("Alice", Access::all());
        let mut rx = state.register_recipient(session.user_id).await;
        let ping = || Transaction::request(ClientOpcode::Ping, 1, vec![]);
        let config = ServerConfig::default();

        // First request: 10 points, under both thresholds.
        let replies = session.handle(&ping(), &state, &config).await;
        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());

        // Second request: 20 points, over throttle_threshold; dropped, with
        // a warning pushed back to the sender instead of a reply.
        let replies = session.handle(&ping(), &state, &config).await;
        assert_eq!(replies.len(), 1);
        assert_eq!(
            replies[0].r#type(),
            TransactionType::Push(ServerOpcode::Msg)
        );

        // Third request: 30 points, over kick_threshold; disconnected.
        let replies = session.handle(&ping(), &state, &config).await;
        assert!(replies.is_empty());

        let msg = rx.try_recv().expect("expected a recipient message");
        assert!(msg.is_disconnect(), "flood kick must send no wire notice");
    }

    #[tokio::test]
    async fn can_spam_exempts_a_session_from_the_flood_policy() {
        let state = state_with_flood(crate::flood::FloodConfig {
            default_points: 100,
            chat_points: 100,
            decay: Duration::from_secs(60),
            throttle_threshold: 15,
            kick_threshold: 25,
        });
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::CAN_SPAM);
        let mut rx = state.register_recipient(session.user_id).await;
        let config = ServerConfig::default();

        for _ in 0..5 {
            let replies = session
                .handle(
                    &Transaction::request(ClientOpcode::Ping, 1, vec![]),
                    &state,
                    &config,
                )
                .await;
            assert_eq!(replies.len(), 1);
            assert!(!replies[0].is_error());
        }
        assert!(
            rx.try_recv().is_err(),
            "exempt session was never disconnected"
        );
    }

    #[tokio::test]
    async fn private_message_delivered_only_to_target() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(42).await;
        let mut sender_rx = state.register_recipient(session.user_id).await;

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

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Msg push");
        assert_eq!(
            delivered.string(ServerFieldId::Text, encoding).unwrap(),
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
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::u16(ClientFieldId::Options, MsgOption::UserMessage.into()),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;
        assert!(!replies[0].is_error());

        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Msg push");
        assert_eq!(
            delivered.field(ServerFieldId::Options).unwrap().as_u16(),
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
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::u16(ClientFieldId::Options, MsgOption::UserMessage.into()),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].string(ServerFieldId::Text, encoding).unwrap(),
            "Bob does not accept private messages."
        );
        assert_eq!(
            replies[0].field(ServerFieldId::Options).unwrap().as_u16(),
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
                automatic_response: Some("At lunch, back soon.".to_string()),
                ..test_user(42, "Bob")
            })
            .await;
        let mut target_rx = state.register_recipient(42).await;

        let request = Transaction::request(
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::u16(ClientFieldId::Options, MsgOption::UserMessage.into()),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].string(ServerFieldId::Text, encoding).unwrap(),
            "At lunch, back soon."
        );
        assert_eq!(
            replies[0].field(ServerFieldId::Options).unwrap().as_u16(),
            Some(MsgOption::AutomaticResponse.into())
        );
        let delivered = target_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("automatic response still lets the message through");
        assert_eq!(
            delivered.string(ServerFieldId::Text, encoding).unwrap(),
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
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::u16(ClientFieldId::Options, MsgOption::UserMessage.into()),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
    }

    #[tokio::test]
    async fn msg_allowed_via_access_extra_msg_without_send_message() {
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.insert(AccessExtra::MSG);
        let state = SharedState::default();
        let mut target_rx = state.register_recipient(42).await;

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
        assert!(target_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn msg_denied_without_send_message_or_access_extra_msg() {
        let mut session = test_session("Alice", Access::empty());
        session.access_extra.remove(AccessExtra::MSG);
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::Msg,
            1,
            vec![
                Field::u16(ClientFieldId::Uid, 42),
                Field::string(ClientFieldId::Text, "hi there"),
            ],
        );
        let replies = session.on_private_msg(&request, &state).await;
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn chat_membership_opcodes_denied_without_chat_private() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::CHAT_PRIVATE);
        let state = SharedState::default();

        let invite = Transaction::request(
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, 1),
                Field::u16(ClientFieldId::Uid, 2),
            ],
        );
        assert!(session.on_chat_invite(&invite, &state).await[0].is_error());

        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, 1)],
        );
        assert!(session.on_chat_join(&join, &state).await.is_error());

        let part = Transaction::request(
            ClientOpcode::ChatPart,
            1,
            vec![Field::u32(ClientFieldId::ChatId, 1)],
        );
        assert!(session.on_chat_part(&part, &state).await[0].is_error());

        let decline = Transaction::request(
            ClientOpcode::ChatDecline,
            1,
            vec![Field::u32(ClientFieldId::ChatId, 1)],
        );
        assert!(session.on_chat_decline(&decline, &state).await[0].is_error());

        let subject = Transaction::request(
            ClientOpcode::ChatSubject,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, 1),
                Field::string(ClientFieldId::ChatSubject, "Topic"),
            ],
        );
        assert!(session.on_chat_subject(&subject, &state).await[0].is_error());
    }

    #[tokio::test]
    async fn message_broadcast_reaches_everyone_except_sender() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut sender_rx = state.register_recipient(session.user_id).await;
        let mut rx = state.register_recipient(2).await;

        let request = Transaction::request(
            ClientOpcode::MsgBroadcast,
            1,
            vec![Field::string(ClientFieldId::Text, "server going down")],
        );
        let replies = session.on_msg_broadcast(&request, &state).await;
        assert!(!replies[0].is_error());

        let sent = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a MsgBroadcast push");
        assert_eq!(
            sent.r#type(),
            TransactionType::Push(ServerOpcode::MsgBroadcast)
        );
        assert_eq!(
            sent.field(ServerFieldId::Text).unwrap().as_string(encoding),
            "server going down"
        );

        assert!(sender_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn message_broadcast_denied_without_privilege() {
        let session = test_session("Alice", Access::default());
        let state = SharedState::default();

        let request = Transaction::request(
            ClientOpcode::MsgBroadcast,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );
        let replies = session.on_msg_broadcast(&request, &state).await;
        assert!(replies[0].is_error());
    }

    #[tokio::test]
    async fn account_create_then_read_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            ClientOpcode::AccountCreate,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::obfuscated_string(ClientFieldId::Password, "secret"),
                Field::string(ClientFieldId::Name, "Bob"),
                Field::u64(ClientFieldId::Access, Access::SEND_CHAT.bits()),
            ],
        );
        let replies = session.on_account_create(&create, &state).await;
        assert!(!replies[0].is_error());

        let read = Transaction::request(
            ClientOpcode::AccountRead,
            2,
            vec![Field::obfuscated_string(ClientFieldId::Login, "bob")],
        );
        let replies = session.on_account_read(&read, &state).await;
        assert!(!replies[0].is_error());
        assert_eq!(
            replies[0].string(ServerFieldId::Name, encoding).unwrap(),
            "Bob"
        );
        assert_eq!(
            replies[0]
                .field(ServerFieldId::Access)
                .and_then(Field::as_u64),
            Some(Access::SEND_CHAT.bits())
        );
    }

    #[tokio::test]
    async fn account_create_folds_the_login_to_lowercase() {
        let session = test_session("Admin", Access::all());
        let state = SharedState::default();

        let create = Transaction::request(
            ClientOpcode::AccountCreate,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "Bob"),
                Field::obfuscated_string(ClientFieldId::Password, "secret"),
                Field::string(ClientFieldId::Name, "Bob"),
                Field::u64(ClientFieldId::Access, Access::SEND_CHAT.bits()),
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
            ClientOpcode::AccountDelete,
            1,
            vec![Field::obfuscated_string(ClientFieldId::Login, "BOB")],
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
            ClientOpcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::string(ClientFieldId::Name, "Bobby"),
                Field::bytes(ClientFieldId::Password, vec![0]),
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
        // clears the password, matching the reference's own behavior.
        let modify = Transaction::request(
            ClientOpcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::string(ClientFieldId::Name, "Bobby"),
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
                ClientOpcode::AccountRead,
                1,
                vec![Field::obfuscated_string(ClientFieldId::Login, login)],
            )
        };

        let [with_password] = session
            .on_account_read(&read("bob"), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            with_password
                .field(ServerFieldId::Password)
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
                .field(ServerFieldId::Password)
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
            ClientOpcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::u64(ClientFieldId::Access, Access::BROADCAST.bits()),
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
            notify.field(ServerFieldId::UserId).and_then(Field::as_u16),
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
            ClientOpcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::u64(ClientFieldId::Access, Access::empty().bits()),
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
            ClientOpcode::AccountModify,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "bob"),
                Field::u64(
                    ClientFieldId::Access,
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
            ClientOpcode::AccountDelete,
            1,
            vec![Field::obfuscated_string(ClientFieldId::Login, "bob")],
        );
        let replies = session.on_account_delete(&delete, &state).await;
        assert!(!replies[0].is_error());
        assert!(state.account("bob").await.is_none());
    }

    #[tokio::test]
    async fn account_crud_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let login_field = Field::obfuscated_string(ClientFieldId::Login, "bob");

        let create =
            Transaction::request(ClientOpcode::AccountCreate, 1, vec![login_field.clone()]);
        assert!(session.on_account_create(&create, &state).await[0].is_error());

        let read = Transaction::request(ClientOpcode::AccountRead, 2, vec![login_field.clone()]);
        assert!(session.on_account_read(&read, &state).await[0].is_error());

        let modify =
            Transaction::request(ClientOpcode::AccountModify, 3, vec![login_field.clone()]);
        assert!(session.on_account_modify(&modify, &state).await[0].is_error());

        let delete = Transaction::request(ClientOpcode::AccountDelete, 4, vec![login_field]);
        assert!(session.on_account_delete(&delete, &state).await[0].is_error());
    }

    /// One `HTLC_HDR_SET_USER_LIST` record: `old_login` is the account's
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
            fields.push(Field::obfuscated_string(ClientFieldId::Text, old_login));
        }
        if !name.is_empty() {
            fields.push(Field::string(ClientFieldId::Name, name));
        }
        if !login.is_empty() {
            fields.push(Field::obfuscated_string(ClientFieldId::Login, login));
        }
        fields.push(password);
        fields.push(Field::u64(ClientFieldId::Access, access.bits()));
        Field::bytes(
            ClientFieldId::Text,
            Transaction::encode_field_list(&fields, encoding).unwrap(),
        )
    }

    #[tokio::test]
    async fn get_user_list_denied_without_open_user() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();
        let request = Transaction::request(ClientOpcode::GetUserList, 1, vec![]);
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

        let request = Transaction::request(ClientOpcode::GetUserList, 1, vec![]);
        let [reply] = session
            .on_get_user_list(&request, &state)
            .await
            .try_into()
            .unwrap();

        // The seeded admin/guest accounts plus the one just added.
        let records: Vec<_> = reply.fields(ServerFieldId::Text).collect();
        assert_eq!(records.len(), 3);

        let bob = records
            .iter()
            .map(|f| Transaction::decode_field_list(&f.as_bytes(encoding)).unwrap())
            .find(|fields| fields.string(ServerFieldId::Name, encoding).as_deref() == Some("Bob"))
            .expect("bob's record");
        let login = bob
            .field(ServerFieldId::Login)
            .unwrap()
            .as_deobfuscated_string(encoding);
        assert_eq!(login, "bob");
        let password = bob.field(ServerFieldId::Password).unwrap();
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
            Field::obfuscated_string(ClientFieldId::Password, "secret"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![record]);
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
            Field::obfuscated_string(ClientFieldId::Password, "secret"),
            Access::all(),
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![record]);
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
            Field::obfuscated_string(ClientFieldId::Password, "new"),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![record]);
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
            Field::bytes(ClientFieldId::Password, vec![0u8]),
            Access::SEND_CHAT,
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![record]);
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
            Field::bytes(ClientFieldId::Password, vec![]),
            Access::empty(),
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![record]);
        let replies = session.on_set_user_list(&request, &state).await;
        assert!(!replies[0].is_error());
        assert!(state.account("bob").await.is_none());

        let kick = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Politequit push");
        assert_eq!(
            kick.r#type(),
            TransactionType::Push(ServerOpcode::Politequit)
        );
    }

    #[tokio::test]
    async fn set_user_list_denied_without_matching_privilege() {
        let session = test_session("Alice", Access::empty());
        let state = SharedState::default();

        let create = user_list_record(
            None,
            "Bob",
            "bob",
            Field::obfuscated_string(ClientFieldId::Password, "secret"),
            Access::empty(),
        );
        let request = Transaction::request(ClientOpcode::SetUserList, 1, vec![create]);
        assert!(session.on_set_user_list(&request, &state).await[0].is_error());
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

        let invite = bob_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatInvite push");
        assert_eq!(
            invite.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );

        let bob = test_session("Bob", Access::all());
        let bob = Session { user_id: 2, ..bob };
        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let join_reply = bob.on_chat_join(&join, &state).await;
        let user_list_ids: Vec<u16> = join_reply
            .fields(ServerFieldId::UserList)
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
            join_reply
                .string(ServerFieldId::ChatSubject, encoding)
                .unwrap(),
            ""
        );

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members.len(), 2);

        // Room chat goes via targeted delivery, not the public broadcast.
        let mut creator_rx = state.register_recipient(1).await;
        let room_chat = Transaction::request(
            ClientOpcode::Chat,
            2,
            vec![
                Field::string(ClientFieldId::Text, "room only"),
                Field::u32(ClientFieldId::ChatId, chat_id),
            ],
        );
        creator.on_chat(&room_chat, &state).await;

        let delivered = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a room-chat push");
        assert_eq!(
            delivered.string(ServerFieldId::Text, encoding).unwrap(),
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
            ClientOpcode::ChatPart,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let replies = bob.on_chat_part(&part, &state).await;

        assert!(replies.is_empty(), "leaving a chat room gets no reply");

        let notify = creator_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatUserPart push");
        assert_eq!(
            notify.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );
        assert_eq!(
            notify.field(ServerFieldId::UserId).unwrap().as_u16(),
            Some(2)
        );

        let members = state.chat_room_members(chat_id).await.unwrap();
        assert_eq!(members, vec![1]);
    }

    #[tokio::test]
    async fn joining_an_unprotected_room_without_an_invite_is_an_error() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let join = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
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
    async fn joining_a_password_protected_room_requires_the_right_password() {
        let bob = test_session("Bob", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;
        state
            .set_chat_password(chat_id, "hunter2".to_string())
            .await;

        let no_password = Transaction::request(
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        assert!(bob.on_chat_join(&no_password, &state).await.is_error());

        let wrong_password = Transaction::request(
            ClientOpcode::ChatJoin,
            2,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::string(ClientFieldId::Password, "nope"),
            ],
        );
        assert!(bob.on_chat_join(&wrong_password, &state).await.is_error());

        let right_password = Transaction::request(
            ClientOpcode::ChatJoin,
            3,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::string(ClientFieldId::Password, "hunter2"),
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
            ClientOpcode::ChatJoin,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
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
            ClientOpcode::ChatSubject,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::string(ClientFieldId::ChatSubject, "Nope"),
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
            ClientOpcode::ChatSubject,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::string(ClientFieldId::Password, "hunter2"),
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
            notify.string(ServerFieldId::Password, encoding),
            Some("hunter2".to_string())
        );
    }

    #[tokio::test]
    async fn setting_the_global_room_subject_requires_set_subject() {
        let guest = test_session("Guest", Access::empty());
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
        let encoding = text_encoding::DEFAULT;
        let mut admin = test_session("Admin", Access::empty());
        admin.access_extra.insert(AccessExtra::SET_SUBJECT);
        let state = SharedState::default();
        let mut rx = state.register_recipient(admin.user_id).await;

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

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a ChatSubject push");
        assert_eq!(
            notify.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(0)
        );
        assert_eq!(
            notify.string(ServerFieldId::ChatSubject, encoding),
            Some("Welcome".to_string())
        );
    }

    #[tokio::test]
    async fn inviting_a_disconnected_user_is_an_error() {
        let alice = test_session("Alice", Access::all());
        let state = SharedState::default();
        let chat_id = state.create_chat_room(1).await;

        let invite = Transaction::request(
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::u16(ClientFieldId::Uid, 2),
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
            ClientOpcode::ChatInvite,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        assert!(alice.on_chat_invite(&missing_uid, &state).await[0].is_error());

        let zero_chat_id = Transaction::request(
            ClientOpcode::ChatInvite,
            2,
            vec![
                Field::u32(ClientFieldId::ChatId, 0),
                Field::u16(ClientFieldId::Uid, 2),
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
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::u16(ClientFieldId::Uid, 2),
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
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::u16(ClientFieldId::Uid, 2),
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
            ClientOpcode::ChatInvite,
            1,
            vec![
                Field::u32(ClientFieldId::ChatId, chat_id),
                Field::u16(ClientFieldId::Uid, 2),
            ],
        );
        let replies = alice.on_chat_invite(&invite, &state).await;

        assert_eq!(replies.len(), 2);
        assert_eq!(
            replies[0].string(ServerFieldId::Text, encoding).unwrap(),
            "Bob does not accept private chats."
        );
        assert_eq!(
            replies[0].field(ServerFieldId::Options).unwrap().as_u16(),
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
        state.invite(chat_id, 2).await;

        let bob = Session {
            user_id: 2,
            ..test_session("Bob", Access::all())
        };
        let decline = Transaction::request(
            ClientOpcode::ChatDecline,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
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
            ClientOpcode::ChatDecline,
            1,
            vec![Field::u32(ClientFieldId::ChatId, chat_id)],
        );
        let replies = bob.on_chat_decline(&decline, &state).await;
        assert!(replies.is_empty());

        let notify = alice_rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Chat push notifying the room");
        assert_eq!(notify.r#type(), TransactionType::Push(ServerOpcode::Chat));
        assert_eq!(
            notify.string(ServerFieldId::Text, encoding).unwrap(),
            "\r<<< \"Bob\" declined invitation to chat >>>"
        );
        assert_eq!(
            notify.field(ServerFieldId::ChatId).unwrap().as_u32(),
            Some(chat_id)
        );
    }

    fn state_with_files(files: crate::files::InMemoryFileProvider) -> SharedState {
        SharedState::new(
            std::sync::Arc::new(crate::accounts::InMemoryAccountProvider::default()),
            std::sync::Arc::new(files),
            std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
            std::sync::Arc::new(crate::bans::InMemoryBanProvider::default()),
            std::sync::Arc::new(crate::flood::NoFloodPolicy),
        )
    }

    #[tokio::test]
    async fn file_get_denied_without_download_files() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::empty());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_get_replies_with_ref_and_total_size() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let reference = reply
            .field(ServerFieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();
        let total_size = reply
            .field(ServerFieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(
            total_size,
            5 + hotline_proto::htxf::FLAT_FILE_HEADER_LEN as u32
        );
        let file_size = reply
            .field(ServerFieldId::FileSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(file_size, 5);
        assert_ne!(file_size, total_size);

        let transfer = state.take_transfer(reference).await.unwrap();
        assert_eq!(transfer.direction, TransferDirection::Get);
        assert_eq!(transfer.size, 5);
        assert_eq!(transfer.resume_offset, 0);
    }

    /// Builds a `FileResumeData` (`HTLC_DATA_RFLT`) blob claiming
    /// `already_have` bytes of the `DATA` fork, matching the wire shape
    /// [`hotline_proto::htxf::decode_resume_data`] parses.
    fn resume_data(already_have: u32) -> [u8; 58] {
        hotline_proto::wire::PacketWriter::new()
            .bytes(b"RFLT")
            .u16(1)
            .bytes(&[0; 34])
            .u16(1)
            .bytes(b"DATA")
            .u32(already_have)
            .u32(0)
            .u32(0)
            .finish()
    }

    #[tokio::test]
    async fn file_get_with_resume_data_shrinks_the_transfer_and_records_the_offset() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(ClientFieldId::ResumeFileTransfer, resume_data(6)),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .field(ServerFieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(
            total_size,
            5 + hotline_proto::htxf::FLAT_FILE_HEADER_LEN as u32
        );
        let file_size = reply
            .field(ServerFieldId::FileSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(file_size, 11);

        let reference = reply
            .field(ServerFieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();
        let transfer = state.take_transfer(reference).await.unwrap();
        assert_eq!(transfer.size, 11);
        assert_eq!(transfer.resume_offset, 6);
    }

    #[tokio::test]
    async fn file_get_with_resume_data_past_eof_clamps_to_zero_remaining() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(ClientFieldId::ResumeFileTransfer, resume_data(999)),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .field(ServerFieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, hotline_proto::htxf::FLAT_FILE_HEADER_LEN as u32);
    }

    #[tokio::test]
    async fn file_get_with_malformed_resume_data_is_an_error() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(ClientFieldId::ResumeFileTransfer, vec![1, 2, 3]),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_get_missing_file_is_an_error() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![Field::string(ClientFieldId::FileName, "missing.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_list_of_a_dropbox_is_empty_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let list = Transaction::request(
            ClientOpcode::FileList,
            1,
            vec![Field::bytes(
                ClientFieldId::FilePath,
                HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(reply.fields.is_empty());
    }

    #[tokio::test]
    async fn file_list_of_a_dropbox_is_visible_with_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let list = Transaction::request(
            ClientOpcode::FileList,
            1,
            vec![Field::bytes(
                ClientFieldId::FilePath,
                HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 1);
    }

    #[tokio::test]
    async fn file_get_from_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let get = Transaction::request(
            ClientOpcode::FileGet,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "secret.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_in_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let delete = Transaction::request(
            ClientOpcode::FileDelete,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "secret.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::new(["Drop Box"]), "secret.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_getinfo_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let mut session = test_session("Alice", access);
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            ClientOpcode::FileGetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "secret.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            ClientOpcode::FileMkdir,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "Drop Box"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::default().encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "secret.txt"),
                Field::string(ClientFieldId::FileNewName, "renamed.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_move_into_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Drop Box"]), "placeholder.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let move_tx = Transaction::request(
            ClientOpcode::FileMove,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Drop Box"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_symlink_of_a_dropbox_target_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let symlink = Transaction::request(
            ClientOpcode::FileSymlink,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "secret.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::default().encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn file_put_outside_a_dropbox_is_denied_without_upload_anywhere() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = Transaction::request(
            ClientOpcode::FilePut,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_put_into_a_dropbox_is_allowed_without_upload_anywhere() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = Transaction::request(
            ClientOpcode::FilePut,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    hotline_proto::types::HlPath::new(["Uploads"])
                        .encode(encoding)
                        .unwrap(),
                ),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(ServerFieldId::TransferId).is_some());
    }

    #[tokio::test]
    async fn file_put_rejects_an_existing_file() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"existing");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            ClientOpcode::FilePut,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_removes_a_file() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let delete = Transaction::request(
            ClientOpcode::FileDelete,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            !state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_delete_denied_without_delete_files() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::DELETE_FILE);
        let session = test_session("Alice", access);

        let delete = Transaction::request(
            ClientOpcode::FileDelete,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_missing_file_is_an_error() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let delete = Transaction::request(
            ClientOpcode::FileDelete,
            1,
            vec![Field::string(ClientFieldId::FileName, "missing.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_creates_a_folder() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            ClientOpcode::FileMkdir,
            1,
            vec![Field::string(ClientFieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "NewFolder")
                .await
        );
    }

    #[tokio::test]
    async fn file_mkdir_denied_without_create_folders() {
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::CREATE_FOLDER);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            ClientOpcode::FileMkdir,
            1,
            vec![Field::string(ClientFieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_list_denied_without_access_extra() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::FILE_LIST);

        let list = Transaction::request(ClientOpcode::FileList, 1, vec![]);
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_getinfo_denied_without_access_extra() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            ClientOpcode::FileGetinfo,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_getinfo_returns_name_and_size() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            ClientOpcode::FileGetinfo,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply.string(ServerFieldId::FileName, encoding).unwrap(),
            "a.txt"
        );
        assert_eq!(
            reply.field(ServerFieldId::FileSize).and_then(Field::as_u32),
            Some(5)
        );
        // No real HFS metadata is persisted, so these are omitted rather
        // than sent zeroed.
        assert!(reply.field(ServerFieldId::FileType).is_none());
        assert!(reply.field(ServerFieldId::FileCreator).is_none());
        assert!(reply.field(ServerFieldId::FileDateCreate).is_none());
        assert!(reply.field(ServerFieldId::FileComment).is_none());
    }

    #[tokio::test]
    async fn file_getinfo_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            ClientOpcode::FileGetinfo,
            1,
            vec![Field::bytes(
                ClientFieldId::FilePath,
                HlPath::new(["a.txt"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply.string(ServerFieldId::FileName, encoding).unwrap(),
            "a.txt"
        );
    }

    #[tokio::test]
    async fn file_setinfo_renames_a_file() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::string(ClientFieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "b.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_setinfo_denied_without_rename_files() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::RENAME_FILE);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::string(ClientFieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_without_a_new_name_is_a_no_op() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_setinfo_sets_a_comment_returned_by_getinfo() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::string(ClientFieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let getinfo = Transaction::request(
            ClientOpcode::FileGetinfo,
            1,
            vec![Field::string(ClientFieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            reply.string(ServerFieldId::FileComment, encoding),
            Some("a note".to_string())
        );
    }

    #[tokio::test]
    async fn file_setinfo_comment_denied_without_set_file_comment() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FILE_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::string(ClientFieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_folder_comment_denied_without_set_folder_comment() {
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Folder"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FOLDER_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            ClientOpcode::FileSetinfo,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "Folder"),
                Field::string(ClientFieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::files::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            ClientOpcode::FileMkdir,
            1,
            vec![Field::bytes(
                ClientFieldId::FilePath,
                HlPath::new(["NewFolder"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "NewFolder")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_relocates_a_file() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Sub"]), ".keep", *b"");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = Transaction::request(
            ClientOpcode::FileMove,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_to_root_treats_an_absent_new_path_as_root() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Sub"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = Transaction::request(
            ClientOpcode::FileMove,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FilePath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
                // No FileNewPath field: the destination is the root directory.
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_denied_without_move_files() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MOVE_FILE);
        let session = test_session("Alice", access);

        let move_tx = Transaction::request(
            ClientOpcode::FileMove,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_symlink_denied_without_make_aliases() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MAKE_ALIAS);
        let session = test_session("Alice", access);

        let symlink = Transaction::request(
            ClientOpcode::FileSymlink,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_symlink_creates_an_alias_reachable_at_the_new_location() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::files::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let symlink = Transaction::request(
            ClientOpcode::FileSymlink,
            1,
            vec![
                Field::string(ClientFieldId::FileName, "a.txt"),
                Field::bytes(
                    ClientFieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn news_mkdir_and_mkcategory_create_items_listdir_can_see() {
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            ClientOpcode::NewsMkdir,
            1,
            vec![Field::string(ClientFieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_news_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let mkcategory = Transaction::request(
            ClientOpcode::NewsMkcategory,
            2,
            vec![Field::string(ClientFieldId::NewsCatName, "General")],
        );
        let [reply] = session
            .on_news_mkcategory(&mkcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listdir = Transaction::request(ClientOpcode::NewsListdir, 3, vec![]);
        let [reply] = session
            .on_news_listdir(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 2);
    }

    #[tokio::test]
    async fn news_mkdir_denied_without_news_create_fldr() {
        let state = SharedState::default();
        let mut access = Access::all();
        access.remove(Access::NEWS_CREATE_FLDR);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            ClientOpcode::NewsMkdir,
            1,
            vec![Field::string(ClientFieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_news_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn news_delete_removes_a_bundle_and_requires_news_delete_fldr() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());
        state
            .news_mkdir(&HlPath::default(), "Bundle")
            .await
            .unwrap();

        let mut denied_access = Access::all();
        denied_access.remove(Access::NEWS_DELETE_FLDR);
        let denied_session = test_session("Bob", denied_access);
        let delete = Transaction::request(
            ClientOpcode::NewsDelete,
            1,
            vec![Field::bytes(
                ClientFieldId::NewsPath,
                HlPath::new(["Bundle"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = denied_session
            .on_news_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());

        let [reply] = session
            .on_news_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listdir = Transaction::request(ClientOpcode::NewsListdir, 2, vec![]);
        let [reply] = session
            .on_news_listdir(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(reply.fields.len(), 0);
    }

    #[tokio::test]
    async fn news_postthread_getthread_and_listcategory_round_trip_an_article() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();
        let path_field = || {
            Field::bytes(
                ClientFieldId::NewsPath,
                HlPath::new(["General"]).encode(encoding).unwrap(),
            )
        };

        let post = Transaction::request(
            ClientOpcode::NewsPostthread,
            1,
            vec![
                path_field(),
                Field::string(ClientFieldId::NewsArtTitle, "Hello"),
                Field::string(ClientFieldId::NewsArtDataFlavor, "text/plain"),
                Field::bytes(ClientFieldId::NewsArtData, b"body text".to_vec()),
            ],
        );
        let [reply] = session
            .on_news_postthread(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listcategory =
            Transaction::request(ClientOpcode::NewsListcategory, 2, vec![path_field()]);
        let [reply] = session
            .on_news_listcategory(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 1);

        let getthread = Transaction::request(
            ClientOpcode::NewsGetthread,
            3,
            vec![
                path_field(),
                Field::u32(ClientFieldId::NewsArtId, 1),
                Field::string(ClientFieldId::NewsArtDataFlavor, "text/plain"),
            ],
        );
        let [reply] = session
            .on_news_getthread(&getthread, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let data = reply
            .field(ServerFieldId::NewsPost)
            .map(|f| f.as_bytes(encoding).into_owned());
        assert_eq!(data, Some(b"body text".to_vec()));
        assert_eq!(
            reply.string(ServerFieldId::NewsSubject, encoding),
            Some("Hello".to_string())
        );

        let delete = Transaction::request(
            ClientOpcode::NewsDeletethread,
            4,
            vec![path_field(), Field::u32(ClientFieldId::NewsArtId, 1)],
        );
        let [reply] = session
            .on_news_deletethread(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listcategory =
            Transaction::request(ClientOpcode::NewsListcategory, 5, vec![path_field()]);
        let [reply] = session
            .on_news_listcategory(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn news_postthread_denied_without_post_news_art() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let mut access = Access::all();
        access.remove(Access::POST_NEWS_ART);
        let session = test_session("Alice", access);
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();

        let post = Transaction::request(
            ClientOpcode::NewsPostthread,
            1,
            vec![
                Field::bytes(
                    ClientFieldId::NewsPath,
                    HlPath::new(["General"]).encode(encoding).unwrap(),
                ),
                Field::string(ClientFieldId::NewsArtTitle, "Hello"),
                Field::bytes(ClientFieldId::NewsArtData, b"x".to_vec()),
            ],
        );
        let [reply] = session
            .on_news_postthread(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }
}
