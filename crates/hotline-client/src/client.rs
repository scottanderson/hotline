use std::collections::HashMap;
use std::net::SocketAddr;
use std::num::NonZero;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use hotline_proto::text_encoding::EncodingHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::types::{self, FileNameWithInfo, HlPath, UserNameWithInfo};
use hotline_proto::{
    ClientHandshake, Field, FieldId, HtxfHeader, Opcode, Received, ServerHandshake, Transaction,
};

use crate::ban::Ban;
use crate::client_info_text::ClientInfoText;
use crate::client_user_info::ClientUserInfo;
use crate::error::{ClientError, Result};
use crate::file_download::FileDownload;
use crate::file_upload::FileUpload;
use crate::folder_download::FolderDownload;
use crate::folder_upload::FolderUpload;
use crate::joined_chat::JoinedChat;
use crate::login_result::LoginResult;

type Conn = Framed<TcpStream, TransactionCodec>;

/// [`myField_Options`] (113) on [`myTran_SendInstantMsg`] (108) for a message
/// a user typed.
///
/// [`myField_Options`]: hotline_proto::FieldId::Options
/// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
const USER_MESSAGE: u16 = 1;

/// How long [`HotlineClient::request`] waits for a reply.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// [`myField_FileXferOptions`] (204) on an upload that resumes a partial one.
///
/// [`myField_FileXferOptions`]: hotline_proto::FieldId::FileXferOptions
const RESUME: u16 = 1;

/// A pending-request table: still accepting new requests, or
/// permanently closed because the connection ended. See [`read_loop`]'s
/// shutdown handling.
enum PendingState {
    Open(HashMap<u32, oneshot::Sender<Transaction<Received>>>),
    Closed,
}

/// The state that [`HotlineClient`] and its [`read_loop`] share.
struct Shared {
    pending: Mutex<PendingState>,
    /// When the handshake ended.
    connected: Instant,
    /// When a transaction last went either way, in nanoseconds after `connected`.
    last_traffic: AtomicU64,
}

/// The write half of the connection and the next transaction ID, used
/// only by the clones of [`HotlineClient`].
struct Writer {
    sink: Mutex<SplitSink<Conn, Transaction>>,
    trans_counter: AtomicTransCounter,
}

/// Gives out the transaction ID of each request: 1 up to `u32::MAX`, then
/// 1 again, and never 0.
struct AtomicTransCounter(AtomicU32);

impl AtomicTransCounter {
    /// A counter whose first ID is 1.
    const fn new() -> Self {
        Self(AtomicU32::new(1))
    }

    /// The next transaction ID. A 0 from the counter goes to the next one.
    fn next(&self) -> NonZero<u32> {
        loop {
            let id = self.0.fetch_add(1, Ordering::Relaxed);
            if let Some(id) = NonZero::new(id) {
                return id;
            }
        }
    }
}

/// A connected, logged-in-or-not Hotline client session. Cloning is
/// cheap: it shares the same connection and pending-request table. The
/// read half runs on its own spawned task (`read_loop`); this struct
/// only touches the write half.
#[derive(Clone)]
pub struct HotlineClient {
    writer: Arc<Writer>,
    shared: Arc<Shared>,
    encoding: EncodingHandle,
    /// The server's transaction address.
    peer: SocketAddr,
}

impl HotlineClient {
    /// Connect to `addr`, perform the `TRTP` handshake, and start the
    /// background read loop. Returns the client plus a channel that
    /// receives every server-initiated transaction (chat messages, user
    /// list changes, news posts, disconnect notices, etc).
    pub async fn connect<A: ToSocketAddrs>(
        addr: A,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Transaction<Received>>)> {
        let mut stream = TcpStream::connect(addr).await?;
        let peer = stream.peer_addr()?;

        // The TRTP handshake isn't shaped like a Transaction, so it's
        // done on the raw stream before handing it to the codec.
        stream
            .write_all(&ClientHandshake::default().encode())
            .await?;

        let mut handshake_buf = [0u8; ServerHandshake::LEN];
        stream.read_exact(&mut handshake_buf).await?;
        ServerHandshake::decode(&handshake_buf)?;

        let encoding = EncodingHandle::default();
        let codec = TransactionCodec::for_client(encoding.clone());
        let (writer, reader) = Framed::new(stream, codec).split();
        let shared = Arc::new(Shared {
            pending: Mutex::new(PendingState::Open(HashMap::new())),
            connected: Instant::now(),
            last_traffic: AtomicU64::new(0),
        });
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        tokio::spawn(read_loop(reader, shared.clone(), event_tx));

        Ok((
            Self {
                writer: Arc::new(Writer {
                    sink: Mutex::new(writer),
                    trans_counter: AtomicTransCounter::new(),
                }),
                shared,
                encoding,
                peer,
            },
            event_rx,
        ))
    }

    /// This connection's active text encoding, shared with the codec
    /// that reads/writes it and any caller (e.g. the event stream
    /// consumer) that needs to interpret a pushed [`Transaction`] itself.
    pub fn encoding(&self) -> EncodingHandle {
        self.encoding.clone()
    }

    /// Send a request transaction and wait up to [`REQUEST_TIMEOUT`] for its
    /// matching reply. Returns `Err` if the server replied with the error bit
    /// set.
    pub async fn request(
        &self,
        opcode: Opcode,
        fields: Vec<Field>,
    ) -> Result<Transaction<Received>> {
        let id = self.writer.trans_counter.next().get();
        let tx = Transaction::request(opcode, id, fields);

        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut pending = self.shared.pending.lock().await;
            match &mut *pending {
                PendingState::Open(map) => {
                    map.insert(id, reply_tx);
                }
                PendingState::Closed => return Err(ClientError::ConnectionClosed),
            }
        }

        if let Err(e) = self.send(tx).await {
            if let PendingState::Open(map) = &mut *self.shared.pending.lock().await {
                map.remove(&id);
            }
            return Err(e);
        }

        let reply = match tokio::time::timeout(REQUEST_TIMEOUT, reply_rx).await {
            Ok(reply) => reply.map_err(|_| ClientError::ConnectionClosed)?,
            Err(_) => {
                if let PendingState::Open(map) = &mut *self.shared.pending.lock().await {
                    map.remove(&id);
                }
                return Err(ClientError::Timeout);
            }
        };
        if reply.is_error() {
            let msg = reply
                .string(FieldId::ErrorText)
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(ClientError::Server(msg));
        }
        Ok(reply)
    }

    /// Log in with the given credentials, [`myTran_Login`] (107). `login`
    /// and `password` are obfuscated with Hotline's XOR-0xFF transform
    /// before going on the wire.
    ///
    /// The server follows the reply with a [`myTran_ShowAgreement`] (109)
    /// push on the event stream returned by [`Self::connect`], carrying
    /// either agreement text or a no-agreement marker. Call [`Self::agree`]
    /// with the caller's name/icon after handling it to complete login.
    ///
    /// [`myTran_Login`]: hotline_proto::Opcode::Login
    /// [`myTran_ShowAgreement`]: hotline_proto::Opcode::ShowAgreement
    pub async fn login(&self, login: &str, password: &str) -> Result<LoginResult> {
        let reply = self
            .request(
                Opcode::Login,
                vec![
                    Field::obfuscated_string(FieldId::UserLogin, login),
                    Field::obfuscated_string(FieldId::UserPassword, password),
                    Field::u16(FieldId::Vers, 190),
                ],
            )
            .await?;

        Ok(LoginResult {
            version: reply.field(FieldId::Vers).map(Field::as_u16),
            banner_id: reply.field(FieldId::CommunityBannerID).map(Field::as_u16),
            server_name: reply.string(FieldId::ServerName).map(Into::into),
        })
    }

    /// Accept the server's agreement, completing login, [`myTran_Agreed`] (121).
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
    pub async fn agree(&self, info: &ClientUserInfo) -> Result<()> {
        self.request(Opcode::Agreed, info.fields()).await?;
        Ok(())
    }

    /// Send a public chat message, [`myTran_ChatSend`] (105).
    ///
    /// [`myTran_ChatSend`]: hotline_proto::Opcode::ChatSend
    pub async fn send_chat(&self, message: &str) -> Result<()> {
        self.send_without_reply(
            Opcode::ChatSend,
            vec![Field::string(FieldId::Data, message)],
        )
        .await
    }

    /// Send a message to the private chat room `chat_id`,
    /// [`myTran_ChatSend`] (105).
    ///
    /// [`myTran_ChatSend`]: hotline_proto::Opcode::ChatSend
    pub async fn send_room_chat(&self, chat_id: u32, message: &str) -> Result<()> {
        self.send_without_reply(
            Opcode::ChatSend,
            vec![
                Field::string(FieldId::Data, message),
                Field::u32(FieldId::ChatID, chat_id),
            ],
        )
        .await
    }

    /// Create a private chat room and invite `user_id` to it,
    /// [`myTran_InviteNewChat`] (112).
    ///
    /// [`myTran_InviteNewChat`]: hotline_proto::Opcode::InviteNewChat
    pub async fn invite_new_chat(&self, user_id: u16) -> Result<JoinedChat> {
        let reply = self
            .request(
                Opcode::InviteNewChat,
                vec![Field::u16(FieldId::UserID, user_id)],
            )
            .await?;
        Ok(JoinedChat {
            id: chat_id(&reply)?,
            subject: Box::default(),
            users: reply.user_entry().into_iter().collect(),
        })
    }

    /// Invite `user_id` to the private chat room `chat_id`,
    /// [`myTran_InviteToChat`] (113). The call does not wait for a reply.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    pub async fn invite_to_chat(&self, chat_id: u32, user_id: u16) -> Result<()> {
        self.send_without_reply(
            Opcode::InviteToChat,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::u16(FieldId::UserID, user_id),
            ],
        )
        .await
    }

    /// Decline an invitation to the private chat room `chat_id`,
    /// [`myTran_RejectChatInvite`] (114).
    ///
    /// [`myTran_RejectChatInvite`]: hotline_proto::Opcode::RejectChatInvite
    pub async fn reject_chat_invite(&self, chat_id: u32) -> Result<()> {
        self.send_without_reply(
            Opcode::RejectChatInvite,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        )
        .await
    }

    /// Join the private chat room `chat_id`, [`myTran_JoinChat`] (115).
    ///
    /// [`myTran_JoinChat`]: hotline_proto::Opcode::JoinChat
    pub async fn join_chat(&self, chat_id: u32) -> Result<JoinedChat> {
        let reply = self
            .request(Opcode::JoinChat, vec![Field::u32(FieldId::ChatID, chat_id)])
            .await?;
        Ok(JoinedChat {
            id: chat_id,
            subject: reply
                .string(FieldId::ChatSubject)
                .unwrap_or_default()
                .into(),
            users: reply.user_list_entries()?,
        })
    }

    /// Leave the private chat room `chat_id`, [`myTran_LeaveChat`] (116).
    ///
    /// [`myTran_LeaveChat`]: hotline_proto::Opcode::LeaveChat
    pub async fn leave_chat(&self, chat_id: u32) -> Result<()> {
        self.send_without_reply(
            Opcode::LeaveChat,
            vec![Field::u32(FieldId::ChatID, chat_id)],
        )
        .await
    }

    /// Set the subject of the private chat room `chat_id`,
    /// [`myTran_SetChatSubject`] (120). The call does not wait for a reply.
    ///
    /// [`myTran_SetChatSubject`]: hotline_proto::Opcode::SetChatSubject
    pub async fn set_chat_subject(&self, chat_id: u32, subject: &str) -> Result<()> {
        self.send_without_reply(
            Opcode::SetChatSubject,
            vec![
                Field::u32(FieldId::ChatID, chat_id),
                Field::string(FieldId::ChatSubject, subject),
            ],
        )
        .await
    }

    /// Send a request that gets no reply.
    async fn send_without_reply(&self, opcode: Opcode, fields: Vec<Field>) -> Result<()> {
        let id = self.writer.trans_counter.next().get();
        let tx = Transaction::request(opcode, id, fields);
        self.send(tx).await
    }

    /// Write `tx` and note the traffic.
    async fn send(&self, tx: Transaction) -> Result<()> {
        self.writer.sink.lock().await.send(tx).await?;
        self.shared.touch();
        Ok(())
    }

    /// Close the connection. The server then ends it, and each later
    /// request fails.
    pub async fn close(&self) -> Result<()> {
        self.writer.sink.lock().await.close().await?;
        Ok(())
    }

    /// When a transaction last went either way on the connection.
    pub fn last_traffic(&self) -> Instant {
        let nanos = self.shared.last_traffic.load(Ordering::Relaxed);
        let connected = self.shared.connected;
        connected
            .checked_add(Duration::from_nanos(nanos))
            .unwrap_or(connected)
    }

    /// Tell the server that the connection is in use,
    /// [`myTran_KeepConnectionAlive`] (500).
    ///
    /// [`myTran_KeepConnectionAlive`]: hotline_proto::Opcode::KeepConnectionAlive
    pub async fn keep_connection_alive(&self) -> Result<()> {
        self.request(Opcode::KeepConnectionAlive, Vec::new())
            .await?;
        Ok(())
    }

    /// Send a private message to `user_id`, [`myTran_SendInstantMsg`] (108).
    ///
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    pub async fn send_instant_msg(&self, user_id: u16, message: &str) -> Result<()> {
        self.request(
            Opcode::SendInstantMsg,
            vec![
                Field::u16(FieldId::UserID, user_id),
                Field::u16(FieldId::Options, USER_MESSAGE),
                Field::string(FieldId::Data, message),
            ],
        )
        .await?;
        Ok(())
    }

    /// Disconnect `user_id`, with `ban` on the user's address,
    /// [`myTran_DisconnectUser`] (110). A permanent ban also sends `name`
    /// in [`myField_Data`] (101).
    ///
    /// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    pub async fn disconnect_user(&self, user_id: u16, name: &str, ban: Ban) -> Result<()> {
        let mut fields = vec![Field::u16(FieldId::UserID, user_id)];
        if let Some(options) = ban.options() {
            fields.push(Field::u16(FieldId::Options, options));
        }
        if ban == Ban::Permanent && !name.is_empty() {
            fields.push(Field::string(FieldId::Data, name));
        }
        self.request(Opcode::DisconnectUser, fields).await?;
        Ok(())
    }

    /// Send `message` to every connected user, [`myTran_UserBroadcast`] (355).
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    pub async fn user_broadcast(&self, message: &str) -> Result<()> {
        self.request(
            Opcode::UserBroadcast,
            vec![Field::string(FieldId::Data, message)],
        )
        .await?;
        Ok(())
    }

    /// Fetch the connected-user list, [`myTran_GetUserNameList`] (300).
    ///
    /// [`myTran_GetUserNameList`]: hotline_proto::Opcode::GetUserNameList
    pub async fn get_user_name_list(&self) -> Result<Vec<UserNameWithInfo>> {
        let reply = self.request(Opcode::GetUserNameList, vec![]).await?;
        Ok(reply.user_list_entries()?)
    }

    /// Change this user's name, icon, and options after login,
    /// [`myTran_SetClientUserInfo`] (304). The server sends no reply.
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub async fn set_client_user_info(&self, info: &ClientUserInfo) -> Result<()> {
        self.send_without_reply(Opcode::SetClientUserInfo, info.fields())
            .await
    }

    /// Fetch `user_id`'s info text, [`myTran_GetClientInfoText`] (303).
    ///
    /// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
    pub async fn get_client_info_text(&self, user_id: u16) -> Result<ClientInfoText> {
        let reply = self
            .request(
                Opcode::GetClientInfoText,
                vec![Field::u16(FieldId::UserID, user_id)],
            )
            .await?;
        Ok(ClientInfoText {
            name: reply.string(FieldId::UserName).unwrap_or_default().into(),
            text: reply.string(FieldId::Data).unwrap_or_default().into(),
        })
    }

    /// Fetch the classic news text, [`myTran_GetMsgs`] (101).
    ///
    /// [`myTran_GetMsgs`]: hotline_proto::Opcode::GetMsgs
    pub async fn get_msgs(&self) -> Result<String> {
        let reply = self.request(Opcode::GetMsgs, vec![]).await?;
        Ok(reply.string(FieldId::Data).unwrap_or_default())
    }

    /// Post to the classic news, [`myTran_OldPostNews`] (103).
    ///
    /// [`myTran_OldPostNews`]: hotline_proto::Opcode::OldPostNews
    pub async fn old_post_news(&self, text: &str) -> Result<()> {
        self.request(
            Opcode::OldPostNews,
            vec![Field::string(FieldId::Data, text)],
        )
        .await?;
        Ok(())
    }

    /// Fetch the listing of the folder at `path`, [`myTran_GetFileNameList`] (200).
    ///
    /// [`myTran_GetFileNameList`]: hotline_proto::Opcode::GetFileNameList
    pub async fn get_file_name_list(&self, path: &HlPath) -> Result<Vec<FileNameWithInfo>> {
        let fields = self.file_path_field(path)?.into_iter().collect();
        let reply = self.request(Opcode::GetFileNameList, fields).await?;
        Ok(reply.file_list_entries()?)
    }

    /// Ask to download the file `name` in the folder at `path`,
    /// [`myTran_DownloadFile`] (202). An `offset` above 0 sends
    /// [`myField_FileResumeData`] (203), so the server sends only the data
    /// from `offset` on. [`FileDownload::receive`] then reads it.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    /// [`myField_FileResumeData`]: hotline_proto::FieldId::FileResumeData
    pub async fn download_file(
        &self,
        path: &HlPath,
        name: &str,
        offset: u32,
    ) -> Result<FileDownload> {
        let mut fields = self.name_fields(path, name)?;
        if offset > 0 {
            fields.push(Field::bytes(
                FieldId::FileResumeData,
                types::ResumeData {
                    data: offset,
                    resource: 0,
                }
                .encode()
                .to_vec(),
            ));
        }
        let reply = self.request(Opcode::DownloadFile, fields).await?;
        let u32_field = |id| reply.field(id).map(Field::as_u32);
        Ok(FileDownload {
            file_size: u32_field(FieldId::FileSize).unwrap_or_default(),
            waiting_count: u32_field(FieldId::WaitingCount).unwrap_or_default(),
            reference: reference(&reply)?,
            transfer_addr: self.transfer_addr()?,
            encoding: self.encoding.get(),
        })
    }

    /// Cancel the download `reference` while it waits in the server's queue,
    /// [`myTran_KillDownload`] (214).
    ///
    /// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
    pub async fn kill_download(&self, reference: u32) -> Result<()> {
        self.request(
            Opcode::KillDownload,
            vec![Field::u32(FieldId::RefNum, reference)],
        )
        .await?;
        Ok(())
    }

    /// Ask to download the folder `name` in the folder at `path`, with
    /// everything inside it, [`myTran_DownloadFldr`] (210).
    /// [`FolderDownload::open`] then starts it.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    pub async fn download_folder(&self, path: &HlPath, name: &str) -> Result<FolderDownload> {
        let fields = self.name_fields(path, name)?;
        let reply = self.request(Opcode::DownloadFldr, fields).await?;
        let u32_field = |id| reply.field(id).map(Field::as_u32);
        let item_count = u32_field(FieldId::FldrItemCount).unwrap_or_default();
        Ok(FolderDownload {
            item_count,
            transfer_size: u32_field(FieldId::TransferSize).unwrap_or_default(),
            waiting_count: u32_field(FieldId::WaitingCount).unwrap_or_default(),
            reference: (item_count > 0).then(|| reference(&reply)).transpose()?,
            transfer_addr: self.transfer_addr()?,
            encoding: self.encoding.get(),
        })
    }

    /// Ask to upload `file_size` bytes as the file `name` in the folder at
    /// `path`, [`myTran_UploadFile`] (203). With `resume`, the upload continues
    /// the server's partial upload, and [`FileUpload::offset`] tells where.
    /// [`FileUpload::send`] then sends it.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    pub async fn upload_file(
        &self,
        path: &HlPath,
        name: &str,
        file_size: u64,
        resume: bool,
    ) -> Result<FileUpload> {
        let (file_size, transfer_size) = u32::try_from(file_size)
            .ok()
            .and_then(|size| Some((size, FileUpload::transfer_size(size)?)))
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidInput, "the file is too large")
            })?;
        let mut fields = self.name_fields(path, name)?;
        fields.push(if resume {
            Field::u16(FieldId::FileXferOptions, RESUME)
        } else {
            Field::u32(FieldId::TransferSize, transfer_size)
        });
        let reply = self.request(Opcode::UploadFile, fields).await?;
        let offset = if resume {
            let field = reply
                .field(FieldId::FileResumeData)
                .ok_or(ClientError::MissingField(FieldId::FileResumeData))?;
            types::ResumeData::decode(field.as_bytes())?
                .data
                .min(file_size)
        } else {
            0
        };
        Ok(FileUpload {
            reference: reference(&reply)?,
            file_size,
            offset,
            transfer_addr: self.transfer_addr()?,
        })
    }

    /// Ask to upload a folder of `item_count` files and folders, with
    /// `total_size` bytes of file data, as the folder `name` in the folder at
    /// `path`, [`myTran_UploadFldr`] (213). With `resume`, the upload goes
    /// into the server's existing folder. [`FolderUpload::open`] then starts
    /// it. It sends the low 32 bits of `total_size`.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    pub async fn upload_folder(
        &self,
        path: &HlPath,
        name: &str,
        item_count: u32,
        total_size: u64,
        resume: bool,
    ) -> Result<FolderUpload> {
        let total_size = total_size as u32;
        let mut fields = self.name_fields(path, name)?;
        fields.push(Field::u32(FieldId::TransferSize, total_size));
        fields.push(Field::u32(FieldId::FldrItemCount, item_count));
        if resume {
            fields.push(Field::u16(FieldId::FileXferOptions, RESUME));
        }
        let reply = self.request(Opcode::UploadFldr, fields).await?;
        Ok(FolderUpload {
            reference: (item_count > 0).then(|| reference(&reply)).transpose()?,
            item_count,
            transfer_addr: self.transfer_addr()?,
            encoding: self.encoding.get(),
        })
    }

    /// The server's `HTXF` address, one port above the transaction port.
    fn transfer_addr(&self) -> Result<SocketAddr> {
        let port = self.peer.port().checked_add(1).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::AddrNotAvailable,
                "no transfer port above 65535",
            )
        })?;
        Ok(SocketAddr::new(self.peer.ip(), port))
    }

    /// A [`myField_FilePath`] (202) field for `path`, `None` at the root.
    ///
    /// [`myField_FilePath`]: hotline_proto::FieldId::FilePath
    pub(crate) fn file_path_field(&self, path: &HlPath) -> Result<Option<Field>> {
        self.path_field(FieldId::FilePath, path)
    }

    /// An `id` field for `path`, `None` at the root.
    pub(crate) fn path_field(&self, id: FieldId, path: &HlPath) -> Result<Option<Field>> {
        if path.components.is_empty() {
            return Ok(None);
        }
        let encoding = self.encoding.get();
        Ok(Some(Field::bytes(id, path.encode(encoding)?)))
    }
}

/// Connect to the `HTXF` address `addr` and send `header`.
pub(crate) async fn open_transfer(addr: SocketAddr, header: HtxfHeader) -> Result<TcpStream> {
    let mut stream = TcpStream::connect(addr).await?;
    stream.write_all(&header.encode()).await?;
    Ok(stream)
}

/// A chat reply's [`myField_ChatID`] (114).
///
/// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
fn chat_id(reply: &Transaction<Received>) -> Result<u32> {
    reply
        .field(FieldId::ChatID)
        .map(Field::as_u32)
        .ok_or(ClientError::MissingField(FieldId::ChatID))
}

/// A transfer reply's [`myField_RefNum`] (107).
///
/// [`myField_RefNum`]: hotline_proto::FieldId::RefNum
fn reference(reply: &Transaction<Received>) -> Result<u32> {
    reply
        .field(FieldId::RefNum)
        .map(Field::as_u32)
        .ok_or(ClientError::MissingField(FieldId::RefNum))
}

impl Shared {
    /// Move `last_traffic` forward to now.
    fn touch(&self) {
        let nanos = u64::try_from(self.connected.elapsed().as_nanos()).unwrap_or(u64::MAX);
        self.last_traffic.fetch_max(nanos, Ordering::Relaxed);
    }
}

/// Forwards every transaction the server sends until the connection ends.
async fn read_loop(
    mut reader: SplitStream<Conn>,
    shared: Arc<Shared>,
    events: mpsc::UnboundedSender<Transaction<Received>>,
) {
    while let Some(Ok(tx)) = reader.next().await {
        shared.touch();
        if tx.is_reply() {
            let sender = match &mut *shared.pending.lock().await {
                PendingState::Open(map) => map.remove(&tx.trans()),
                PendingState::Closed => None,
            };
            if let Some(sender) = sender {
                let _ = sender.send(tx);
                continue;
            }
        }
        let _ = events.send(tx);
    }

    // Swapping in Closed under the same lock request() checks is what
    // makes this race-free.
    let old = std::mem::replace(&mut *shared.pending.lock().await, PendingState::Closed);
    drop(old); // dropping each sender fails its matching reply_rx.await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_counts_up_from_1() {
        let counter = AtomicTransCounter::new();
        assert_eq!(counter.next().get(), 1);
        assert_eq!(counter.next().get(), 2);
    }

    #[test]
    fn next_skips_0_at_the_wrap() {
        let counter = AtomicTransCounter(AtomicU32::new(u32::MAX));
        assert_eq!(counter.next().get(), u32::MAX);
        assert_eq!(counter.next().get(), 1);
        assert_eq!(counter.next().get(), 2);
    }
}
