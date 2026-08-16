use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use hotline_proto::htxf::{self, ForkHeader, HtxfHeader};

use crate::accounts::AccountProvider;
use crate::error::Result;
use crate::files::{FileProvider, FileRequester};
use crate::news::NewsProvider;
use crate::session::Session;
use crate::state::{PendingTransfer, SharedState, TransferDirection};

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind, e.g. `"0.0.0.0:5500"`. The file transfer data
    /// connection listens one port above this.
    pub bind_addr: String,
    /// Agreement text shown to clients on login (empty = no agreement).
    pub agreement: String,
    /// Name reported in the `Login` reply (protocol version >= 151).
    pub server_name: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".to_string(),
            agreement: "Hotline".to_string(),
            server_name: "Hotline".to_string(),
        }
    }
}

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: SharedState,
}

impl Server {
    /// Create a server with the given configuration, backed by `accounts`
    /// (see [`AccountProvider`]), `files` (see [`FileProvider`]), and
    /// `news` (see [`NewsProvider`]) - the embedding application supplies
    /// all three concrete backends.
    pub fn new(
        config: ServerConfig,
        accounts: Arc<dyn AccountProvider>,
        files: Arc<dyn FileProvider>,
        news: Arc<dyn NewsProvider>,
    ) -> Self {
        let state = SharedState::new(accounts, files, news);
        Self { config, state }
    }

    /// Bind and accept connections forever, spawning one task per client.
    /// Never returns unless the listener errors.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        tracing::info!(addr = %self.config.bind_addr, "hotline-server listening");

        let htxf_addr = SocketAddr::new(local_addr.ip(), local_addr.port() + 1);
        let htxf_listener = TcpListener::bind(htxf_addr).await?;
        tracing::info!(addr = %htxf_addr, "hotline-server listening for file transfers");
        let htxf_state = self.state.clone();
        tokio::spawn(async move {
            run_htxf_listener(htxf_listener, htxf_state).await;
        });

        loop {
            let (stream, peer) = listener.accept().await?;
            // Avoids Nagle delaying small replies behind the peer's ACKs.
            if let Err(e) = stream.set_nodelay(true) {
                tracing::warn!(%peer, error = %e, "failed to set TCP_NODELAY");
            }
            let state = self.state.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                tracing::debug!(%peer, "connection accepted");
                if let Err(e) = Session::run(stream, state, config).await {
                    tracing::warn!(%peer, error = %e, "session ended with error");
                }
            });
        }
    }
}

/// Accept `HTXF` data connections forever, spawning one task per
/// connection. Each connection completes exactly one pending transfer,
/// matched by the reference number in its handshake header.
async fn run_htxf_listener(listener: TcpListener, state: SharedState) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "htxf: accept failed");
                continue;
            }
        };
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_htxf_connection(stream, peer, &state).await {
                tracing::warn!(%peer, error = %e, "htxf: connection ended with error");
            }
        });
    }
}

/// Complete one `HTXF` data connection: read its 16-byte handshake
/// header, match it to a pending transfer registered by `FILE_GET`/
/// `FILE_PUT`, then stream the file in the appropriate direction.
async fn handle_htxf_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    state: &SharedState,
) -> Result<()> {
    let mut header_buf = [0u8; 16];
    stream.read_exact(&mut header_buf).await?;
    let header = HtxfHeader::decode(&header_buf)?;

    let Some(transfer) = state.take_transfer(header.reference).await else {
        tracing::warn!(%peer, reference = header.reference, "htxf: no pending transfer for reference");
        return Ok(());
    };
    if transfer.client_ip != peer.ip() {
        tracing::warn!(%peer, expected = %transfer.client_ip, "htxf: connection from wrong address for reference");
        return Ok(());
    }

    match transfer.direction {
        TransferDirection::Get => send_file(&mut stream, &transfer, state).await,
        TransferDirection::Put => receive_file(&mut stream, &transfer, state).await,
    }
}

/// Send a `FILE_GET` download: the flattened-file-object header, then
/// the file's raw bytes. `transfer.resume_offset` skips that many leading
/// bytes of the data fork, reducing the header's declared size to match,
/// for a client resuming an interrupted download.
async fn send_file(
    stream: &mut TcpStream,
    transfer: &PendingTransfer,
    state: &SharedState,
) -> Result<()> {
    let requester = FileRequester {
        login: &transfer.login,
    };
    let mut reader = state
        .open_file_for_read(requester, &transfer.dir, &transfer.name)
        .await?;
    let remaining = transfer.size - transfer.resume_offset;
    stream
        .write_all(&htxf::flat_file_header(remaining as u32))
        .await?;
    if transfer.resume_offset > 0 {
        skip_bytes(&mut reader, transfer.resume_offset).await?;
    }
    tokio::io::copy(&mut reader, stream).await?;
    Ok(())
}

/// Discard the first `n` bytes read from `reader`, for starting a resumed
/// download partway through the file.
async fn skip_bytes(reader: &mut (impl AsyncRead + Unpin), mut n: u64) -> Result<()> {
    let mut buf = [0u8; 8192];
    while n > 0 {
        let want = n.min(buf.len() as u64) as usize;
        reader.read_exact(&mut buf[..want]).await?;
        n -= want as u64;
    }
    Ok(())
}

/// Receive a `FILE_PUT` upload: parse the client's flattened-file-object
/// framing, writing the `DATA` fork's bytes to storage and discarding
/// any other fork (e.g. a resource fork this server doesn't keep).
async fn receive_file(
    stream: &mut TcpStream,
    transfer: &PendingTransfer,
    state: &SharedState,
) -> Result<()> {
    let mut prefix = [0u8; htxf::FLAT_FILE_PREFIX_LEN];
    stream.read_exact(&mut prefix).await?;
    let fork_count = htxf::decode_flat_file_prefix(&prefix)?;

    let requester = FileRequester {
        login: &transfer.login,
    };
    let mut out = state
        .create_file_for_write(requester, &transfer.dir, &transfer.name)
        .await?;

    for _ in 0..fork_count {
        let mut fork_header_buf = [0u8; htxf::ForkHeader::LEN];
        stream.read_exact(&mut fork_header_buf).await?;
        let fork_header = ForkHeader::decode(&fork_header_buf)?;

        let mut remaining = fork_header.data_size as u64;
        if &fork_header.fork_type == b"DATA" {
            let mut limited = stream.take(remaining);
            tokio::io::copy(&mut limited, &mut out).await?;
        } else {
            let mut discard_buf = [0u8; 4096];
            while remaining > 0 {
                let n = remaining.min(discard_buf.len() as u64) as usize;
                stream.read_exact(&mut discard_buf[..n]).await?;
                remaining -= n as u64;
            }
        }
    }
    out.shutdown().await?;
    Ok(())
}
