use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, ToSocketAddrs};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_util::codec::Framed;

use hotline_codec::TransactionCodec;
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use hotline_proto::{
    ClientFieldId, ClientHandshake, ClientOpcode, Field, ServerFieldId, ServerHandshake,
    Transaction,
};

use crate::error::{ClientError, Result};

type Conn = Framed<TcpStream, TransactionCodec>;

/// A pending-request table: still accepting new requests, or
/// permanently closed because the connection ended. See [`read_loop`]'s
/// shutdown handling.
enum PendingState {
    Open(HashMap<u32, oneshot::Sender<Transaction>>),
    Closed,
}

type PendingMap = Arc<Mutex<PendingState>>;

/// Fields returned by a successful login, per the `HTLC_HDR_LOGIN` (107) reply.
#[derive(Debug, Clone, Default)]
pub struct LoginResult {
    /// Server's protocol version, echoed back.
    pub version: Option<u16>,
    /// Banner ID, used to fetch the banner over HTTP (version >= 151 only).
    pub banner_id: Option<u16>,
    /// Human-readable server name (version >= 151 only).
    pub server_name: Option<String>,
}

/// A connected, logged-in-or-not Hotline client session. Cloning is
/// cheap: it shares the same connection and pending-request table. The
/// read half runs on its own spawned task ([`read_loop`]); this struct
/// only touches the write half.
#[derive(Clone)]
pub struct HotlineClient {
    writer: Arc<Mutex<SplitSink<Conn, Transaction>>>,
    pending: PendingMap,
    next_id: Arc<AtomicU32>,
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

        let (writer, reader) = Framed::new(stream, TransactionCodec::for_client()).split();
        let pending: PendingMap = Arc::new(Mutex::new(PendingState::Open(HashMap::new())));
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        tokio::spawn(read_loop(reader, pending.clone(), event_tx));

        Ok((
            Self {
                writer: Arc::new(Mutex::new(writer)),
                pending,
                next_id: Arc::new(AtomicU32::new(1)),
            },
            event_rx,
        ))
    }

    /// Send a request transaction and wait for its matching reply.
    /// Returns `Err` if the server replied with the error bit set.
    pub async fn request(&self, opcode: ClientOpcode, fields: Vec<Field>) -> Result<Transaction> {
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
            let msg = reply
                .field(ServerFieldId::ErrorText)
                .map(Field::as_string)
                .unwrap_or_else(|| "unknown error".to_string());
            return Err(ClientError::Server(msg));
        }
        Ok(reply)
    }

    /// Log in with the given credentials (`HTLC_HDR_LOGIN`, 107). `login`
    /// and `password` are obfuscated with Hotline's XOR-0xFF transform
    /// before going on the wire.
    ///
    /// The server follows the reply with an `HTLS_HDR_AGREEMENT` (109)
    /// push on the event stream returned by [`Self::connect`], carrying
    /// either agreement text or a no-agreement marker. Call [`Self::agree`]
    /// with the caller's name/icon after handling it to complete login.
    pub async fn login(&self, login: &str, password: &str) -> Result<LoginResult> {
        let reply = self
            .request(
                ClientOpcode::Login,
                vec![
                    Field::obfuscated_string(ClientFieldId::Login, login),
                    Field::obfuscated_string(ClientFieldId::Password, password),
                    Field::u16(ClientFieldId::Version, 151),
                ],
            )
            .await?;

        Ok(LoginResult {
            version: reply
                .field(ServerFieldId::Serverversion)
                .and_then(Field::as_u16),
            banner_id: reply.field(ServerFieldId::Bannerid).and_then(Field::as_u16),
            server_name: reply.field(ServerFieldId::Servername).map(Field::as_string),
        })
    }

    /// Accept the server's agreement, completing login (`HTLC_HDR_AGREED`, 121).
    pub async fn agree(&self, nick: &str, icon: u16) -> Result<()> {
        self.request(
            ClientOpcode::Agreed,
            vec![
                Field::string(ClientFieldId::Name, nick),
                Field::u16(ClientFieldId::Icon, icon),
            ],
        )
        .await?;
        Ok(())
    }

    /// Send a public chat message (`HTLC_HDR_CHAT`, 105).
    pub async fn send_chat(&self, message: &str) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tx = Transaction::request(
            ClientOpcode::Chat,
            id,
            vec![Field::string(ClientFieldId::Text, message)],
        );
        self.writer.lock().await.send(tx).await?;
        Ok(())
    }

    /// Fetch the connected-user list (`HTLC_HDR_USER_GETLIST`, 300).
    pub async fn get_user_name_list(&self) -> Result<Vec<UserNameWithInfo>> {
        let reply = self.request(ClientOpcode::UserGetlist, vec![]).await?;
        Ok(reply
            .fields
            .iter()
            .filter(|f| f.id == u16::from(ServerFieldId::UserList))
            .map(|f| UserNameWithInfo::decode(&f.as_bytes()))
            .collect::<hotline_proto::Result<_>>()?)
    }

    /// Fetch the root folder's file listing (`HTLC_HDR_FILE_LIST`, 200).
    pub async fn get_file_name_list(&self) -> Result<Vec<FileNameWithInfo>> {
        let reply = self.request(ClientOpcode::FileList, vec![]).await?;
        Ok(reply
            .fields
            .iter()
            .filter(|f| f.id == u16::from(ServerFieldId::FileList))
            .map(|f| FileNameWithInfo::decode(&f.as_bytes()))
            .collect::<hotline_proto::Result<_>>()?)
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
