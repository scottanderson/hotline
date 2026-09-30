use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use hotline_proto::htxf::{self, FolderAction, FolderItemHeader, ForkHeader, HtxfHeader};
use hotline_proto::types::{self, HlPath};

use crate::error::Result;
use crate::file::FileRequester;
use crate::server::idle_stream::IdleStream;
use crate::state::{
    FileTransfer, FolderTransfer, PendingTransfer, SharedState, TransferDirection, TransferKind,
    TransferRef, TransferTarget,
};

/// Time a new `HTXF` connection may take to send its header.
const HEADER_TIMEOUT: Duration = Duration::from_secs(60);

/// Time an upload may go without receiving a byte, openline's 120 s.
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Accept `HTXF` data connections forever, spawning one task per
/// connection. Each connection completes exactly one pending transfer,
/// matched by the reference number in its handshake header.
pub(crate) async fn run_htxf_listener(listener: TcpListener, state: SharedState) {
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

/// Complete one `HTXF` data connection: match its handshake to a pending
/// banner, file, or folder transfer, then run it.
async fn handle_htxf_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    state: &SharedState,
) -> Result<()> {
    let mut header_buf = [0u8; 16];
    let Ok(read) = tokio::time::timeout(HEADER_TIMEOUT, stream.read_exact(&mut header_buf)).await
    else {
        tracing::debug!(%peer, "htxf: header timed out");
        return Ok(());
    };
    read?;
    let header = HtxfHeader::decode(&header_buf)?;

    let reference = TransferRef(header.reference);
    let Some(transfer) = state.take_transfer(reference).await else {
        tracing::warn!(%peer, reference = header.reference, "htxf: no pending transfer for reference");
        return Ok(());
    };
    if transfer.client_ip != peer.ip() {
        tracing::warn!(%peer, expected = %transfer.client_ip, "htxf: connection from wrong address for reference");
        state.release_download_slot(reference).await;
        return Ok(());
    }

    if transfer.direction == TransferDirection::Get
        && !matches!(transfer.kind, TransferKind::Banner)
        && !wait_for_slot_or_close(&stream, state, reference).await
    {
        return Ok(());
    }
    match &transfer.kind {
        TransferKind::Banner => send_banner(&mut stream, state).await,
        TransferKind::File(file) => match transfer.direction {
            TransferDirection::Get => {
                let send = send_file(&mut stream, file, &transfer, state);
                run_download(state, reference, send).await
            }
            TransferDirection::Put => {
                let mut stream = IdleStream::new(&mut stream, UPLOAD_IDLE_TIMEOUT);
                let receive = receive_file(&mut stream, file, &transfer, state);
                run_upload(state, transfer.user_id, receive).await
            }
        },
        TransferKind::Folder(folder) => match transfer.direction {
            TransferDirection::Get => {
                let send = send_folder(&mut stream, folder, state);
                run_download(state, reference, send).await
            }
            TransferDirection::Put => {
                let mut stream = IdleStream::new(&mut stream, UPLOAD_IDLE_TIMEOUT);
                let receive = receive_folder(&mut stream, folder, state);
                run_upload(state, transfer.user_id, receive).await
            }
        },
    }
}

/// `false` if download `reference` leaves the queue or `stream` closes
/// before it holds a slot.
async fn wait_for_slot_or_close(
    stream: &TcpStream,
    state: &SharedState,
    reference: TransferRef,
) -> bool {
    let closed = async {
        let mut probe = [0u8; 1];
        if let Ok(1..) = stream.peek(&mut probe).await {
            // Early data is not a close; keep waiting for the slot alone.
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {
        granted = state.wait_for_download_slot(reference) => granted,
        () = closed => {
            state.release_download_slot(reference).await;
            false
        }
    }
}

/// Run `receive` until it ends or its owner's session ends.
async fn run_upload(
    state: &SharedState,
    user_id: u16,
    receive: impl Future<Output = Result<()>>,
) -> Result<()> {
    let Some(session_ended) = state.session_ended(user_id).await else {
        return Ok(());
    };
    tokio::select! {
        result = receive => result,
        () = session_ended => Ok(()),
    }
}

/// Run `send` for download `reference`, which holds a slot, then free
/// the slot. A [`myTran_KillDownload`] (214) stops it early.
///
/// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
async fn run_download(
    state: &SharedState,
    reference: TransferRef,
    send: impl Future<Output = Result<()>>,
) -> Result<()> {
    let Some(cancel) = state.download_cancel_token(reference) else {
        return Ok(());
    };
    let result = tokio::select! {
        result = send => result,
        () = cancel.cancelled() => Ok(()),
    };
    state.release_download_slot(reference).await;
    result
}

/// Send a [`myTran_DownloadBanner`] (212) download: the banner image's raw bytes,
/// with none of a file transfer's flattened-object framing.
///
/// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
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

/// Send a [`myTran_DownloadFile`] (202) download: the flattened-file-object
/// header, then the file's raw bytes. `transfer.raw` sends the raw bytes alone,
/// with no header, like [`send_banner`]. `transfer.resume_offset` skips that
/// many leading bytes of the data fork, reducing the header's declared size to
/// match, for a client resuming an interrupted download.
///
/// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
async fn send_file(
    stream: &mut TcpStream,
    file: &FileTransfer,
    transfer: &PendingTransfer,
    state: &SharedState,
) -> Result<()> {
    let FileTransfer { target, raw } = file;
    let TransferTarget { dir, name, .. } = target;
    let requester = target.requester();
    let mut reader = state.open_file_for_read(requester, dir, name).await?;
    if *raw {
        tokio::io::copy(&mut reader, stream).await?;
    } else {
        send_flat_file(stream, reader, transfer.size, transfer.resume_offset).await?;
    }
    drain_stream(stream).await
}

/// Write a flattened file object for `reader`'s `size` bytes, skipping
/// the first `resume_offset` of them.
async fn send_flat_file(
    stream: &mut TcpStream,
    mut reader: impl AsyncRead + Unpin,
    size: u64,
    resume_offset: u64,
) -> Result<()> {
    let remaining = size - resume_offset;
    stream
        .write_all(&htxf::flat_file_header(remaining as u32))
        .await?;
    if resume_offset > 0 {
        skip_bytes(&mut reader, resume_offset).await?;
    }
    tokio::io::copy(&mut reader.take(remaining), stream).await?;
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

/// Receive a [`myTran_UploadFile`] (203) upload: parse the client's
/// flattened-file-object framing, writing the `DATA` fork's bytes to storage
/// and discarding any other fork (e.g. a resource fork this server doesn't
/// keep). `transfer.resume_offset > 0` seeks the destination to that offset
/// instead of creating it fresh. Renames the upload to its real name (see
/// [`SharedState::finish_upload`]) on success; an error leaves it under its
/// working name for a later resume.
///
/// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
async fn receive_file(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    file: &FileTransfer,
    transfer: &PendingTransfer,
    state: &SharedState,
) -> Result<()> {
    let TransferTarget { dir, name, .. } = &file.target;
    let requester = file.target.requester();
    let mut out = if transfer.resume_offset > 0 {
        state
            .resume_file_for_write(requester, dir, name, transfer.resume_offset)
            .await?
    } else {
        state.create_file_for_write(requester, dir, name).await?
    };
    receive_flat_file(stream, &mut out).await?;
    out.shutdown().await?;
    state.finish_upload(requester, dir, name).await?;
    Ok(())
}

/// Read one flattened file object, writing its `DATA` fork to `out` and
/// discarding any other fork.
async fn receive_flat_file(
    stream: &mut (impl AsyncRead + Unpin),
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
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
            let copied = tokio::io::copy(&mut limited, out).await?;
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
    Ok(())
}

/// Send a [`myTran_DownloadFldr`] (210) download: the next item's
/// [`FolderItemHeader`] on each `NextFile`, then a file's size and
/// flattened object on `SendFile`/`ResumeFile`. Ends once the items run
/// out or the client closes.
///
/// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
async fn send_folder(
    stream: &mut TcpStream,
    folder: &FolderTransfer,
    state: &SharedState,
) -> Result<()> {
    let FolderTransfer {
        target,
        items,
        encoding,
        ..
    } = folder;
    let TransferTarget { dir, name, .. } = target;
    let requester = target.requester();
    let root = folder_root(dir, name);
    let mut items = items.iter();
    // The file whose header was just sent, until its data is asked for.
    let mut pending_file = None;
    while let Some(action) = read_folder_action(stream).await? {
        match action {
            FolderAction::NextFile => {
                let Some(item) = items.next() else {
                    break;
                };
                let header = FolderItemHeader {
                    is_folder: item.is_folder,
                    path: item.path.clone(),
                };
                stream.write_all(&header.encode(encoding)?).await?;
                pending_file = (!item.is_folder).then_some(item);
            }
            FolderAction::SendFile | FolderAction::ResumeFile => {
                let Some(item) = pending_file.take() else {
                    continue;
                };
                let resume_offset = if action == FolderAction::ResumeFile {
                    u64::from(read_resume_data(stream).await?).min(item.size)
                } else {
                    0
                };
                let (dir, name) = split_item_path(&root, &item.path)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                let reader = state.open_file_for_read(requester, &dir, name).await?;
                let flat_size = htxf::FLAT_FILE_HEADER_LEN as u64 + item.size - resume_offset;
                stream.write_all(&(flat_size as u32).to_be_bytes()).await?;
                send_flat_file(stream, reader, item.size, resume_offset).await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Receive a [`myTran_UploadFldr`] (213) upload: ask for each item's header with
/// `NextFile`, then receive it with [`receive_folder_item`]. Ends after
/// the declared item count. Every item gets a `NextFile` after it except
/// a last item whose data was received.
///
/// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
async fn receive_folder(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    folder: &FolderTransfer,
    state: &SharedState,
) -> Result<()> {
    let FolderTransfer {
        target,
        item_count,
        encoding,
        ..
    } = folder;
    let TransferTarget { dir, name, .. } = target;
    let requester = target.requester();
    let root = folder_root(dir, name);
    write_folder_action(stream, FolderAction::NextFile).await?;
    for remaining in (0..*item_count).rev() {
        let body = read_u16_prefixed(stream).await?;
        let received = match FolderItemHeader::decode_body(&body, encoding) {
            Ok(header) => match split_item_path(&root, &header.path) {
                Some((dir, name)) => {
                    receive_folder_item(stream, requester, &dir, name, header.is_folder, state)
                        .await?
                }
                None => false,
            },
            Err(_) => false,
        };
        if !received || remaining > 0 {
            write_folder_action(stream, FolderAction::NextFile).await?;
        }
    }
    Ok(())
}

/// Receive one folder upload item. Asks for a file's data with
/// `SendFile`, or `ResumeFile` for an unfinished upload, unless
/// [`SharedState::prepare_folder_upload_item`] skips the item. `true`
/// once a file's data is received.
async fn receive_folder_item(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    requester: FileRequester<'_>,
    dir: &HlPath,
    name: &str,
    is_folder: bool,
    state: &SharedState,
) -> Result<bool> {
    let Some(resume_offset) = state
        .prepare_folder_upload_item(requester, dir, name, is_folder)
        .await
    else {
        return Ok(false);
    };
    let opened = if resume_offset > 0 {
        state
            .resume_file_for_write(requester, dir, name, resume_offset)
            .await
    } else {
        state.create_file_for_write(requester, dir, name).await
    };
    let Ok(mut out) = opened else {
        return Ok(false);
    };
    if resume_offset > 0 {
        let resume_data = types::encode_resume_data(resume_offset as u32);
        write_folder_action(stream, FolderAction::ResumeFile).await?;
        stream
            .write_all(&(resume_data.len() as u16).to_be_bytes())
            .await?;
        stream.write_all(&resume_data).await?;
    } else {
        write_folder_action(stream, FolderAction::SendFile).await?;
    }
    let mut flat_size = [0u8; 4];
    stream.read_exact(&mut flat_size).await?;
    receive_flat_file(stream, &mut out).await?;
    out.shutdown().await?;
    state.finish_upload(requester, dir, name).await?;
    Ok(true)
}

/// A folder transfer's root folder path: `dir` plus `name`.
fn folder_root(dir: &HlPath, name: &str) -> Vec<String> {
    let mut root = dir.components.clone();
    root.push(name.to_string());
    root
}

/// Split an item's `path` under `root` into its directory and name.
/// `None` for an empty path.
fn split_item_path<'a>(root: &[String], path: &'a HlPath) -> Option<(HlPath, &'a str)> {
    let (name, parents) = path.components.split_last()?;
    Some((HlPath::new(root.iter().chain(parents)), name))
}

/// The next [`FolderAction`], or `None` once the client closes.
async fn read_folder_action(stream: &mut (impl AsyncRead + Unpin)) -> Result<Option<FolderAction>> {
    let mut buf = [0u8; 2];
    match stream.read_exact(&mut buf).await {
        Ok(_) => Ok(Some(FolderAction::from(u16::from_be_bytes(buf)))),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Send one [`FolderAction`].
async fn write_folder_action(
    stream: &mut (impl AsyncWrite + Unpin),
    action: FolderAction,
) -> Result<()> {
    stream.write_all(&u16::from(action).to_be_bytes()).await?;
    Ok(())
}

/// A `ResumeFile` action's `size(2) | resume data`, as the byte count the
/// client already has.
async fn read_resume_data(stream: &mut (impl AsyncRead + Unpin)) -> Result<u32> {
    let buf = read_u16_prefixed(stream).await?;
    Ok(types::decode_resume_data(&buf)?)
}

/// Read a `len(2)` prefix, then that many bytes.
async fn read_u16_prefixed(stream: &mut (impl AsyncRead + Unpin)) -> Result<Vec<u8>> {
    let mut len = [0u8; 2];
    stream.read_exact(&mut len).await?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::account::InMemoryAccountProvider;
    use crate::ban::InMemoryBanProvider;
    use crate::banner::NoBanner;
    use crate::file::{FileRequester, InMemoryFileProvider};
    use crate::flood::NoFloodPolicy;
    use crate::news::InMemoryNewsProvider;
    use crate::providers::Providers;
    use hotline_proto::consts::Access;
    use hotline_proto::htxf::HtxfType;
    use hotline_proto::text_encoding;
    use hotline_proto::types::HlPath;
    use tokio::io::AsyncWriteExt;
    use tokio::task::JoinHandle;

    fn state_with_files(files: InMemoryFileProvider) -> SharedState {
        SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(files),
            flat_news: Arc::new(InMemoryNewsProvider::default()),
            threaded_news: Arc::new(InMemoryNewsProvider::default()),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
            activity: Arc::new(crate::activity::NoActivityLog),
        })
    }

    /// `tokio::io::copy` returns early on a closed connection the same
    /// way it would at a clean EOF, so a cancelled upload must not be
    /// renamed to its real name.
    #[tokio::test]
    async fn a_cancelled_upload_is_not_renamed_off_its_hpf_name() {
        let state = state_with_files(InMemoryFileProvider::default());
        let requester = FileRequester { login: "alice" };
        let file = FileTransfer {
            target: TransferTarget {
                login: "alice".into(),
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
        };
        let transfer = PendingTransfer {
            kind: TransferKind::File(file.clone()),
            direction: TransferDirection::Put,
            size: 11,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let full = htxf::encode_flat_file(b"hello world");
        client.write_all(&full[..full.len() - 1]).await.unwrap();
        drop(client);

        let result = receive_file(&mut server_stream, &file, &transfer, &state).await;
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
        let file = FileTransfer {
            target: TransferTarget {
                login: "alice".into(),
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
        };
        let transfer = PendingTransfer {
            kind: TransferKind::File(file.clone()),
            direction: TransferDirection::Put,
            size: 11,
            resume_offset: 6,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();

        let resumed = htxf::encode_flat_file(b"there");
        client.write_all(&resumed).await.unwrap();
        drop(client);

        receive_file(&mut server_stream, &file, &transfer, &state)
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

    /// A wrong-address connection still frees the download's slot.
    #[tokio::test]
    async fn htxf_connection_from_the_wrong_address_releases_the_download_slot() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        state.set_download_limits(1, 10);

        let held = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(FileTransfer {
                    target: TransferTarget {
                        login: "alice".into(),
                        dir: HlPath::default(),
                        name: "a.txt".into(),
                    },
                    raw: true,
                }),
                direction: TransferDirection::Get,
                size: 5,
                resume_offset: 0,
                client_ip: "10.0.0.1".parse().unwrap(),
                user_id: 1,
            })
            .await;
        assert_eq!(state.admit_download(held, 1).await, 0);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(addr).await.unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();

        let header = hotline_proto::htxf::HtxfHeader {
            reference: held.0,
            len: 0,
            transfer_type: hotline_proto::htxf::HtxfType::File,
        };
        client.write_all(&header.encode()).await.unwrap();

        handle_htxf_connection(server_stream, peer, &state)
            .await
            .unwrap();

        // `held`'s slot is free, so a second download starts active.
        let second = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(FileTransfer {
                    target: TransferTarget {
                        login: "bob".into(),
                        dir: HlPath::default(),
                        name: "a.txt".into(),
                    },
                    raw: true,
                }),
                direction: TransferDirection::Get,
                size: 5,
                resume_offset: 0,
                client_ip: "127.0.0.1".parse().unwrap(),
                user_id: 2,
            })
            .await;
        assert_eq!(state.admit_download(second, 2).await, 0);
    }

    /// A transfer of `a.txt` in the root folder, from loopback.
    fn a_txt_transfer(direction: TransferDirection, user_id: u16) -> PendingTransfer {
        PendingTransfer {
            kind: TransferKind::File(FileTransfer {
                target: TransferTarget {
                    login: "alice".into(),
                    dir: HlPath::default(),
                    name: "a.txt".into(),
                },
                raw: direction == TransferDirection::Get,
            }),
            direction,
            size: 11,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id,
        }
    }

    /// Connect over loopback and send `reference`'s `HTXF` header. Returns
    /// the client, the server's side, and the client's address.
    async fn connect_htxf(reference: TransferRef) -> (TcpStream, TcpStream, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let header = HtxfHeader {
            reference: reference.0,
            len: 0,
            transfer_type: HtxfType::File,
        };
        client.write_all(&header.encode()).await.unwrap();
        (client, server_stream, peer)
    }

    #[tokio::test]
    async fn an_upload_stops_when_its_session_ends() {
        let state = state_with_files(InMemoryFileProvider::default());
        let session = state.register_recipient(1).await;
        let reference = state
            .register_transfer(a_txt_transfer(TransferDirection::Put, 1))
            .await;
        let (mut client, server_stream, peer) = connect_htxf(reference).await;
        let full = htxf::encode_flat_file(b"hello world");
        client.write_all(&full[..full.len() - 1]).await.unwrap();

        drop(session);
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_htxf_connection(server_stream, peer, &state),
        )
        .await;

        assert!(result.expect("the upload should stop").is_ok());
        let requester = FileRequester { login: "alice" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
        drop(client);
    }

    #[tokio::test]
    async fn an_upload_whose_session_has_ended_is_refused() {
        let state = state_with_files(InMemoryFileProvider::default());
        let reference = state
            .register_transfer(a_txt_transfer(TransferDirection::Put, 1))
            .await;
        let (mut client, server_stream, peer) = connect_htxf(reference).await;
        client
            .write_all(&htxf::encode_flat_file(b"hello world"))
            .await
            .unwrap();

        handle_htxf_connection(server_stream, peer, &state)
            .await
            .unwrap();

        let requester = FileRequester { login: "alice" };
        let root = HlPath::default();
        assert!(!state.file_exists(requester, &root, "a.txt").await);
        assert!(!state.file_exists(requester, &root, "a.txt.hpf").await);
    }

    #[tokio::test]
    async fn a_queued_download_leaves_the_queue_when_its_client_closes() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        let state = state_with_files(files);
        state.set_download_limits(1, 10);
        let held = state
            .register_transfer(a_txt_transfer(TransferDirection::Get, 1))
            .await;
        assert_eq!(state.admit_download(held, 1).await, 0);
        let queued = state
            .register_transfer(a_txt_transfer(TransferDirection::Get, 2))
            .await;
        assert_eq!(state.admit_download(queued, 2).await, 1);
        let (client, server_stream, peer) = connect_htxf(queued).await;

        drop(client);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_htxf_connection(server_stream, peer, &state),
        )
        .await
        .expect("the wait should end")
        .unwrap();

        // Only `held` is ahead of a new download now.
        let third = state
            .register_transfer(a_txt_transfer(TransferDirection::Get, 3))
            .await;
        assert_eq!(state.admit_download(third, 3).await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_connection_that_sends_no_header_is_closed() {
        let state = state_with_files(InMemoryFileProvider::default());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();

        tokio::time::timeout(
            HEADER_TIMEOUT * 2,
            handle_htxf_connection(server_stream, peer, &state),
        )
        .await
        .expect("the header wait should time out")
        .unwrap();
        drop(client);
    }

    /// Register `folder`, connect to it over loopback, and send the
    /// `HTXF` header, with the server side running in a spawned task.
    async fn start_folder_transfer(
        state: &SharedState,
        folder: FolderTransfer,
        direction: TransferDirection,
    ) -> (TcpStream, JoinHandle<()>) {
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::Folder(folder),
                direction,
                size: 0,
                resume_offset: 0,
                client_ip: "127.0.0.1".parse().unwrap(),
                user_id: 1,
            })
            .await;
        if direction == TransferDirection::Get {
            state.admit_download(reference, 1).await;
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        // User 1's session stays open for the whole transfer.
        let session = state.register_recipient(1).await;
        let state = state.clone();
        let server = tokio::spawn(async move {
            handle_htxf_connection(server_stream, peer, &state)
                .await
                .unwrap();
            drop(session);
        });
        let header = HtxfHeader {
            reference: reference.0,
            len: 0,
            transfer_type: HtxfType::Folder,
        };
        client.write_all(&header.encode()).await.unwrap();
        (client, server)
    }

    fn folder_transfer(
        name: &str,
        items: Vec<crate::file::FolderItem>,
        item_count: u32,
    ) -> FolderTransfer {
        FolderTransfer {
            target: TransferTarget {
                login: "alice".into(),
                dir: HlPath::default(),
                name: name.into(),
            },
            items,
            item_count,
            encoding: text_encoding::DEFAULT,
        }
    }

    async fn send_action(client: &mut TcpStream, action: FolderAction) {
        client
            .write_all(&u16::from(action).to_be_bytes())
            .await
            .unwrap();
    }

    async fn read_action(client: &mut TcpStream) -> FolderAction {
        let mut buf = [0u8; 2];
        client.read_exact(&mut buf).await.unwrap();
        FolderAction::from(u16::from_be_bytes(buf))
    }

    async fn read_item_header(client: &mut TcpStream) -> FolderItemHeader {
        let body = read_u16_prefixed(client).await.unwrap();
        FolderItemHeader::decode_body(&body, text_encoding::DEFAULT).unwrap()
    }

    /// A file's `size(4)` then its flattened object, returning the object's
    /// `DATA` fork bytes.
    async fn read_sized_flat_file(client: &mut TcpStream) -> Vec<u8> {
        let mut size = [0u8; 4];
        client.read_exact(&mut size).await.unwrap();
        let mut flat = vec![0u8; u32::from_be_bytes(size) as usize];
        client.read_exact(&mut flat).await.unwrap();
        flat.split_off(htxf::FLAT_FILE_HEADER_LEN)
    }

    async fn send_item_header(client: &mut TcpStream, is_folder: bool, path: &[&str]) {
        let header = FolderItemHeader {
            is_folder,
            path: HlPath::new(path.iter().copied()),
        };
        client
            .write_all(&header.encode(text_encoding::DEFAULT).unwrap())
            .await
            .unwrap();
    }

    async fn send_sized_flat_file(client: &mut TcpStream, data: &[u8]) {
        let flat = htxf::encode_flat_file(data);
        client
            .write_all(&(flat.len() as u32).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&flat).await.unwrap();
    }

    async fn read_file(state: &SharedState, dir: &[&str], name: &str) -> Vec<u8> {
        let requester = FileRequester { login: "alice" };
        let mut reader = state
            .open_file_for_read(requester, &HlPath::new(dir.iter().copied()), name)
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        buf
    }

    #[tokio::test]
    async fn folder_download_steps_through_each_item_then_closes() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top", "Sub"]), "a.txt", *b"hello");
        files.seed(&HlPath::new(["Top"]), "b.txt", *b"hi");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let items = state
            .walk_folder(requester, &HlPath::default(), "Top", Access::all())
            .await
            .unwrap();
        let folder = folder_transfer("Top", items, 3);
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Get).await;

        send_action(&mut client, FolderAction::NextFile).await;
        let header = read_item_header(&mut client).await;
        assert!(header.is_folder);
        assert_eq!(header.path, HlPath::new(["Sub"]));

        send_action(&mut client, FolderAction::NextFile).await;
        let header = read_item_header(&mut client).await;
        assert!(!header.is_folder);
        assert_eq!(header.path, HlPath::new(["Sub", "a.txt"]));
        send_action(&mut client, FolderAction::SendFile).await;
        assert_eq!(read_sized_flat_file(&mut client).await, b"hello");

        send_action(&mut client, FolderAction::NextFile).await;
        let header = read_item_header(&mut client).await;
        assert_eq!(header.path, HlPath::new(["b.txt"]));
        // Skip b.txt's data; the items then run out.
        send_action(&mut client, FolderAction::NextFile).await;

        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn folder_download_resumes_a_file_from_its_resume_data() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let items = state
            .walk_folder(requester, &HlPath::default(), "Top", Access::all())
            .await
            .unwrap();
        let folder = folder_transfer("Top", items, 1);
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Get).await;

        send_action(&mut client, FolderAction::NextFile).await;
        read_item_header(&mut client).await;
        let resume_data = types::encode_resume_data(2);
        send_action(&mut client, FolderAction::ResumeFile).await;
        client
            .write_all(&(resume_data.len() as u16).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&resume_data).await.unwrap();
        assert_eq!(read_sized_flat_file(&mut client).await, b"llo");

        drop(client);
        server.await.unwrap();
    }

    /// Every item counts toward the declared total, a skipped one too.
    /// A skipped last item still gets a `NextFile` after it.
    #[tokio::test]
    async fn folder_upload_creates_items_skips_existing_files_and_ends_at_the_item_count() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "exists.txt", *b"old");
        let state = state_with_files(files);
        let folder = folder_transfer("Top", Vec::new(), 3);
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Put).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, true, &["Sub"]).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["Sub", "a.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::SendFile);
        send_sized_flat_file(&mut client, b"hello").await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["exists.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);

        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        server.await.unwrap();

        assert_eq!(read_file(&state, &["Top", "Sub"], "a.txt").await, b"hello");
        assert_eq!(read_file(&state, &["Top"], "exists.txt").await, b"old");
    }

    #[tokio::test]
    async fn folder_upload_resumes_an_unfinished_file() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt.hpf", *b"hello ");
        let state = state_with_files(files);
        let folder = folder_transfer("Top", Vec::new(), 1);
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Put).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["a.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::ResumeFile);
        assert_eq!(read_resume_data(&mut client).await.unwrap(), 6);
        send_sized_flat_file(&mut client, b"world").await;

        server.await.unwrap();
        assert_eq!(read_file(&state, &["Top"], "a.txt").await, b"hello world");
    }

    /// A path through an existing file skips the item rather than
    /// creating anything under it. A received last file gets no
    /// `NextFile` after it.
    #[tokio::test]
    async fn folder_upload_skips_an_item_whose_parent_is_a_file() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "f", *b"x");
        let state = state_with_files(files);
        let folder = folder_transfer("Top", Vec::new(), 2);
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Put).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["f", "a.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["b.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::SendFile);
        send_sized_flat_file(&mut client, b"b").await;

        let mut rest = Vec::new();
        client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        server.await.unwrap();
        assert_eq!(read_file(&state, &["Top"], "b.txt").await, b"b");
    }
}
