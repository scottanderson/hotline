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
    ClientFieldId, ClientHello, ClientOpcode, Field, ServerFieldId, ServerHello, Transaction,
};

use crate::error::{ClientError, Result};

type Conn = Framed<TcpStream, TransactionCodec>;

/// What a pending-request table looks like: still accepting new
/// requests, or permanently closed because the connection ended. See
/// [`read_loop`]'s shutdown handling for why this needs to be a single
/// state behind one lock rather than a plain map that just gets cleared
/// once, since a request racing the connection's death must never be able to
/// insert *after* that one-time clear and wait forever unanswered.
enum PendingState {
    Open(HashMap<u32, oneshot::Sender<Transaction>>),
    Closed,
}

type PendingMap = Arc<Mutex<PendingState>>;

/// Fields returned by a successful login, per the `Login` (107) reply.
#[derive(Debug, Clone, Default)]
pub struct LoginResult {
    /// Server's protocol version, echoed back.
    pub version: Option<u16>,
    /// Banner ID, used to fetch the banner over HTTP (version >= 151 only).
    pub banner_id: Option<u16>,
    /// Human-readable server name (version >= 151 only).
    pub server_name: Option<String>,
}

/// A connected, logged-in-or-not Hotline client session.
///
/// Cloning is cheap: it shares the same underlying connection and pending
/// request table, so multiple call sites can issue requests concurrently.
///
/// Framing is [`hotline_codec::TransactionCodec`] over
/// [`tokio_util::codec::Framed`]. This struct only touches the write half
/// ([`futures::stream::SplitSink`]); the read half runs on its own
/// spawned task ([`read_loop`]) as a `Stream::next()` pipeline.
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

        // The TRTP handshake (12 bytes out, 8 bytes back) predates - and
        // isn't shaped like - a Transaction, so it's done on the raw
        // stream before handing it to the codec.
        stream.write_all(&ClientHello::default().encode()).await?;

        let mut hello_buf = [0u8; 8];
        stream.read_exact(&mut hello_buf).await?;
        ServerHello::decode(&hello_buf)?;

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
                // Checked and inserted under the same lock read_loop's
                // shutdown uses to transition to `Closed` - there's no
                // window where this could insert just after a one-time
                // cleanup already ran and be left waiting forever.
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

    /// Log in with the given credentials, per the `Login` (107) sequence
    /// described in the protocol reference. Icon defaults to `0` if not
    /// meaningful for your client.
    ///
    /// Both `login` and `password` are obfuscated with Hotline's
    /// symmetric XOR-0xFF transform before going on the wire. Real
    /// servers expect *both* fields encoded this way.
    pub async fn login(
        &self,
        login: &str,
        password: &str,
        nick: &str,
        icon: u16,
    ) -> Result<LoginResult> {
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

        let result = LoginResult {
            version: reply
                .field(ServerFieldId::Serverversion)
                .and_then(Field::as_u16),
            banner_id: reply.field(ServerFieldId::Bannerid).and_then(Field::as_u16),
            server_name: reply.field(ServerFieldId::Servername).map(Field::as_string),
        };

        // Announce nick/icon. Real clients also wait for Show Agreement
        // (109) and send Agreed (121) here; see hlc's session flow for the
        // fuller happy path.
        self.set_client_user_info(nick, icon).await?;
        Ok(result)
    }

    /// Send `SET_CLIENT_USER_INFO` (304) to announce nick/icon.
    pub async fn set_client_user_info(&self, nick: &str, icon: u16) -> Result<()> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let tx = Transaction::request(
            ClientOpcode::UserChange,
            id,
            vec![
                Field::string(ClientFieldId::Name, nick),
                Field::u16(ClientFieldId::Icon, icon),
                Field::u16(ClientFieldId::Options, 0),
            ],
        );
        // No reply expected per the reference.
        self.writer.lock().await.send(tx).await?;
        Ok(())
    }

    /// Send a public chat message (`CHAT_SEND`, 105).
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

    /// Fetch the connected-user list (`GET_USER_NAME_LIST`, 300).
    pub async fn get_user_name_list(&self) -> Result<Vec<UserNameWithInfo>> {
        let reply = self.request(ClientOpcode::UserGetlist, vec![]).await?;
        Ok(reply
            .fields
            .iter()
            .filter(|f| f.id == u16::from(ServerFieldId::UserList))
            .filter_map(|f| UserNameWithInfo::decode(&f.as_bytes()).ok())
            .collect())
    }

    /// Fetch the root folder's file listing (`GET_FILE_NAME_LIST`, 200).
    pub async fn get_file_name_list(&self) -> Result<Vec<FileNameWithInfo>> {
        let reply = self.request(ClientOpcode::FileList, vec![]).await?;
        Ok(reply
            .fields
            .iter()
            .filter(|f| f.id == u16::from(ServerFieldId::FileList))
            .filter_map(|f| FileNameWithInfo::decode(&f.as_bytes()).ok())
            .collect())
    }
}

/// Forwards every transaction the server sends until the connection ends
/// (clean close, a framing error, or the transport failing outright) -
/// see the note at the end of this function on why that end condition
/// must be handled explicitly rather than just letting the loop exit.
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

    // The loop above ends on a clean EOF, a framing error, or any other
    // transport failure - in every case, nothing is ever going to answer
    // whatever's still sitting in `pending`, or anything inserted after
    // this point. Swapping in `Closed` here, under the same lock
    // `request()` checks before inserting, is what makes this race-free:
    // a request either wins the race and gets included in `old` below
    // (so its sender gets dropped, resolving its `reply_rx.await` with an
    // error), or loses the race and sees `Closed` immediately, failing
    // fast instead of ever waiting on an answer that will never come.
    let old = std::mem::replace(&mut *pending.lock().await, PendingState::Closed);
    drop(old); // dropping each sender fails its matching reply_rx.await
}
