//! The per-connection `Session`: handshake, run loop, and dispatch.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, Stream, StreamExt};
use num_enum::{FromPrimitive, IntoPrimitive};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::{Encoding, EncodingHandle};
use hotline_proto::types::{UserFlags, UserNameWithInfo};
use hotline_proto::FieldId;
use hotline_proto::{ClientHandshake, Field, ServerHandshake, Transaction};

use crate::account::AccessExtra;
use crate::banner::BannerKind;
use crate::error::{Result, ServerError};
use crate::flood::FloodDecision;
use crate::server::ServerConfig;
use crate::state::{
    AccessUpdate, ConsoleUpdate, RecipientMessage, SharedState, UserEntry, IDLE_THRESHOLD,
};

use super::options::UserChangeOptions;

/// `Options` (113) values on `myTran_SendInstantMsg` (108)/`myTran_ServerMsg` (104):
/// which kind of instant message this is, or which kind of bounce a
/// refusal/automatic-response reply carries.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub(super) enum MsgOption {
    UserMessage = 1,
    RefuseMessage = 2,
    RefuseChat = 3,
    AutomaticResponse = 4,

    /// Not recognized by this build.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// `Options` (113) values on `myTran_DisconnectUser` (110): whether the kick
/// also imposes an address ban, and for how long.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KickBan {
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

/// Cap on the classic news text in one `myTran_GetMsgs` (101) reply,
/// the most a `u16` field length allows.
pub(super) const MAX_FLAT_NEWS_LEN: usize = u16::MAX as usize;

/// Duration of a temporary kick-to-ban (`Options` (113) value 1). Not yet
/// configurable; a fixed default until this gets a `ServerConfig` knob.
pub(super) const TEMP_BAN_DURATION: Duration = Duration::from_secs(30 * 60);

/// How long a kicked client may drain its `myTran_DisconnectMsg` (111)
/// notice. openline closes at once.
const KICK_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Reads and discards `stream` until it ends or `timeout` elapses.
async fn drain_with_timeout<S: Stream + Unpin>(mut stream: S, timeout: Duration) {
    let _ = tokio::time::timeout(timeout, async { while stream.next().await.is_some() {} }).await;
}

/// Which opcodes a session may currently send.
pub(super) enum SessionState {
    /// Before `myTran_Login` (107) or `myTran_Agreed` (121) complete login.
    Agreement,
    /// Login has completed.
    Joined,
}

/// The write half of a session's framed connection.
type SessionWriter = SplitSink<Framed<TcpStream, TransactionCodec>, Transaction>;

/// The read half of a session's framed connection.
type SessionReader = SplitStream<Framed<TcpStream, TransactionCodec>>;

/// How a session's dispatch loop ended.
enum SessionEnd {
    /// Close the connection at once.
    Closed,
    /// Drain the reader for [`KICK_DRAIN_TIMEOUT`] before closing.
    Kicked,
}

/// A single client connection: handshake state plus the transaction
/// dispatch loop. Not `Clone`, one per socket.
pub struct Session {
    pub(super) user_id: u16,
    pub(super) name: String,
    /// Account login this session authenticated as, empty until login.
    pub(super) login: String,
    /// The account's own registered display name, empty until login. See
    /// [`crate::state::UserEntry::account_name`].
    pub(super) account_name: String,
    /// Privileges granted at login.
    pub(super) access: Access,
    /// Server-side-only account settings; no client opcode reads or sets them.
    pub(super) access_extra: AccessExtra,
    /// Client-reported protocol version (`myField_Vers`, field
    /// 160) from login, `None` if it never sent one.
    pub(super) client_version: Option<u16>,
    pub(super) state: SessionState,
    /// Whether the last broadcast `UserChange` marked this session away.
    pub(super) is_away: bool,
    /// Refuses incoming private messages, set via `myTran_SetClientUserInfo` (304).
    pub(super) refuses_pm: bool,
    /// Refuses incoming private chat invites, set via `myTran_SetClientUserInfo` (304).
    pub(super) refuses_chat: bool,
    /// Custom automatic-response text sent to private-message senders, `None` when off.
    pub(super) automatic_response: Option<String>,
    /// Remote socket address this connection was accepted from.
    pub(super) remote_addr: SocketAddr,
    /// When this connection was accepted.
    pub(super) connected_at: Instant,
    /// This connection's active text encoding, shared with the write half's `TransactionCodec`.
    pub(super) encoding: EncodingHandle,
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

        let outcome = Self::dispatch(
            &mut session,
            &state,
            &config,
            &mut writer,
            &mut reader,
            &mut rx,
        )
        .await;
        // Runs on every exit from `dispatch`, including an early `?`.
        Self::leave_server(&session, &state).await;
        if let Ok(SessionEnd::Kicked) = outcome {
            // Give the client a bounded chance to read the notice.
            drain_with_timeout(&mut reader, KICK_DRAIN_TIMEOUT).await;
        }
        outcome.map(|_| ())
    }

    /// Dispatches this connection's traffic until it ends. The caller owns
    /// the cleanup.
    async fn dispatch(
        session: &mut Session,
        state: &SharedState,
        config: &ServerConfig,
        writer: &mut SessionWriter,
        reader: &mut SessionReader,
        rx: &mut mpsc::Receiver<RecipientMessage>,
    ) -> Result<SessionEnd> {
        let idle_sleep = tokio::time::sleep(IDLE_THRESHOLD);
        tokio::pin!(idle_sleep);

        loop {
            tokio::select! {
                // Traffic from this client.
                frame = reader.next() => {
                    let Some(tx) = frame.transpose()? else { break Ok(SessionEnd::Closed) }; // clean EOF
                    state.touch_user(session.user_id).await;
                    idle_sleep.as_mut().reset(tokio::time::Instant::now() + IDLE_THRESHOLD);
                    if session.is_away {
                        session.set_away(false, state).await;
                    }
                    for reply in session.handle(&tx, state, config).await {
                        writer.send(reply).await?;
                    }
                }
                // Fires after IDLE_THRESHOLD with no request from this client.
                () = &mut idle_sleep, if !session.is_away && matches!(session.state, SessionState::Joined) => {
                    session.set_away(true, state).await;
                }
                // Everything server-initiated for this session.
                msg = rx.recv() => {
                    match msg {
                        Some(RecipientMessage::Push(tx)) => {
                            let opcode = tx.r#type();
                            let is_kick = opcode == TransactionType::Push(Opcode::Politequit);
                            if session.may_receive(opcode) {
                                writer.send(tx).await?;
                            }
                            if is_kick {
                                break Ok(SessionEnd::Kicked);
                            }
                        }
                        Some(RecipientMessage::AccessUpdate(update)) => {
                            session.apply_access_update(update, state).await;
                        }
                        Some(RecipientMessage::Console(update)) => {
                            session.apply_console_update(update, state).await;
                        }
                        Some(RecipientMessage::Disconnect) => break Ok(SessionEnd::Closed),
                        None => break Ok(SessionEnd::Closed),
                    }
                }
            }
        }
    }

    /// Removes a session from cross-session state and notifies other users.
    async fn leave_server(session: &Session, state: &SharedState) {
        state.remove_recipient(session.user_id).await;
        state.remove_user(session.user_id).await;
        state.reset_flood(session.user_id).await;
        for (chat_id, remaining) in state.leave_all_chat_rooms(session.user_id).await {
            let notify = Transaction::push(
                Opcode::ChatUserPart,
                vec![
                    Field::u32(FieldId::ChatId, chat_id),
                    Field::u16(FieldId::UserId, session.user_id),
                ],
            );
            for member in remaining {
                state.send_to(member, notify.clone()).await;
            }
        }
        let leave = Transaction::push(
            Opcode::UserPart,
            vec![Field::u16(FieldId::UserId, session.user_id)],
        );
        state.publish(leave).await;
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
    const fn may_receive(&self, opcode: TransactionType) -> bool {
        match opcode {
            TransactionType::Push(Opcode::NotifyChangeUser)
            | TransactionType::Push(Opcode::UserPart) => {
                self.access_extra.contains(AccessExtra::USER_GETLIST)
            }
            TransactionType::Push(Opcode::ChatMsg) => self.access.contains(Access::READ_CHAT),
            TransactionType::Push(Opcode::NewMsg) => self.access.contains(Access::NEWS_READ_ART),
            _ => true,
        }
    }

    /// Handle one request, returning zero or more transactions in reply.
    pub(super) async fn handle(
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
                        Opcode::ServerMsg,
                        vec![Field::string(FieldId::Text, reason)],
                    )];
                }
            }
            FloodDecision::Allow => {}
        }

        match self.state {
            SessionState::Agreement => self.handle_agreement(opcode, tx, state, config).await,
            SessionState::Joined => self.handle_joined(opcode, tx, state, config).await,
        }
    }

    /// Opcodes valid before login completes.
    async fn handle_agreement(
        &mut self,
        opcode: Opcode,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        match opcode {
            /* 107 */ Opcode::Login => self.on_login(tx, state, config).await,
            /* 121 */ Opcode::Agreed => self.on_agreed(tx, state).await,
            /* 500 */ Opcode::Ping => vec![tx.reply(vec![])],
            other => {
                tracing::debug!(?other, "opcode not allowed before login completes");
                vec![tx.reply_error("please finish logging in first")]
            }
        }
    }

    /// Opcodes valid once a session has joined.
    async fn handle_joined(
        &mut self,
        opcode: Opcode,
        tx: &Transaction,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        match opcode {
            /* 101 */ Opcode::NewsfileGet => self.on_newsfile_get(tx, state).await,
            /* 103 */ Opcode::OldPostNews => self.on_newsfile_post(tx, state).await,
            /* 105 */ Opcode::ChatSend => self.on_chat(tx, state, config).await,
            /* 107 */ Opcode::Login => vec![tx.reply_error("already logged in")],
            /* 108 */ Opcode::SendInstantMsg => self.on_private_msg(tx, state).await,
            /* 110 */ Opcode::UserKick => self.on_user_kick(tx, state).await,
            /* 112 */ Opcode::ChatCreate => vec![self.on_chat_create(tx, state).await],
            /* 113 */ Opcode::ChatInvite => self.on_chat_invite(tx, state).await,
            /* 114 */ Opcode::ChatDecline => self.on_chat_decline(tx, state).await,
            /* 115 */ Opcode::ChatJoin => vec![self.on_chat_join(tx, state).await],
            /* 116 */ Opcode::ChatPart => self.on_chat_part(tx, state).await,
            /* 120 */ Opcode::SetChatSubject => self.on_chat_subject(tx, state).await,
            /* 121 */ Opcode::Agreed => vec![tx.reply_error("already logged in")],
            /* 200 */ Opcode::FileList => self.on_get_file_name_list(tx, state).await,
            /* 202 */ Opcode::FileGet => self.on_file_get(tx, state).await,
            /* 203 */ Opcode::FilePut => self.on_file_put(tx, state).await,
            /* 204 */ Opcode::FileDelete => self.on_file_delete(tx, state).await,
            /* 205 */ Opcode::FileMkdir => self.on_file_mkdir(tx, state).await,
            /* 206 */ Opcode::FileGetinfo => self.on_file_getinfo(tx, state).await,
            /* 207 */ Opcode::FileSetinfo => self.on_file_setinfo(tx, state).await,
            /* 208 */ Opcode::FileMove => self.on_file_move(tx, state).await,
            /* 209 */ Opcode::FileSymlink => self.on_file_symlink(tx, state).await,
            /* 212 */ Opcode::BannerGet => self.on_banner_get(tx, state).await,
            /* 300 */ Opcode::UserGetlist => self.on_user_get_list(tx, state).await,
            /* 370 */ Opcode::NewsListdir => self.on_news_listdir(tx, state).await,
            /* 371 */
            Opcode::NewsListcategory => self.on_news_listcategory(tx, state).await,
            /* 380 */ Opcode::NewsDelete => self.on_news_delete(tx, state).await,
            /* 381 */ Opcode::NewsMkdir => self.on_news_mkdir(tx, state).await,
            /* 382 */
            Opcode::NewsMkcategory => self.on_news_mkcategory(tx, state).await,
            /* 400 */ Opcode::NewsGetthread => self.on_news_getthread(tx, state).await,
            /* 410 */
            Opcode::NewsPostthread => self.on_news_postthread(tx, state).await,
            /* 411 */
            Opcode::NewsDeletethread => self.on_news_deletethread(tx, state).await,
            /* 303 */ Opcode::UserGetinfo => self.on_user_get_info(tx, state).await,
            /* 304 */ Opcode::SetClientUserInfo => self.on_user_change(tx, state).await,
            /* 348 */ Opcode::GetUserList => self.on_get_user_list(tx, state).await,
            /* 349 */ Opcode::SetUserList => self.on_set_user_list(tx, state).await,
            /* 350 */ Opcode::AccountCreate => self.on_account_create(tx, state).await,
            /* 351 */ Opcode::AccountDelete => self.on_account_delete(tx, state).await,
            /* 352 */ Opcode::AccountRead => self.on_account_read(tx, state).await,
            /* 353 */ Opcode::AccountModify => self.on_account_modify(tx, state).await,
            /* 355 */ Opcode::MsgBroadcast => self.on_msg_broadcast(tx, state).await,
            /* 500 */ Opcode::Ping => vec![tx.reply(vec![])],
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
            Field::u16(FieldId::Version, 190),
            Field::u16(FieldId::Bannerid, 0),
            Field::string(FieldId::Servername, config.server_name.clone()),
        ];
        let mut replies = vec![tx.reply(fields)];

        // No myField_Vers (160) counts as pre-150.
        let modern = self.client_version.unwrap_or(0) >= 150;

        // DONT_SHOW_AGREEMENT skips the real text, not just its display.
        let show_agreement = !self.access.contains(Access::NO_AGREEMENT);
        let have_agreement = !config.agreement.is_empty();
        if show_agreement && have_agreement {
            replies.push(Transaction::push(
                Opcode::Agreement,
                vec![Field::string(FieldId::Text, config.agreement.clone())],
            ));
        } else if modern {
            replies.push(Transaction::push(
                Opcode::Agreement,
                vec![Field::bytes(FieldId::Noagreement, vec![0, 1])],
            ));
        }

        // A name plus non-zero icon in Login, or a pre-150 client, joins right away.
        let icon = tx.icon().unwrap_or(0);
        let name = tx.name(encoding);
        if (name.is_some() && icon != 0) || !modern {
            let selfinfo = self.complete_join(name, icon, state).await;
            replies.push(selfinfo);
            if let Some(banner) = self.banner_push(state).await {
                replies.push(banner);
            }
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
                    Opcode::Politequit,
                    vec![Field::string(FieldId::Text, reason)],
                ),
            )
            .await;
        vec![tx.reply_error(reason)]
    }

    /// Complete login after the client accepts the agreement (`myTran_Agreed`, 121).
    async fn on_agreed(&mut self, tx: &Transaction, state: &SharedState) -> Vec<Transaction> {
        let icon = tx.icon().unwrap_or(0);
        let encoding = self.encoding();
        let name = tx.name(encoding);
        let selfinfo = self.complete_join(name, icon, state).await;
        let mut replies = vec![tx.reply(vec![]), selfinfo];
        if let Some(banner) = self.banner_push(state).await {
            replies.push(banner);
        }
        replies
    }

    /// If a banner is configured and this client's reported version can
    /// display one (`myField_Vers` >= 151), build the
    /// `myTran_ServerBanner` push: `BannerType`, plus `BannerUrl` for a URL
    /// banner. An image banner sends no URL; the client is expected to
    /// follow up with `myTran_DownloadBanner`.
    async fn banner_push(&self, state: &SharedState) -> Option<Transaction> {
        if self.client_version.unwrap_or(0) < 151 {
            return None;
        }
        let kind = state.banner_kind().await?;
        let mut fields = vec![Field::bytes(FieldId::BannerType, kind.type_tag().to_vec())];
        if let BannerKind::Url(url) = &kind {
            fields.push(Field::string(FieldId::BannerUrl, url.clone()));
        }
        Some(Transaction::push(Opcode::Banner, fields))
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
            flags: self.user_flags(),
            name: self.name.clone(),
        };

        Transaction::push(
            Opcode::UserSelfinfo,
            vec![
                Field::u64(FieldId::Access, self.access.bits()),
                Field::bytes(FieldId::UserList, self_entry.encode(self.encoding())),
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
            tx.field(FieldId::Options)
                .and_then(Field::as_u16)
                .unwrap_or(0),
        );
        self.refuses_pm = options.contains(UserChangeOptions::REFUSE_PM);
        self.refuses_chat = options.contains(UserChangeOptions::REFUSE_CHAT);
        self.automatic_response = options.contains(UserChangeOptions::AUTO_RESPONSE).then(|| {
            tx.string(FieldId::AutomaticResponse, encoding)
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
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserId, self.user_id),
                Field::u16(FieldId::Icon, icon),
                Field::string(FieldId::Name, self.name.clone()),
                Field::u16(FieldId::Color, self.user_flags().bits()),
            ],
        );
        if exclude_self {
            state.announce(self.user_id, notify).await;
        } else {
            state.publish(notify).await;
        }
    }

    /// Applies a live account-modify change for this session's login.
    pub(super) async fn apply_access_update(&mut self, update: AccessUpdate, state: &SharedState) {
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
        let icon = self.current_icon(state).await;
        self.announce_user_change(icon, state, false).await;
    }

    /// Applies a console-command-driven live update (see
    /// [`crate::ChatCommandDispatcher`]) to this exact session.
    pub(super) async fn apply_console_update(
        &mut self,
        update: ConsoleUpdate,
        state: &SharedState,
    ) {
        if let Some(access) = update.access {
            self.access = access;
        }
        if let Some(access_extra) = update.access_extra {
            self.access_extra = access_extra;
        }
        if let Some(name) = update.name {
            self.name = name;
        }
        let icon = match update.icon {
            Some(icon) => icon,
            None => self.current_icon(state).await,
        };
        self.announce_user_change(icon, state, false).await;
    }

    /// This session's icon from `SharedState`, `0` if it has no entry.
    async fn current_icon(&self, state: &SharedState) -> u16 {
        state.user(self.user_id).await.map_or(0, |e| e.icon)
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
                Opcode::NotifyChangeUser,
                vec![
                    Field::u16(FieldId::UserId, self.user_id),
                    Field::u16(FieldId::Icon, entry.icon),
                    Field::string(FieldId::Name, entry.name),
                    Field::u16(FieldId::Color, self.user_flags().bits()),
                ],
            ))
            .await;
    }

    /// This connection's active text encoding.
    pub(super) fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }

    /// This session's own admin + away flags, for `UserChange`/`UserSelfinfo`.
    pub(super) fn user_flags(&self) -> UserFlags {
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::Account;

    use crate::session::test_support::*;
    use hotline_proto::text_encoding;

    #[test]
    fn may_receive_gates_presence_pushes_on_user_getlist_not_other_traffic() {
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::USER_GETLIST);

        assert!(!session.may_receive(TransactionType::Push(Opcode::NotifyChangeUser)));
        assert!(!session.may_receive(TransactionType::Push(Opcode::UserPart)));
        assert!(session.may_receive(TransactionType::Push(Opcode::ChatMsg)));

        session.access_extra.insert(AccessExtra::USER_GETLIST);
        assert!(session.may_receive(TransactionType::Push(Opcode::NotifyChangeUser)));
        assert!(session.may_receive(TransactionType::Push(Opcode::UserPart)));
    }

    #[test]
    fn may_receive_gates_chat_on_read_chat() {
        let mut session = test_session("Alice", Access::all());
        session.access.remove(Access::READ_CHAT);
        assert!(!session.may_receive(TransactionType::Push(Opcode::ChatMsg)));

        session.access.insert(Access::READ_CHAT);
        assert!(session.may_receive(TransactionType::Push(Opcode::ChatMsg)));
    }

    #[test]
    fn may_receive_gates_newsfile_post_on_read_news() {
        let mut session = test_session("Alice", Access::all());
        session.access.remove(Access::NEWS_READ_ART);
        assert!(!session.may_receive(TransactionType::Push(Opcode::NewMsg)));

        session.access.insert(Access::NEWS_READ_ART);
        assert!(session.may_receive(TransactionType::Push(Opcode::NewMsg)));
    }

    #[tokio::test]
    async fn login_reply_advertises_server_version_190() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = Transaction::request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0].field(FieldId::Version).and_then(Field::as_u16),
            Some(190)
        );
    }

    #[tokio::test]
    async fn login_reply_sends_community_banner_id_zero() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = Transaction::request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0].field(FieldId::Bannerid).and_then(Field::as_u16),
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

        let login = Transaction::request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0].string(FieldId::Servername, encoding),
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
            Opcode::Login,
            1,
            vec![
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        session.on_login(&login, &state, &config).await;

        let notify = observer
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        assert_eq!(notify.string(FieldId::Name, encoding).unwrap(), "Alice");
        assert_eq!(
            notify.field(FieldId::Icon).and_then(Field::as_u16),
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
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "alice"),
                Field::obfuscated_string(FieldId::Password, "hunter2"),
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
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "ALICE"),
                Field::obfuscated_string(FieldId::Password, "hunter2"),
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
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::Login, "alice")],
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
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "alice"),
                Field::obfuscated_string(FieldId::Password, "wrong"),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
        let kick = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a Politequit push");
        assert_eq!(kick.r#type(), TransactionType::Push(Opcode::Politequit));
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
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "alice"),
                Field::obfuscated_string(FieldId::Password, "anything"),
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
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::Login, "nobody")],
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

        let login = Transaction::request(Opcode::Login, 1, vec![]);
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
        let login = Transaction::request(Opcode::Login, 1, vec![Field::u16(FieldId::Version, 151)]);
        session.on_login(&login, &state, &config).await;

        assert!(bystander.try_recv().is_err());
        assert!(matches!(session.state, SessionState::Agreement));

        let agreed = Transaction::request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        session.on_agreed(&agreed, &state).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("agreed should announce a join");
        assert_eq!(notify.string(FieldId::Name, encoding).unwrap(), "Alice");
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
        let login = Transaction::request(Opcode::Login, 1, vec![]);
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
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 151),
                Field::string(FieldId::Name, "Alice"),
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
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 151),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
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
    async fn login_push_carries_image_banner_type_with_no_url() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let config = ServerConfig::default();

        let login = Transaction::request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 151),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let banner = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::Banner))
            .expect("expected a myTran_ServerBanner push");
        assert_eq!(
            banner
                .field(FieldId::BannerType)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b"JPEG"
        );
        assert!(banner.field(FieldId::BannerUrl).is_none());
    }

    #[tokio::test]
    async fn login_push_carries_url_banner_type_and_url() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(crate::banner::UrlBannerProvider::new(
            "https://example.com/banner.jpg",
        )));
        let config = ServerConfig::default();

        let login = Transaction::request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 151),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let banner = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::Banner))
            .expect("expected a myTran_ServerBanner push");
        assert_eq!(
            banner
                .field(FieldId::BannerType)
                .unwrap()
                .as_bytes(encoding)
                .as_ref(),
            b"URL "
        );
        assert_eq!(
            banner.string(FieldId::BannerUrl, encoding),
            Some("https://example.com/banner.jpg".to_string())
        );
    }

    #[tokio::test]
    async fn login_push_omitted_below_client_version_151() {
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let config = ServerConfig::default();

        let login = Transaction::request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 150),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::Banner)));
    }

    #[tokio::test]
    async fn login_push_omitted_when_no_banner_configured() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = Transaction::request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Version, 151),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::Banner)));
    }

    #[tokio::test]
    async fn modern_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: String::new(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(Opcode::Login, 1, vec![Field::u16(FieldId::Version, 151)]);
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::Agreement))
            .expect("modern client should get a Noagreement marker");
        assert!(agreement.field(FieldId::Noagreement).is_some());
    }

    #[tokio::test]
    async fn pre_150_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: String::new(),
            ..ServerConfig::default()
        };

        let login = Transaction::request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert!(
            !replies
                .iter()
                .any(|tx| tx.r#type() == TransactionType::Push(Opcode::Agreement)),
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
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "skipper"),
                Field::u16(FieldId::Version, 151),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::Agreement))
            .expect("modern client still gets a no-agreement marker");
        assert!(
            agreement.field(FieldId::Noagreement).is_some(),
            "DONT_SHOW_AGREEMENT must skip the real text, not just its display"
        );
        assert!(agreement.field(FieldId::Text).is_none());
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
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::Login, "skipper")],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::Agreement)),);
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
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "hi")],
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
            Opcode::Agreed,
            1,
            vec![
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let selfinfo = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::UserSelfinfo))
            .expect("on_agreed should push UserSelfinfo");
        let entry = selfinfo
            .field(FieldId::UserList)
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
            Opcode::Agreed,
            1,
            vec![
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Icon, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let selfinfo = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::UserSelfinfo))
            .expect("on_agreed should push UserSelfinfo");
        assert_eq!(
            selfinfo.field(FieldId::Access).and_then(Field::as_u64),
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

        let login =
            Transaction::request(Opcode::Login, 1, vec![Field::string(FieldId::Name, "Mod")]);
        session.on_login(&login, &state, &config).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        let color = notify
            .field(FieldId::Color)
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
            .field(FieldId::Color)
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
            .field(FieldId::Color)
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
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::Name, "Alice".to_string()),
                Field::u16(FieldId::Icon, 0),
                Field::u16(
                    FieldId::Options,
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
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::Name, "Alice".to_string()),
                Field::u16(FieldId::Icon, 0),
                Field::u16(FieldId::Options, UserChangeOptions::AUTO_RESPONSE.bits()),
                Field::string(FieldId::AutomaticResponse, "Back in five."),
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
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::Name, "Alice".to_string()),
                Field::u16(FieldId::Icon, 0),
                Field::u16(FieldId::Options, UserChangeOptions::AUTO_RESPONSE.bits()),
            ],
        );
        session.on_user_change(&request, &state).await;

        assert_eq!(
            session.automatic_response.as_deref(),
            Some("I'm not able to respond right now. Please try later!")
        );
    }

    #[tokio::test]
    async fn drain_with_timeout_returns_once_stream_ends() {
        let stream = futures::stream::iter([1, 2, 3]);
        drain_with_timeout(stream, Duration::from_secs(60)).await;
    }

    #[tokio::test(start_paused = true)]
    async fn drain_with_timeout_gives_up_on_a_stuck_stream() {
        // Stands in for a client that never drains its kick notice.
        let stuck = futures::stream::pending::<()>();
        drain_with_timeout(stuck, KICK_DRAIN_TIMEOUT).await;
    }

    /// A malformed transaction leaves `run` through `?` and must still
    /// unregister the session.
    #[tokio::test]
    async fn a_malformed_transaction_still_unregisters_the_session() {
        use tokio::net::TcpListener;

        let state = SharedState::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server_state = state.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            Session::run(stream, server_state, ServerConfig::default()).await
        });

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        client
            .write_all(&ClientHandshake::default().encode())
            .await
            .unwrap();
        let mut reply = [0u8; ServerHandshake::LEN];
        client.read_exact(&mut reply).await.unwrap();

        // A 20-byte header declaring len == len2 == 0.
        client.write_all(&[0u8; 20]).await.unwrap();

        assert!(server.await.unwrap().is_err());
        assert_eq!(state.recipient_count().await, 0);
        assert!(state.users().await.is_empty());
    }
}
