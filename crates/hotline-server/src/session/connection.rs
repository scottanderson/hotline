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
use hotline_proto::consts::Access;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::{self, Encoding, EncodingHandle};
use hotline_proto::types::{UserChangeOptions, UserFlags, UserNameWithInfo};
use hotline_proto::FieldId;
use hotline_proto::{ClientHandshake, Field, Received, ServerHandshake, Transaction};

use crate::account::AccessExtra;
use crate::activity::{Activity, ActivityEntry};
use crate::banner::BannerKind;
use crate::error::{Result, ServerError};
use crate::flood::{FloodDecision, FloodEvent};
use crate::server::ServerConfig;
use crate::state::{
    AccessUpdate, ConsoleUpdate, RecipientMessage, SharedState, UserEntry, IDLE_THRESHOLD,
};

/// [`myField_Options`] (113) values on
/// [`myTran_SendInstantMsg`] (108)/[`myTran_ServerMsg`] (104): which kind of
/// instant message this is, or which kind of bounce a
/// refusal/automatic-response reply carries.
///
/// [`myField_Options`]: hotline_proto::FieldId::Options
/// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
/// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
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

/// [`myField_Options`] (113) values on [`myTran_DisconnectUser`] (110): whether the
/// kick also imposes an address ban, and for how long.
///
/// [`myField_Options`]: hotline_proto::FieldId::Options
/// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
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

/// Cap on the classic news text in one [`myTran_GetMsgs`] (101) reply,
/// the most a `u16` field length allows.
///
/// [`myTran_GetMsgs`]: hotline_proto::Opcode::GetMsgs
pub(super) const MAX_FLAT_NEWS_LEN: usize = u16::MAX as usize;

/// How long a kicked client may drain its [`myTran_DisconnectMsg`] (111)
/// notice. openline closes at once.
///
/// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
const KICK_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// The longest [`myField_AutomaticResponse`] (215) kept, in bytes.
///
/// [`myField_AutomaticResponse`]: hotline_proto::FieldId::AutomaticResponse
const MAX_AUTOMATIC_RESPONSE: usize = 127;

/// Reads and discards `stream` until it ends or `timeout` elapses.
async fn drain_with_timeout<S: Stream + Unpin>(mut stream: S, timeout: Duration) {
    let _ = tokio::time::timeout(timeout, async { while stream.next().await.is_some() {} }).await;
}

/// Which opcodes a session may currently send.
pub(super) enum SessionState {
    /// Before [`myTran_Login`] (107) or [`myTran_Agreed`] (121) complete login.
    ///
    /// [`myTran_Login`]: hotline_proto::Opcode::Login
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
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
    pub(super) name: Box<str>,
    /// The cleaned name the client last sent. The user name while the
    /// user holds `ANY_NAME`.
    pub(super) real_name: Box<str>,
    /// Account login this session authenticated as, empty until login.
    pub(super) login: Box<str>,
    /// The account's own registered display name, empty until login. See
    /// [`crate::state::UserEntry::account_name`].
    pub(super) account_name: Box<str>,
    /// Privileges granted at login.
    pub(super) access: Access,
    /// Server-side-only account settings; no client opcode reads or sets them.
    pub(super) access_extra: AccessExtra,
    /// Client-reported protocol version, [`myField_Vers`] (160), from
    /// login. `None` if it never sent one.
    ///
    /// [`myField_Vers`]: hotline_proto::FieldId::Vers
    pub(super) client_version: Option<u16>,
    pub(super) state: SessionState,
    /// Whether the last broadcast `UserChange` marked this session away.
    pub(super) is_away: bool,
    /// Refuses incoming private messages, set via [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub(super) refuses_pm: bool,
    /// Refuses incoming private chat invites, set via [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub(super) refuses_chat: bool,
    /// Custom automatic-response text sent to private-message senders, `None` when off.
    pub(super) automatic_response: Option<Box<str>>,
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
        let login_deadline = tokio::time::Instant::now() + config.login_timeout;
        let Ok(handshake) =
            tokio::time::timeout_at(login_deadline, Self::handshake(&mut stream)).await
        else {
            tracing::debug!(%remote_addr, "handshake timed out");
            return Ok(());
        };
        handshake?;

        let encoding = EncodingHandle::from(config.encoding);
        let codec = TransactionCodec::for_server(encoding.clone());

        let user_id = state.next_user_id();
        let mut session = Session {
            user_id,
            name: format!("guest{user_id}").into(),
            real_name: Box::default(),
            // Empty until on_login succeeds.
            login: Box::default(),
            account_name: Box::default(),
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
            login_deadline,
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
        login_deadline: tokio::time::Instant,
    ) -> Result<SessionEnd> {
        let idle_sleep = tokio::time::sleep(IDLE_THRESHOLD);
        tokio::pin!(idle_sleep);
        let login_sleep = tokio::time::sleep_until(login_deadline);
        tokio::pin!(login_sleep);

        loop {
            tokio::select! {
                // Traffic from this client.
                frame = reader.next() => {
                    let Some(tx) = frame.transpose()? else { break Ok(SessionEnd::Closed) }; // clean EOF
                    state.touch_user(session.user_id).await;
                    let now = tokio::time::Instant::now();
                    idle_sleep.as_mut().reset(now + IDLE_THRESHOLD);
                    login_sleep.as_mut().reset(now + config.login_timeout);
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
                // Fires after `login_timeout` with no request before the session joins.
                () = &mut login_sleep, if matches!(session.state, SessionState::Agreement) => {
                    tracing::debug!(user_id = session.user_id, "login timed out");
                    break Ok(SessionEnd::Closed);
                }
                // Everything server-initiated for this session.
                msg = rx.recv() => {
                    match msg {
                        Some(RecipientMessage::Push(tx)) => {
                            let opcode = tx.r#type();
                            let is_kick = opcode == TransactionType::Push(Opcode::DisconnectMsg);
                            if session.may_receive(opcode) {
                                writer.send(tx).await?;
                            }
                            if is_kick {
                                break Ok(SessionEnd::Kicked);
                            }
                        }
                        Some(RecipientMessage::Broadcast(tx)) => {
                            if session.may_receive_broadcast(tx.r#type()) {
                                writer.send(tx).await?;
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
                Opcode::NotifyChatDeleteUser,
                vec![
                    Field::u32(FieldId::ChatID, chat_id),
                    Field::u16(FieldId::UserID, session.user_id),
                ],
            );
            for member in remaining {
                state.send_to(member, notify.clone()).await;
            }
        }
        let leave = Transaction::push(
            Opcode::NotifyDeleteUser,
            vec![Field::u16(FieldId::UserID, session.user_id)],
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
            | TransactionType::Push(Opcode::NotifyDeleteUser) => {
                self.access_extra.contains(AccessExtra::USER_GETLIST)
            }
            TransactionType::Push(Opcode::ChatMsg) => self.access.contains(Access::READ_CHAT),
            TransactionType::Push(Opcode::NewMsg) => self.access.contains(Access::NEWS_READ_ART),
            _ => true,
        }
    }

    /// Whether this session should receive a push sent to every session:
    /// only after login, and then as [`Self::may_receive`] says.
    const fn may_receive_broadcast(&self, opcode: TransactionType) -> bool {
        matches!(self.state, SessionState::Joined) && self.may_receive(opcode)
    }

    /// Handle one request, returning zero or more transactions in reply.
    pub(super) async fn handle(
        &mut self,
        tx: &Transaction<Received>,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        if self.flood_banned(FloodEvent::Transaction, state).await {
            return vec![];
        }
        let TransactionType::Request(opcode) = tx.r#type() else {
            tracing::debug!(opcode = ?tx.r#type(), "not a recognized client request");
            return vec![tx.reply_error("not implemented")];
        };

        match self.state {
            SessionState::Agreement => self.handle_agreement(opcode, tx, state, config).await,
            SessionState::Joined => self.handle_joined(opcode, tx, state, config).await,
        }
    }

    /// `true` if `event` makes the flood policy ban and disconnect the session.
    pub(super) async fn flood_banned(&self, event: FloodEvent<'_>, state: &SharedState) -> bool {
        if self.access_extra.contains(AccessExtra::CAN_SPAM)
            || state.check_flood(self.user_id, event).await == FloodDecision::Allow
        {
            return false;
        }
        let ip = self.remote_addr.ip();
        tracing::info!(user_id = self.user_id, %ip, "banned for flooding");
        state.temp_ban(ip).await;
        state.disconnect(self.user_id).await;
        true
    }

    /// `tx`'s [`myField_Data`] (101) bytes, empty if absent.
    ///
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    pub(super) fn data_bytes<'a>(&self, tx: &'a Transaction<Received>) -> &'a [u8] {
        tx.field(FieldId::Data)
            .map(Field::as_bytes)
            .unwrap_or_default()
    }

    /// Opcodes valid before login completes. Any other opcode disconnects
    /// the client.
    async fn handle_agreement(
        &mut self,
        opcode: Opcode,
        tx: &Transaction<Received>,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        match opcode {
            /* 107 */ Opcode::Login => self.on_login(tx, state, config).await,
            /* 121 */ Opcode::Agreed => self.on_agreed(tx, state).await,
            other => {
                tracing::debug!(?other, "opcode before login completes; disconnecting");
                state.disconnect(self.user_id).await;
                vec![]
            }
        }
    }

    /// Opcodes valid once a session has joined.
    async fn handle_joined(
        &mut self,
        opcode: Opcode,
        tx: &Transaction<Received>,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        match opcode {
            /* 101 */ Opcode::GetMsgs => self.on_get_msgs(tx, state).await,
            /* 103 */ Opcode::OldPostNews => self.on_old_post_news(tx, state).await,
            /* 105 */ Opcode::ChatSend => self.on_chat_send(tx, state, config).await,
            /* 107 */ Opcode::Login => vec![tx.reply_error("already logged in")],
            /* 108 */ Opcode::SendInstantMsg => self.on_send_instant_msg(tx, state).await,
            /* 110 */ Opcode::DisconnectUser => self.on_disconnect_user(tx, state).await,
            /* 112 */ Opcode::InviteNewChat => self.on_invite_new_chat(tx, state).await,
            /* 113 */ Opcode::InviteToChat => self.on_invite_to_chat(tx, state).await,
            /* 114 */ Opcode::RejectChatInvite => self.on_reject_chat_invite(tx, state).await,
            /* 115 */ Opcode::JoinChat => vec![self.on_join_chat(tx, state).await],
            /* 116 */ Opcode::LeaveChat => self.on_leave_chat(tx, state).await,
            /* 120 */ Opcode::SetChatSubject => self.on_set_chat_subject(tx, state).await,
            /* 121 */ Opcode::Agreed => vec![tx.reply_error("already logged in")],
            /* 200 */ Opcode::GetFileNameList => self.on_get_file_name_list(tx, state).await,
            /* 202 */ Opcode::DownloadFile => self.on_download_file(tx, state).await,
            /* 203 */ Opcode::UploadFile => self.on_upload_file(tx, state).await,
            /* 204 */ Opcode::DeleteFile => self.on_delete_file(tx, state).await,
            /* 205 */ Opcode::NewFolder => self.on_new_folder(tx, state).await,
            /* 206 */ Opcode::GetFileInfo => self.on_get_file_info(tx, state).await,
            /* 207 */ Opcode::SetFileInfo => self.on_set_file_info(tx, state).await,
            /* 208 */ Opcode::MoveFile => self.on_move_file(tx, state).await,
            /* 209 */ Opcode::MakeFileAlias => self.on_make_file_alias(tx, state).await,
            /* 210 */ Opcode::DownloadFldr => self.on_download_fldr(tx, state).await,
            /* 212 */ Opcode::DownloadBanner => self.on_download_banner(tx, state).await,
            /* 213 */ Opcode::UploadFldr => self.on_upload_fldr(tx, state).await,
            /* 214 */ Opcode::KillDownload => self.on_kill_download(tx, state).await,
            /* 300 */ Opcode::GetUserNameList => self.on_get_user_name_list(tx, state).await,
            /* 370 */
            Opcode::GetNewsCatNameList => self.on_get_news_cat_name_list(tx, state).await,
            /* 371 */
            Opcode::GetNewsArtNameList => self.on_get_news_art_name_list(tx, state).await,
            /* 380 */ Opcode::DelNewsItem => self.on_del_news_item(tx, state).await,
            /* 381 */ Opcode::NewNewsFldr => self.on_new_news_fldr(tx, state).await,
            /* 382 */
            Opcode::NewNewsCat => self.on_new_news_cat(tx, state).await,
            /* 400 */ Opcode::GetNewsArtData => self.on_get_news_art_data(tx, state).await,
            /* 410 */
            Opcode::PostNewsArt => self.on_post_news_art(tx, state).await,
            /* 411 */
            Opcode::DelNewsArt => self.on_del_news_art(tx, state).await,
            /* 303 */
            Opcode::GetClientInfoText => self.on_get_client_info_text(tx, state).await,
            /* 304 */
            Opcode::SetClientUserInfo => self.on_set_client_user_info(tx, state).await,
            /* 348 */ Opcode::GetUserList => self.on_get_user_list(tx, state).await,
            /* 349 */ Opcode::SetUserList => self.on_set_user_list(tx, state).await,
            /* 350 */ Opcode::NewUser => self.on_new_user(tx, state).await,
            /* 351 */ Opcode::DeleteUser => self.on_delete_user(tx, state).await,
            /* 352 */ Opcode::GetUser => self.on_get_user(tx, state).await,
            /* 353 */ Opcode::SetUser => self.on_set_user(tx, state).await,
            /* 355 */ Opcode::UserBroadcast => self.on_user_broadcast(tx, state).await,
            /* 500 */ Opcode::KeepConnectionAlive => vec![tx.reply(vec![])],
            other => {
                tracing::debug!(?other, "unhandled opcode");
                vec![tx.reply_error("not implemented")]
            }
        }
    }

    async fn on_login(
        &mut self,
        tx: &Transaction<Received>,
        state: &SharedState,
        config: &ServerConfig,
    ) -> Vec<Transaction> {
        if let Err(reason) = state.require_not_banned(self.remote_addr.ip()).await {
            return self.reject_login(tx, state, reason).await;
        }
        let login = tx.login();
        let password = tx.password();

        // An empty login field means "guest", same as the client omitting it.
        let login = login
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("guest");
        let password = password.as_deref().unwrap_or_default();

        // A `\r` becomes `-`, so a store does not read it as a path.
        let login = login.to_lowercase().replace('\r', "-");
        let account = state.account(&login).await;
        self.client_version = tx.client_version();
        self.access = match account {
            Some(account) if &*account.password == password => {
                self.login = login.into();
                self.account_name = account.name.clone();
                if !account.name.is_empty() {
                    self.name = account.name;
                }
                self.access_extra = account.access_extra;
                account.access
            }
            _ => return self.reject_login(tx, state, "Incorrect login.").await,
        };

        let fields = vec![
            Field::u16(FieldId::Vers, 190),
            Field::u16(FieldId::CommunityBannerID, 0),
            Field::string(FieldId::ServerName, config.server_name.clone()),
        ];
        let mut replies = vec![tx.reply(fields)];

        // No myField_Vers (160) counts as pre-150.
        let modern = self.client_version.unwrap_or(0) >= 150;

        // A named login with an icon, or a pre-150 client, joins right away.
        let icon = tx.icon().unwrap_or(0);
        let name = tx.name();
        let named = name.as_deref().is_some_and(|name| !name.is_empty()) && icon != 0;
        let joined = named || !modern;
        if joined {
            if named {
                self.take_user_name(name.as_deref(), true);
            }
            let selfinfo = self.complete_join(icon, state).await;
            if named {
                self.record_activity(Activity::Connect, state).await;
            }
            replies.push(selfinfo);
        }

        // DONT_SHOW_AGREEMENT skips the real text, not just its display.
        let show_agreement = !self.access.contains(Access::NO_AGREEMENT);
        let have_agreement = !config.agreement.is_empty();
        if show_agreement && have_agreement {
            replies.push(Transaction::push(
                Opcode::ShowAgreement,
                vec![Field::string(FieldId::Data, config.agreement.clone())],
            ));
        } else if modern {
            replies.push(Transaction::push(
                Opcode::ShowAgreement,
                vec![Field::bytes(FieldId::NoServerAgreement, vec![0, 1])],
            ));
        }

        replies
    }

    /// Reply with `reason`, then disconnect: an unknown login or wrong
    /// password drops the connection rather than leaving it stuck
    /// unauthenticated.
    async fn reject_login(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
        reason: &str,
    ) -> Vec<Transaction> {
        state
            .send_to(
                self.user_id,
                Transaction::push(
                    Opcode::DisconnectMsg,
                    vec![Field::string(FieldId::Data, reason)],
                ),
            )
            .await;
        vec![tx.reply_error(reason)]
    }

    /// Complete login after the client accepts the agreement,
    /// [`myTran_Agreed`] (121), with the account read again. A missing
    /// account gets an error reply but no disconnect.
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
    async fn on_agreed(
        &mut self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let account = match state.require_account(&self.login).await {
            Ok(account) => account,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        self.account_name = account.name;
        self.access = account.access;
        self.access_extra = account.access_extra;
        let icon = tx.icon().unwrap_or(0);
        self.take_user_name(tx.name().as_deref(), true);
        self.take_options(tx);
        let selfinfo = self.complete_join(icon, state).await;
        self.record_activity(Activity::Connect, state).await;
        let mut replies = vec![tx.reply(vec![]), selfinfo];
        if let Some(banner) = self.banner_push(state).await {
            replies.push(banner);
        }
        replies
    }

    /// If a banner is configured and this client's reported version can display
    /// one ([`myField_Vers`] (160) >= 151), build the [`myTran_ServerBanner`] (122)
    /// push: [`myField_ServerBannerType`] (152), plus [`myField_ServerBannerUrl`]
    /// (153) for a URL banner. An image banner sends no URL; the client is
    /// expected to follow up with [`myTran_DownloadBanner`] (212).
    ///
    /// [`myField_Vers`]: hotline_proto::FieldId::Vers
    /// [`myField_ServerBannerType`]: hotline_proto::FieldId::ServerBannerType
    /// [`myField_ServerBannerUrl`]: hotline_proto::FieldId::ServerBannerUrl
    /// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
    /// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
    async fn banner_push(&self, state: &SharedState) -> Option<Transaction> {
        if self.client_version.unwrap_or(0) < 151 {
            return None;
        }
        let kind = state.banner_kind().await?;
        let mut fields = vec![Field::bytes(
            FieldId::ServerBannerType,
            kind.type_tag().to_vec(),
        )];
        if let BannerKind::Url(url) = &kind {
            fields.push(Field::string(FieldId::ServerBannerUrl, url.clone()));
        }
        Some(Transaction::push(Opcode::ServerBanner, fields))
    }

    /// Register name/icon, broadcast the join excluding self, mark this
    /// session `Joined`, and build its own [`myTran_UserAccess`] (354) push.
    ///
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
    async fn complete_join(&mut self, icon: u16, state: &SharedState) -> Transaction {
        self.state = SessionState::Joined;
        // Not echoed back to the joiner.
        self.announce_user_change(icon, state, true).await;

        let self_entry = UserNameWithInfo {
            user_id: self.user_id,
            icon,
            flags: self.user_flags(),
            name: self.name.clone(),
        };

        Transaction::push(
            Opcode::UserAccess,
            vec![
                Field::u64(FieldId::UserAccess, self.access.bits()),
                Field::bytes(
                    FieldId::UserNameWithInfo,
                    self_entry.encode(self.encoding()),
                ),
            ],
        )
    }

    /// Report `activity` by this session to the activity log.
    pub(super) async fn record_activity(&self, activity: Activity, state: &SharedState) {
        state
            .record_activity(ActivityEntry {
                address: self.remote_addr.ip(),
                login: self.login.clone(),
                name: self.name.clone(),
                activity,
            })
            .await;
    }

    async fn on_set_client_user_info(
        &mut self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if self.flood_banned(FloodEvent::NameChange, state).await {
            return vec![];
        }
        self.take_user_name(tx.name().as_deref(), false);
        let icon = tx.icon().unwrap_or(0);
        self.take_options(tx);
        // Later changes are self-inclusive, unlike the join announcement.
        self.announce_user_change(icon, state, false).await;
        Vec::new()
    }

    /// Sets the user name from the name the client `sent`. With
    /// `ANY_NAME` the user gets the sent name. Without it, the user gets
    /// the account's name `at_login`, and keeps the name after login.
    fn take_user_name(&mut self, sent: Option<&str>, at_login: bool) {
        let sent: String = sent
            .unwrap_or_default()
            .chars()
            .map(|c| if c.is_control() { '*' } else { c })
            .collect();
        self.real_name = user_name(&sent, self.encoding());
        if self.access.contains(Access::ANY_NAME) {
            self.name = self.real_name.clone();
        } else if at_login {
            self.name = user_name(&self.account_name, self.encoding());
        }
    }

    /// Sets the refusal options and the automatic response from
    /// [`myField_Options`] (113) and [`myField_AutomaticResponse`] (215),
    /// cut to 127 bytes.
    ///
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    /// [`myField_AutomaticResponse`]: hotline_proto::FieldId::AutomaticResponse
    fn take_options(&mut self, tx: &Transaction<Received>) {
        let options = UserChangeOptions::from_bits_truncate(
            tx.field(FieldId::Options)
                .and_then(Field::as_u16)
                .unwrap_or(0),
        );
        self.refuses_pm = options.contains(UserChangeOptions::REFUSE_PM);
        self.refuses_chat = options.contains(UserChangeOptions::REFUSE_CHAT);
        self.automatic_response = options.contains(UserChangeOptions::AUTO_RESPONSE).then(|| {
            tx.string(FieldId::AutomaticResponse)
                .filter(|text| !text.is_empty())
                .map_or_else(
                    || "I'm not able to respond right now. Please try later!".into(),
                    |text| {
                        text_encoding::cut(self.encoding(), &text, MAX_AUTOMATIC_RESPONSE).into()
                    },
                )
        });
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
                Field::u16(FieldId::UserID, self.user_id),
                Field::u16(FieldId::UserIconID, icon),
                Field::string(FieldId::UserName, self.name.clone()),
                Field::u16(FieldId::UserFlags, self.user_flags().bits()),
            ],
        );
        if exclude_self {
            state.announce(self.user_id, notify).await;
        } else {
            state.publish(notify).await;
        }
    }

    /// Applies a live account-modify change for this session's login. A
    /// new access gets a [`myTran_UserAccess`] (354) push. The name becomes
    /// the client's own with `ANY_NAME`, or the account's cut to 31 bytes
    /// without it. Either one can be empty.
    ///
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
    pub(super) async fn apply_access_update(&mut self, update: AccessUpdate, state: &SharedState) {
        if self.login.is_empty() || update.login != self.login {
            return;
        }
        let access_changed = self.access != update.access;
        if access_changed {
            self.access = update.access;
            state
                .send_to(
                    self.user_id,
                    Transaction::push(
                        Opcode::UserAccess,
                        vec![Field::u64(FieldId::UserAccess, self.access.bits())],
                    ),
                )
                .await;
        }
        self.account_name = update.name.unwrap_or_default();
        if let Some(login) = update.renamed_to {
            self.login = login;
        }
        let name = if self.access.contains(Access::ANY_NAME) {
            self.real_name.clone()
        } else {
            text_encoding::cut(self.encoding(), &self.account_name, 31).into()
        };
        let name_changed = name != self.name;
        self.name = name;
        if access_changed || name_changed {
            let icon = self.current_icon(state).await;
            self.announce_user_change(icon, state, false).await;
        }
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
                    Field::u16(FieldId::UserID, self.user_id),
                    Field::u16(FieldId::UserIconID, entry.icon),
                    Field::string(FieldId::UserName, entry.name),
                    Field::u16(FieldId::UserFlags, self.user_flags().bits()),
                ],
            ))
            .await;
    }

    /// This connection's active text encoding.
    pub(super) fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }

    /// This session's own admin and away flags, for [`myTran_NotifyChangeUser`]
    /// (301) and [`myTran_UserAccess`] (354).
    ///
    /// [`myTran_NotifyChangeUser`]: hotline_proto::Opcode::NotifyChangeUser
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
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

/// `name` cut to 31 bytes in `encoding`, or `***` if it is empty.
fn user_name(name: &str, encoding: &'static Encoding) -> Box<str> {
    match text_encoding::cut(encoding, name, 31) {
        "" => "***".into(),
        name => name.into(),
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
        assert!(!session.may_receive(TransactionType::Push(Opcode::NotifyDeleteUser)));
        assert!(session.may_receive(TransactionType::Push(Opcode::ChatMsg)));

        session.access_extra.insert(AccessExtra::USER_GETLIST);
        assert!(session.may_receive(TransactionType::Push(Opcode::NotifyChangeUser)));
        assert!(session.may_receive(TransactionType::Push(Opcode::NotifyDeleteUser)));
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

    #[test]
    fn may_receive_broadcast_waits_for_login() {
        let mut session = test_session("Alice", Access::all());
        session.state = SessionState::Agreement;
        assert!(!session.may_receive_broadcast(TransactionType::Push(Opcode::ChatMsg)));

        session.state = SessionState::Joined;
        assert!(session.may_receive_broadcast(TransactionType::Push(Opcode::ChatMsg)));
        session.access.remove(Access::READ_CHAT);
        assert!(!session.may_receive_broadcast(TransactionType::Push(Opcode::ChatMsg)));
    }

    #[tokio::test]
    async fn login_from_a_banned_address_says_why() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let ip = session.remote_addr.ip();
        let login = request(Opcode::Login, 1, vec![]);

        state.ban(crate::BanRecord::new(ip, None)).await.unwrap();
        let replies = session
            .on_login(&login, &state, &ServerConfig::default())
            .await;
        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::ErrorText)
                .as_deref(),
            Some("You are permanently banned on this server.")
        );

        let later = std::time::SystemTime::now() + Duration::from_secs(60);
        state
            .ban(crate::BanRecord::new(ip, Some(later)))
            .await
            .unwrap();
        let replies = session
            .on_login(&login, &state, &ServerConfig::default())
            .await;
        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::ErrorText)
                .as_deref(),
            Some("You are temporarily banned on this server.")
        );
    }

    #[tokio::test]
    async fn login_reply_advertises_server_version_190() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .field(FieldId::Vers)
                .and_then(Field::as_u16),
            Some(190)
        );
    }

    #[tokio::test]
    async fn login_reply_sends_community_banner_id_zero() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .field(FieldId::CommunityBannerID)
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
            server_name: Box::default(),
            ..ServerConfig::default()
        };

        let login = request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0].received(encoding).string(FieldId::ServerName),
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

        // `myTran_Login` (107) can carry the name and icon too.
        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        session.on_login(&login, &state, &config).await;

        let notify = observer
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        assert_eq!(
            notify.received(encoding).string(FieldId::UserName).unwrap(),
            "Alice"
        );
        assert_eq!(
            notify
                .received(encoding)
                .field(FieldId::UserIconID)
                .and_then(Field::as_u16),
            Some(42)
        );
        assert!(
            joiner_rx.try_recv().is_err(),
            "the joiner itself must not receive its own join announcement"
        );

        let joiner = state.user(1).await.expect("joiner should be registered");
        assert_eq!(&*joiner.name, "Alice");
        assert_eq!(joiner.icon, 42);
        assert!(matches!(session.state, SessionState::Joined));
    }

    #[tokio::test]
    async fn login_records_the_connect() {
        let mut session = test_session("guest1", Access::default());
        let (state, log) = state_recording_activity(Default::default());
        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        session
            .on_login(&login, &state, &ServerConfig::default())
            .await;
        assert_eq!(log.activities(), [Activity::Connect]);
    }

    #[tokio::test]
    async fn login_without_a_name_and_icon_records_no_connect() {
        let mut session = test_session("guest1", Access::default());
        let (state, log) = state_recording_activity(Default::default());
        let login = request(
            Opcode::Login,
            1,
            vec![Field::string(FieldId::UserName, "Alice")],
        );
        session
            .on_login(&login, &state, &ServerConfig::default())
            .await;
        assert_eq!(log.activities(), []);
    }

    #[tokio::test]
    async fn login_with_an_empty_name_defers_join_and_records_no_connect() {
        let mut session = test_session("guest1", Access::default());
        let (state, log) = state_recording_activity(Default::default());
        session.state = SessionState::Agreement;
        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Vers, 151),
                Field::string(FieldId::UserName, ""),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        session
            .on_login(&login, &state, &ServerConfig::default())
            .await;
        assert!(matches!(session.state, SessionState::Agreement));
        assert_eq!(log.activities(), []);
    }

    #[tokio::test]
    async fn agreed_records_the_connect() {
        let mut session = test_session("guest1", Access::default());
        session.login = "guest".into();
        let (state, log) = state_recording_activity(Default::default());
        let agreed = request(Opcode::Agreed, 1, vec![]);
        session.on_agreed(&agreed, &state).await;
        assert_eq!(log.activities(), [Activity::Connect]);
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
                    password: "hunter2".into(),
                    name: "Alice".into(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "alice"),
                Field::obfuscated_string(FieldId::UserPassword, "hunter2"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(&*session.name, "Alice");
    }

    #[tokio::test]
    async fn login_changes_each_cr_to_a_dash() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        let config = ServerConfig::default();
        state
            .put_account(
                "a-b".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: "hunter2".into(),
                    name: "AB".into(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "A\rB"),
                Field::obfuscated_string(FieldId::UserPassword, "hunter2"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(&*session.login, "a-b");
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
                    password: "hunter2".into(),
                    name: "Alice".into(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "ALICE"),
                Field::obfuscated_string(FieldId::UserPassword, "hunter2"),
            ],
        );
        session.on_login(&login, &state, &config).await;

        assert_eq!(session.access, Access::BROADCAST);
        assert_eq!(&*session.login, "alice");
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
                    password: Box::default(),
                    name: "Alice".into(),
                    access: Access::empty(),
                },
            )
            .await
            .unwrap();

        let login = request(
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::UserLogin, "alice")],
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
                    password: "hunter2".into(),
                    name: "Alice".into(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();
        let mut rx = state.register_recipient(session.user_id).await;

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "alice"),
                Field::obfuscated_string(FieldId::UserPassword, "wrong"),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::ErrorText),
            Some("Incorrect login.".to_string())
        );
        let kick = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a DisconnectMsg push");
        assert_eq!(kick.r#type(), TransactionType::Push(Opcode::DisconnectMsg));
        assert!(session.login.is_empty());
    }

    #[tokio::test]
    async fn agreed_without_a_stored_account_is_refused_without_a_kick() {
        for login in ["", "gone"] {
            let mut session = test_session("guest1", Access::all());
            session.state = SessionState::Agreement;
            session.login = login.into();
            let state = SharedState::default();
            let mut own = state.register_recipient(session.user_id).await;
            let mut bystander = state.register_recipient(2).await;

            let agreed = request(
                Opcode::Agreed,
                1,
                vec![
                    Field::string(FieldId::UserName, "Alice"),
                    Field::u16(FieldId::UserIconID, 42),
                ],
            );
            let replies = session.on_agreed(&agreed, &state).await;

            assert_eq!(replies.len(), 1);
            assert_eq!(
                replies[0]
                    .received(text_encoding::DEFAULT)
                    .string(FieldId::ErrorText),
                Some(format!("Cannot read account \u{201c}{login}\u{201d}."))
            );
            assert!(matches!(session.state, SessionState::Agreement));
            assert!(bystander.try_recv().is_err());
            assert!(own.try_recv().is_err());
        }
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
                    password: Box::default(),
                    name: "Alice".into(),
                    access: Access::BROADCAST,
                },
            )
            .await
            .unwrap();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "alice"),
                Field::obfuscated_string(FieldId::UserPassword, "anything"),
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

        let login = request(
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::UserLogin, "nobody")],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert_eq!(
            replies[0]
                .received(text_encoding::DEFAULT)
                .string(FieldId::ErrorText),
            Some("Incorrect login.".to_string())
        );
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn login_without_a_guest_account_is_rejected() {
        let mut session = test_session("guest1", Access::empty());
        let state = SharedState::default();
        state.delete_account("guest").await.unwrap();
        let config = ServerConfig::default();
        let mut rx = state.register_recipient(session.user_id).await;

        let login = request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert!(replies[0].is_error());
        assert!(rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn apply_access_update_takes_an_empty_name_as_is() {
        let state = SharedState::default();
        for access in [Access::empty(), Access::ANY_NAME] {
            let mut session = test_session("Bob", access);
            session.login = "bob".into();
            session.real_name = Box::default();
            let update = AccessUpdate {
                login: "bob".into(),
                renamed_to: None,
                access,
                name: None,
            };
            session.apply_access_update(update, &state).await;
            assert_eq!(&*session.name, "");
        }
    }

    #[tokio::test]
    async fn handle_agreement_disconnects_on_any_other_opcode() {
        for opcode in [Opcode::GetUserNameList, Opcode::KeepConnectionAlive] {
            let mut session = test_session("guest1", Access::default());
            session.state = SessionState::Agreement;
            let state = SharedState::default();
            let config = ServerConfig::default();
            let mut rx = state.register_recipient(session.user_id).await;

            let tx = request(opcode, 1, vec![]);
            assert!(session.handle(&tx, &state, &config).await.is_empty());
            assert!(matches!(rx.try_recv(), Ok(RecipientMessage::Disconnect)));
        }
    }

    #[tokio::test]
    async fn handle_joined_sends_chat_request_errors_only_by_policy() {
        struct Replies;
        impl crate::ServerPolicy for Replies {
            fn error(&self, _case: crate::Silent) -> Option<std::borrow::Cow<'static, str>> {
                Some("refused".into())
            }
        }
        let mut session = test_session("Alice", Access::default());
        let config = ServerConfig::default();
        let requests = [
            request(Opcode::InviteToChat, 1, vec![]),
            request(Opcode::SetChatSubject, 2, vec![]),
        ];

        let state = SharedState::default();
        for request in &requests {
            assert!(session.handle(request, &state, &config).await.is_empty());
        }
        let state = state_with_policy(Replies);
        for request in &requests {
            assert!(session.handle(request, &state, &config).await[0].is_error());
        }
    }

    #[tokio::test]
    async fn login_without_name_defers_join_until_agreed() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        session.state = SessionState::Agreement;
        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 151)]);
        session.on_login(&login, &state, &config).await;

        assert!(bystander.try_recv().is_err());
        assert!(matches!(session.state, SessionState::Agreement));

        let agreed = request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        session.on_agreed(&agreed, &state).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("agreed should announce a join");
        assert_eq!(
            notify.received(encoding).string(FieldId::UserName).unwrap(),
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
        let login = request(Opcode::Login, 1, vec![]);
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
        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Vers, 151),
                Field::string(FieldId::UserName, "Alice"),
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
        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Vers, 151),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
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
    async fn login_with_name_sends_user_access_before_the_agreement() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Vers, 151),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let position = |opcode| {
            replies
                .iter()
                .position(|tx| tx.r#type() == TransactionType::Push(opcode))
                .unwrap()
        };
        assert!(position(Opcode::UserAccess) < position(Opcode::ShowAgreement));
    }

    #[tokio::test]
    async fn agreed_push_carries_image_banner_type_with_no_url() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 151)]);
        session.on_login(&login, &state, &config).await;
        let agreed = request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let banner = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::ServerBanner))
            .expect("expected a myTran_ServerBanner push");
        assert_eq!(
            banner
                .received(encoding)
                .field(FieldId::ServerBannerType)
                .unwrap()
                .as_bytes(),
            b"JPEG"
        );
        assert!(banner.field(FieldId::ServerBannerUrl).is_none());
    }

    #[tokio::test]
    async fn agreed_push_carries_url_banner_type_and_url() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(crate::banner::UrlBannerProvider::new(
            "https://example.com/banner.jpg",
        )));
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 151)]);
        session.on_login(&login, &state, &config).await;
        let agreed = request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let banner = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::ServerBanner))
            .expect("expected a myTran_ServerBanner push");
        assert_eq!(
            banner
                .received(encoding)
                .field(FieldId::ServerBannerType)
                .unwrap()
                .as_bytes(),
            b"URL "
        );
        assert_eq!(
            banner.received(encoding).string(FieldId::ServerBannerUrl),
            Some("https://example.com/banner.jpg".to_string())
        );
    }

    #[tokio::test]
    async fn agreed_push_omitted_below_client_version_151() {
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 150)]);
        session.on_login(&login, &state, &config).await;
        let agreed = request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::ServerBanner)));
    }

    #[tokio::test]
    async fn agreed_push_omitted_when_no_banner_configured() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig::default();

        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 151)]);
        session.on_login(&login, &state, &config).await;
        let agreed = request(
            Opcode::Agreed,
            2,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::ServerBanner)));
    }

    #[tokio::test]
    async fn login_with_a_name_and_icon_sends_no_banner() {
        let mut session = test_session("guest1", Access::default());
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let config = ServerConfig::default();

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::u16(FieldId::Vers, 151),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(matches!(session.state, SessionState::Joined));
        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::ServerBanner)));
    }

    #[tokio::test]
    async fn modern_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: Box::default(),
            ..ServerConfig::default()
        };

        let login = request(Opcode::Login, 1, vec![Field::u16(FieldId::Vers, 151)]);
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::ShowAgreement))
            .expect("modern client should get a Noagreement marker");
        assert!(agreement.field(FieldId::NoServerAgreement).is_some());
    }

    #[tokio::test]
    async fn pre_150_client_gets_no_agreement_push_without_configured_text() {
        let mut session = test_session("guest1", Access::default());
        let state = SharedState::default();
        let config = ServerConfig {
            agreement: Box::default(),
            ..ServerConfig::default()
        };

        let login = request(Opcode::Login, 1, vec![]);
        let replies = session.on_login(&login, &state, &config).await;

        assert!(
            !replies
                .iter()
                .any(|tx| tx.r#type() == TransactionType::Push(Opcode::ShowAgreement)),
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
                    password: Box::default(),
                    name: "Skipper".into(),
                    access: Access::NO_AGREEMENT,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig {
            agreement: "please behave".into(),
            ..ServerConfig::default()
        };

        let login = request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::UserLogin, "skipper"),
                Field::u16(FieldId::Vers, 151),
            ],
        );
        let replies = session.on_login(&login, &state, &config).await;

        let agreement = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::ShowAgreement))
            .expect("modern client still gets a no-agreement marker");
        assert!(
            agreement.field(FieldId::NoServerAgreement).is_some(),
            "DONT_SHOW_AGREEMENT must skip the real text, not just its display"
        );
        assert!(agreement.field(FieldId::Data).is_none());
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
                    password: Box::default(),
                    name: "Skipper".into(),
                    access: Access::NO_AGREEMENT,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig {
            agreement: "please behave".into(),
            ..ServerConfig::default()
        };

        let login = request(
            Opcode::Login,
            1,
            vec![Field::obfuscated_string(FieldId::UserLogin, "skipper")],
        );
        let replies = session.on_login(&login, &state, &config).await;

        assert!(!replies
            .iter()
            .any(|tx| tx.r#type() == TransactionType::Push(Opcode::ShowAgreement)),);
    }

    #[tokio::test]
    async fn agreed_selfinfo_carries_the_joiners_own_user_list_entry() {
        let encoding = text_encoding::DEFAULT;
        let mut session = test_session("guest1", Access::default());
        session.login = "guest".into();
        let state = SharedState::default();

        let agreed = request(
            Opcode::Agreed,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let selfinfo = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::UserAccess))
            .expect("on_agreed should push UserAccess");
        let entry = selfinfo
            .received(encoding)
            .field(FieldId::UserNameWithInfo)
            .and_then(|f| UserNameWithInfo::decode(f.as_bytes(), encoding).ok())
            .expect("UserAccess should carry the joiner's own UserList entry");
        assert_eq!(entry.user_id, 1); // test_session()'s fixed user_id
        assert_eq!(&*entry.name, "Alice");
        assert_eq!(entry.icon, 42);
    }

    fn agreed_as(name: &str, options: UserChangeOptions) -> Transaction<Received> {
        request(
            Opcode::Agreed,
            1,
            vec![
                Field::string(FieldId::UserName, name),
                Field::u16(FieldId::UserIconID, 42),
                Field::u16(FieldId::Options, options.bits()),
            ],
        )
    }

    #[tokio::test]
    async fn agreed_without_any_name_takes_the_account_name() {
        let mut session = test_session("guest1", Access::ANY_NAME);
        session.login = "alice".into();
        session.account_name = "Stale".into();
        let state = SharedState::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Guest".into(),
                    access: Access::default() - Access::ANY_NAME,
                },
            )
            .await
            .unwrap();

        let agreed = agreed_as("Mallory", UserChangeOptions::empty());
        session.on_agreed(&agreed, &state).await;

        assert_eq!(&*session.name, "Guest");
    }

    #[tokio::test]
    async fn agreed_with_any_name_cleans_and_cuts_the_name() {
        let mut session = test_session("guest1", Access::ANY_NAME);
        session.login = "guest".into();
        let state = SharedState::default();

        let long = format!("a\rb{}", "c".repeat(40));
        session
            .on_agreed(&agreed_as(&long, UserChangeOptions::empty()), &state)
            .await;
        assert_eq!(&*session.name, format!("a*b{}", "c".repeat(28)));

        session
            .on_agreed(&agreed_as("", UserChangeOptions::empty()), &state)
            .await;
        assert_eq!(&*session.name, "***");
    }

    #[tokio::test]
    async fn agreed_reads_the_refusal_options() {
        let mut session = test_session("guest1", Access::default());
        session.login = "guest".into();
        let state = SharedState::default();

        let agreed = agreed_as("Alice", UserChangeOptions::REFUSE_PM);
        session.on_agreed(&agreed, &state).await;

        assert!(session.refuses_pm);
        assert!(session.user_flags().contains(UserFlags::REFUSES_PM));
    }

    #[tokio::test]
    async fn set_client_user_info_without_any_name_keeps_the_name() {
        let mut session = test_session("Guest", Access::default() - Access::ANY_NAME);
        let state = SharedState::default();

        let request = request(
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::UserName, "Mallory"),
                Field::u16(FieldId::UserIconID, 0),
            ],
        );
        session.on_set_client_user_info(&request, &state).await;

        assert_eq!(&*session.name, "Guest");
    }

    #[tokio::test]
    async fn agreed_selfinfo_sends_the_access_read_again() {
        let mut session = test_session("guest1", Access::all());
        session.login = "alice".into();
        let state = SharedState::default();
        state
            .put_account(
                "alice".to_string(),
                Account {
                    access_extra: AccessExtra::default(),
                    password: Box::default(),
                    name: "Alice".into(),
                    access: Access::DOWNLOAD_FILE,
                },
            )
            .await
            .unwrap();

        let agreed = request(
            Opcode::Agreed,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserIconID, 42),
            ],
        );
        let replies = session.on_agreed(&agreed, &state).await;

        let selfinfo = replies
            .iter()
            .find(|tx| tx.r#type() == TransactionType::Push(Opcode::UserAccess))
            .expect("on_agreed should push UserAccess");
        assert_eq!(
            selfinfo
                .received(text_encoding::DEFAULT)
                .field(FieldId::UserAccess)
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
                    password: Box::default(),
                    name: Box::default(),
                    access: Access::DISCON_USER,
                },
            )
            .await
            .unwrap();
        let config = ServerConfig::default();
        let mut bystander = state.register_recipient(2).await;

        let login = request(
            Opcode::Login,
            1,
            vec![Field::string(FieldId::UserName, "Mod")],
        );
        session.on_login(&login, &state, &config).await;

        let notify = bystander
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("login should announce a join");
        let color = notify
            .received(text_encoding::DEFAULT)
            .field(FieldId::UserFlags)
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
                login: Box::default(),
                name: session.name.clone(),
                icon: 0,
                client_version: None,
                access: session.access,
                access_extra: AccessExtra::USER_GETLIST,
                account_name: Box::default(),
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
            .received(text_encoding::DEFAULT)
            .field(FieldId::UserFlags)
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
            .received(text_encoding::DEFAULT)
            .field(FieldId::UserFlags)
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
    async fn set_client_user_info_reads_the_refusal_bits_out_of_options() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = request(
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice".to_string()),
                Field::u16(FieldId::UserIconID, 0),
                Field::u16(
                    FieldId::Options,
                    (UserChangeOptions::REFUSE_PM | UserChangeOptions::REFUSE_CHAT).bits(),
                ),
            ],
        );
        session.on_set_client_user_info(&request, &state).await;

        assert!(session.refuses_pm);
        assert!(session.refuses_chat);
        assert!(session.automatic_response.is_none());
        assert!(session.user_flags().contains(UserFlags::REFUSES_PM));
        assert!(session.user_flags().contains(UserFlags::REFUSES_CHAT));
    }

    #[tokio::test]
    async fn set_client_user_info_reads_a_custom_automatic_response() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = request(
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice".to_string()),
                Field::u16(FieldId::UserIconID, 0),
                Field::u16(FieldId::Options, UserChangeOptions::AUTO_RESPONSE.bits()),
                Field::string(FieldId::AutomaticResponse, "Back in five."),
            ],
        );
        session.on_set_client_user_info(&request, &state).await;

        assert_eq!(session.automatic_response.as_deref(), Some("Back in five."));
    }

    #[tokio::test]
    async fn set_client_user_info_cuts_the_automatic_response_to_127_bytes() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = request(
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice".to_string()),
                Field::u16(FieldId::UserIconID, 0),
                Field::u16(FieldId::Options, UserChangeOptions::AUTO_RESPONSE.bits()),
                Field::string(FieldId::AutomaticResponse, "a".repeat(200)),
            ],
        );
        session.on_set_client_user_info(&request, &state).await;

        assert_eq!(
            session.automatic_response.as_deref().map(str::len),
            Some(MAX_AUTOMATIC_RESPONSE)
        );
    }

    #[tokio::test]
    async fn set_client_user_info_falls_back_to_a_default_automatic_response_text() {
        let mut session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let request = request(
            Opcode::SetClientUserInfo,
            1,
            vec![
                Field::string(FieldId::UserName, "Alice".to_string()),
                Field::u16(FieldId::UserIconID, 0),
                Field::u16(FieldId::Options, UserChangeOptions::AUTO_RESPONSE.bits()),
            ],
        );
        session.on_set_client_user_info(&request, &state).await;

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
        assert_eq!(state.connection_count().await, 0);
        assert!(state.users().await.is_empty());
    }

    /// Runs one session with a short login timeout and an agreement.
    /// Returns its task, a client socket, and the state.
    async fn run_with_short_login_timeout() -> (
        tokio::task::JoinHandle<Result<()>>,
        tokio::net::TcpStream,
        SharedState,
    ) {
        let state = SharedState::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let config = ServerConfig {
            login_timeout: Duration::from_millis(300),
            agreement: "Hotline".into(),
            ..ServerConfig::default()
        };
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            Session::run(stream, server_state, config).await
        });
        let client = tokio::net::TcpStream::connect(addr).await.unwrap();
        (server, client, state)
    }

    #[tokio::test]
    async fn login_restarts_the_login_timeout() {
        let (server, mut client, state) = run_with_short_login_timeout().await;
        let guest = Account {
            access_extra: AccessExtra::default(),
            password: "".into(),
            name: "Guest".into(),
            access: Access::empty(),
        };
        state.put_account("guest".to_string(), guest).await.unwrap();
        client
            .write_all(&ClientHandshake::default().encode())
            .await
            .unwrap();
        let mut reply = [0u8; ServerHandshake::LEN];
        client.read_exact(&mut reply).await.unwrap();
        let fields = vec![Field::u16(FieldId::Vers, 190)];
        let login = Transaction::request(Opcode::Login, 1, fields)
            .encode(text_encoding::DEFAULT)
            .unwrap();
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            client.write_all(&login).await.unwrap();
        }

        assert!(!server.is_finished());
        let finished = tokio::time::timeout(Duration::from_secs(5), server).await;
        assert!(finished.unwrap().unwrap().is_ok());
    }

    #[tokio::test]
    async fn a_client_that_never_sends_a_handshake_is_closed() {
        let (server, mut client, _) = run_with_short_login_timeout().await;

        let finished = tokio::time::timeout(Duration::from_secs(5), server).await;

        assert!(finished.unwrap().unwrap().is_ok());
        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
    }
}
