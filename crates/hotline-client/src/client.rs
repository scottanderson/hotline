use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use hotline_proto::text_encoding::EncodingHandle;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use hotline_proto::{ClientHandshake, Field, FieldId, Opcode, ServerHandshake, Transaction};

use crate::error::{ClientError, Result};
use crate::login_result::LoginResult;

type Conn = Framed<TcpStream, TransactionCodec>;

/// A pending-request table: still accepting new requests, or
/// permanently closed because the connection ended. See [`read_loop`]'s
/// shutdown handling.
enum PendingState {
    Open(HashMap<u32, oneshot::Sender<Transaction>>),
    Closed,
}

type PendingMap = Arc<Mutex<PendingState>>;

/// A connected, logged-in-or-not Hotline client session. Cloning is
/// cheap: it shares the same connection and pending-request table. The
/// read half runs on its own spawned task (`read_loop`); this struct
/// only touches the write half.
#[derive(Clone)]
pub struct HotlineClient {
    writer: Arc<Mutex<SplitSink<Conn, Transaction>>>,
    pending: PendingMap,
    next_id: Arc<AtomicU32>,
    encoding: EncodingHandle,
}

impl HotlineClient {
    /// Connect to `addr`, perform the `TRTP` handshake, and start the
    /// background read loop. Returns the client plus a channel that
    /// receives every server-initiated transaction (chat messages, user
    /// list changes, news posts, disconnect notices, etc).
    pub async fn connect<A: ToSocketAddrs>(
        addr: A,
    ) -> Result<(Self, mpsc::UnboundedReceiver<Transaction>)> {
        let mut stream = TcpStream::connect(addr).await?;

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
        let pending: PendingMap = Arc::new(Mutex::new(PendingState::Open(HashMap::new())));
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(read_loop(reader, pending.clone(), event_tx));

        Ok((
            Self {
                writer: Arc::new(Mutex::new(writer)),
                pending,
                next_id: Arc::new(AtomicU32::new(1)),
                encoding,
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

    /// Send a request transaction and wait for its matching reply.
    /// Returns `Err` if the server replied with the error bit set.
    pub async fn request(&self, opcode: Opcode, fields: Vec<Field>) -> Result<Transaction> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tx = Transaction::request(opcode, id, fields);

        let (reply_tx, reply_rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            match &mut *pending {
                PendingState::Open(map) => {
                    map.insert(id, reply_tx);
                }
                PendingState::Closed => return Err(ClientError::ConnectionClosed),
            }
        }

        if let Err(e) = self.writer.lock().await.send(tx).await {
            if let PendingState::Open(map) = &mut *self.pending.lock().await {
                map.remove(&id);
            }
            return Err(e.into());
        }

        let reply = reply_rx.await.map_err(|_| ClientError::ConnectionClosed)?;
        if reply.is_error() {
            let encoding = self.encoding.get();
            let msg = reply
                .string(FieldId::ErrorText, encoding)
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
                    Field::u16(FieldId::Vers, 151),
                ],
            )
            .await?;

        let encoding = self.encoding.get();
        Ok(LoginResult {
            version: reply.field(FieldId::Vers).and_then(Field::as_u16),
            banner_id: reply
                .field(FieldId::CommunityBannerID)
                .and_then(Field::as_u16),
            server_name: reply.string(FieldId::ServerName, encoding).map(Into::into),
        })
    }

    /// Accept the server's agreement, completing login, [`myTran_Agreed`] (121).
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
    pub async fn agree(&self, nick: &str, icon: u16) -> Result<()> {
        self.request(
            Opcode::Agreed,
            vec![
                Field::string(FieldId::UserName, nick),
                Field::u16(FieldId::UserIconID, icon),
            ],
        )
        .await?;
        Ok(())
    }

    /// Send a public chat message, [`myTran_ChatSend`] (105).
    ///
    /// [`myTran_ChatSend`]: hotline_proto::Opcode::ChatSend
    pub async fn send_chat(&self, message: &str) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tx = Transaction::request(
            Opcode::ChatSend,
            id,
            vec![Field::string(FieldId::Data, message)],
        );
        self.writer.lock().await.send(tx).await?;
        Ok(())
    }

    /// Fetch the connected-user list, [`myTran_GetUserNameList`] (300).
    ///
    /// [`myTran_GetUserNameList`]: hotline_proto::Opcode::GetUserNameList
    pub async fn get_user_name_list(&self) -> Result<Vec<UserNameWithInfo>> {
        let reply = self.request(Opcode::GetUserNameList, vec![]).await?;
        let encoding = self.encoding.get();
        Ok(reply.user_list_entries(encoding)?)
    }

    /// Fetch the root folder's file listing, [`myTran_GetFileNameList`] (200).
    ///
    /// [`myTran_GetFileNameList`]: hotline_proto::Opcode::GetFileNameList
    pub async fn get_file_name_list(&self) -> Result<Vec<FileNameWithInfo>> {
        let reply = self.request(Opcode::GetFileNameList, vec![]).await?;
        let encoding = self.encoding.get();
        Ok(reply.file_list_entries(encoding)?)
    }
}

/// Forwards every transaction the server sends until the connection ends.
async fn read_loop(
    mut reader: SplitStream<Conn>,
    pending: PendingMap,
    events: mpsc::UnboundedSender<Transaction>,
) {
    while let Some(Ok(tx)) = reader.next().await {
        if tx.is_reply() {
            let sender = match &mut *pending.lock().await {
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
    let old = std::mem::replace(&mut *pending.lock().await, PendingState::Closed);
    drop(old); // dropping each sender fails its matching reply_rx.await
}
