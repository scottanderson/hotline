use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use hotline_proto::htxf::{self, ForkHeader, HtxfHeader};

use crate::accounts::AccountProvider;
use crate::banner::BannerProvider;
use crate::bans::BanProvider;
use crate::error::Result;
use crate::files::{FileProvider, FileRequester};
use crate::flood::FloodPolicy;
use crate::news::NewsProvider;
use crate::session::Session;
use crate::state::{PendingTransfer, SharedState, TransferDirection, TransferKind};

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
    /// (see [`AccountProvider`]), `files` (see [`FileProvider`]), `news`
    /// (see [`NewsProvider`]), `bans` (see [`BanProvider`]), `flood` (see
    /// [`FloodPolicy`]), and `banner` (see [`BannerProvider`]) - the
    /// embedding application supplies all six concrete backends.
    pub fn new(
        config: ServerConfig,
        accounts: Arc<dyn AccountProvider>,
        files: Arc<dyn FileProvider>,
        news: Arc<dyn NewsProvider>,
        bans: Arc<dyn BanProvider>,
        flood: Arc<dyn FloodPolicy>,
        banner: Arc<dyn BannerProvider>,
    ) -> Self {
        let state = SharedState::new(accounts, files, news, bans, flood, banner);
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
            // Reject a banned address before it can consume a handshake or
            // any other protocol resource, matching how the reference
            // server checks its ban list immediately on accept.
            if self.state.is_banned(peer.ip()).await.is_some() {
                tracing::debug!(%peer, "rejected connection from banned address");
                continue;
            }
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

    match transfer.kind {
        TransferKind::Banner => send_banner(&mut stream, state).await,
        TransferKind::File => match transfer.direction {
            TransferDirection::Get => send_file(&mut stream, &transfer, state).await,
            TransferDirection::Put => receive_file(&mut stream, &transfer, state).await,
        },
    }
}

/// Send a `BANNER_GET` download: the banner image's raw bytes, with none
/// of a file transfer's flattened-object framing.
async fn send_banner(stream: &mut TcpStream, state: &SharedState) -> Result<()> {
    let mut reader = state.open_banner_read().await?;
    tokio::io::copy(&mut reader, stream).await?;
    drain_stream(stream).await
}

/// Read (discarding) until the client closes, instead of closing our
/// side first. A client may still be draining its last buffered chunk
/// when a server-initiated close arrives, and reports that as a failed
/// transfer even after reading every byte.
async fn drain_stream(stream: &mut TcpStream) -> Result<()> {
    let mut discard = [0u8; 4096];
    while stream.read(&mut discard).await? != 0 {}
    Ok(())
}

/// Send a `FILE_GET` download: the flattened-file-object header, then
/// the file's raw bytes. `transfer.raw` sends the raw bytes alone, with
/// no header, like [`send_banner`]. `transfer.resume_offset` skips that
/// many leading bytes of the data fork, reducing the header's declared
/// size to match, for a client resuming an interrupted download.
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
    if !transfer.raw {
        let remaining = transfer.size - transfer.resume_offset;
        stream
            .write_all(&htxf::flat_file_header(remaining as u32))
            .await?;
        if transfer.resume_offset > 0 {
            skip_bytes(&mut reader, transfer.resume_offset).await?;
        }
    }
    tokio::io::copy(&mut reader, stream).await?;
    drain_stream(stream).await
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
/// `transfer.resume_offset > 0` seeks the destination to that offset
/// instead of creating it fresh. Renames the upload to its real name
/// (see [`SharedState::finish_upload`]) on success; an error leaves it
/// under its working name for a later resume.
async fn receive_file(
    stream: &mut TcpStream,
    transfer: &PendingTransfer,
    state: &SharedState,
) -> Result<()> {
    let requester = FileRequester {
        login: &transfer.login,
    };
    let mut out = if transfer.resume_offset > 0 {
        state
            .resume_file_for_write(
                requester,
                &transfer.dir,
                &transfer.name,
                transfer.resume_offset,
            )
            .await?
    } else {
        state
            .create_file_for_write(requester, &transfer.dir, &transfer.name)
            .await?
    };

    let mut prefix = [0u8; htxf::FLAT_FILE_PREFIX_LEN];
    stream.read_exact(&mut prefix).await?;
    let fork_count = htxf::decode_flat_file_prefix(&prefix)?;

    for _ in 0..fork_count {
        let mut fork_header_buf = [0u8; ForkHeader::LEN];
        stream.read_exact(&mut fork_header_buf).await?;
        let fork_header = ForkHeader::decode(&fork_header_buf)?;

        let mut remaining = fork_header.data_size as u64;
        if &fork_header.fork_type == b"DATA" {
            let mut limited = stream.take(remaining);
            let copied = tokio::io::copy(&mut limited, &mut out).await?;
            if copied != remaining {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
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
    state
        .finish_upload(requester, &transfer.dir, &transfer.name)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::InMemoryAccountProvider;
    use crate::banner::NoBanner;
    use crate::bans::InMemoryBanProvider;
    use crate::files::{FileRequester, InMemoryFileProvider};
    use crate::flood::NoFloodPolicy;
    use crate::news::InMemoryNewsProvider;
    use hotline_proto::consts::Access;
    use hotline_proto::types::HlPath;
    use tokio::io::AsyncWriteExt;

    fn state_with_files(files: InMemoryFileProvider) -> SharedState {
        SharedState::new(
            Arc::new(InMemoryAccountProvider::default()),
            Arc::new(files),
            Arc::new(InMemoryNewsProvider::default()),
            Arc::new(InMemoryBanProvider::default()),
            Arc::new(NoFloodPolicy),
            Arc::new(NoBanner),
        )
    }

    /// `tokio::io::copy` returns early on a closed connection the same
    /// way it would at a clean EOF, so a cancelled upload must not be
    /// renamed to its real name.
    #[tokio::test]
    async fn a_cancelled_upload_is_not_renamed_off_its_hpf_name() {
        let state = state_with_files(InMemoryFileProvider::default());
        let requester = FileRequester { login: "alice" };
        let transfer = PendingTransfer {
            kind: TransferKind::File,
            direction: TransferDirection::Put,
            login: "alice".to_string(),
            dir: HlPath::default(),
            name: "a.txt".to_string(),
            size: 11,
            resume_offset: 0,
            raw: false,
            client_ip: "127.0.0.1".parse().unwrap(),
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let full = htxf::encode_flat_file(b"hello world");
        client.write_all(&full[..full.len() - 1]).await.unwrap();
        drop(client);

        let result = receive_file(&mut server_stream, &transfer, &state).await;
        assert!(result.is_err());

        assert_eq!(
            state
                .file_size(requester, &HlPath::default(), "a.txt.hpf", Access::all())
                .await,
            Ok(10)
        );
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    /// `resume_file_for_write` seeks to `resume_offset` before the
    /// client's bytes land, so a longer working file from an earlier
    /// attempt doesn't leave stale bytes past what the client resends.
    #[tokio::test]
    async fn resuming_an_upload_discards_bytes_already_written_past_the_resume_offset() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt.hpf", *b"hello world");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let transfer = PendingTransfer {
            kind: TransferKind::File,
            direction: TransferDirection::Put,
            login: "alice".to_string(),
            dir: HlPath::default(),
            name: "a.txt".to_string(),
            size: 11,
            resume_offset: 6,
            raw: false,
            client_ip: "127.0.0.1".parse().unwrap(),
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let resumed = htxf::encode_flat_file(b"there");
        client.write_all(&resumed).await.unwrap();
        drop(client);

        receive_file(&mut server_stream, &transfer, &state)
            .await
            .unwrap();

        assert_eq!(
            state
                .file_size(requester, &HlPath::default(), "a.txt", Access::all())
                .await,
            Ok(11)
        );
        let mut reader = state
            .open_file_for_read(requester, &HlPath::default(), "a.txt")
            .await
            .unwrap();
        let mut buf = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut buf)
            .await
            .unwrap();
        assert_eq!(buf, b"hello there");
    }
}
