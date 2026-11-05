use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use hotline_proto::htxf::{
    self, FlatFileInfo, FolderAction, FolderItemHeader, ForkHeader, HtxfHeader, ResumedFork,
};
use hotline_proto::text_encoding::Encoding;
use hotline_proto::types::{HlPath, ResumeData};

use crate::error::Result;
use crate::file::{FileProvider, FileRequester};
use crate::server::counting_stream::CountingStream;
use crate::server::idle_stream::IdleStream;
use crate::state::{
    FileTransfer, FolderTransfer, PendingTransfer, ProgressGuard, SharedState, TransferDirection,
    TransferKind, TransferRef, TransferTarget,
};

/// Time a new `HTXF` connection may take to send its header.
const HEADER_TIMEOUT: Duration = Duration::from_secs(60);

/// Time an upload may go without receiving a byte, openline's 120 s.
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Accept `HTXF` data connections forever, spawning one task per
/// connection. Each connection completes exactly one pending transfer,
/// matched by the reference number in its handshake header.
pub(crate) async fn run_htxf_listener(listener: TcpListener, state: Arc<SharedState>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(x) => x,
            Err(e) => {
                tracing::warn!(error = %e, "htxf: accept failed");
                continue;
            }
        };
        if state.is_banned(peer.ip()).await.is_some() {
            tracing::debug!(%peer, "htxf: rejected connection from banned address");
            continue;
        }
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_htxf_connection(stream, peer, &state).await {
                tracing::debug!(%peer, error = %e, "htxf: connection ended with error");
            }
        });
    }
}

/// Complete one `HTXF` data connection: match its handshake to a pending
/// banner, file, or folder transfer, then run it.
async fn handle_htxf_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    state: &Arc<SharedState>,
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
    let Some(transfer) = state
        .take_htxf_transfer(reference, header.transfer_type)
        .await
    else {
        tracing::debug!(%peer, reference = header.reference, transfer_type = ?header.transfer_type, "htxf: no pending transfer of this type for reference");
        return Ok(());
    };
    if transfer.client_ip != peer.ip() {
        tracing::debug!(%peer, expected = %transfer.client_ip, "htxf: connection from wrong address for reference");
        state.release_download_slot(reference).await;
        return Ok(());
    }

    let progress = state.start_progress(reference, &transfer);
    if transfer.direction == TransferDirection::Get
        && !matches!(transfer.kind, TransferKind::Banner)
        && !wait_for_slot_or_close(&stream, state, reference).await
    {
        return Ok(());
    }
    let count = progress
        .as_ref()
        .map_or_else(Arc::default, ProgressGuard::counter);
    let mut stream = CountingStream::new(&mut stream, count);
    match &transfer.kind {
        TransferKind::Banner => {
            let send = send_banner(&mut stream, state);
            run_until_session_ends(state, transfer.user_id, send).await
        }
        TransferKind::File(file) => match transfer.direction {
            TransferDirection::Get => {
                let send = send_file(&mut stream, file, &transfer);
                run_download(state, reference, send).await
            }
            TransferDirection::Put => {
                let mut stream = IdleStream::new(&mut stream, UPLOAD_IDLE_TIMEOUT);
                let receive = receive_file(&mut stream, file, &transfer, state);
                run_until_session_ends(state, transfer.user_id, receive).await
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
                run_until_session_ends(state, transfer.user_id, receive).await
            }
        },
    }
}

/// `false` if download `reference` leaves the queue or `stream` closes
/// before it holds a slot.
async fn wait_for_slot_or_close(
    stream: &TcpStream,
    state: &Arc<SharedState>,
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

/// Run `transfer` until it ends or `user_id`'s session ends.
async fn run_until_session_ends(
    state: &SharedState,
    user_id: u16,
    transfer: impl Future<Output = Result<()>>,
) -> Result<()> {
    let Some(session_ended) = state.session_ended(user_id).await else {
        return Ok(());
    };
    tokio::select! {
        result = transfer => result,
        () = session_ended => Ok(()),
    }
}

/// Run `send` for download `reference`, which holds a slot, then free
/// the slot. A [`myTran_KillDownload`] (214) stops it early.
///
/// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
async fn run_download(
    state: &Arc<SharedState>,
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
async fn send_banner(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    state: &SharedState,
) -> Result<()> {
    let mut reader = state.open_banner_read().await?;
    tokio::io::copy(&mut reader, stream).await?;
    drain_stream(stream).await
}

/// Read (discarding) until the client closes, instead of closing our
/// side first. A client may still be draining its last buffered chunk
/// when a server-initiated close arrives, and reports that as a failed
/// transfer even after reading every byte.
async fn drain_stream(stream: &mut (impl AsyncRead + Unpin)) -> Result<()> {
    let mut discard = [0u8; 4096];
    while stream.read(&mut discard).await? != 0 {}
    Ok(())
}

/// Send a [`myTran_DownloadFile`] (202) download: the flattened-file-object
/// header with `file.info`, the file's raw bytes, then its resource fork.
/// `file.raw` sends the raw bytes alone, with no header, like
/// [`send_banner`]. `transfer.resume_offset` and `file.resource_offset` skip
/// that many leading bytes of each fork, for a client resuming an
/// interrupted download.
///
/// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
async fn send_file(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    file: &FileTransfer,
    transfer: &PendingTransfer,
) -> Result<()> {
    let FileTransfer {
        target,
        raw,
        info,
        resource_size,
        resource_offset,
        ..
    } = file;
    let TransferTarget { dir, name, .. } = target;
    let store = &*target.store;
    let requester = target.requester();
    let mut reader = store.open_read(requester, dir, name).await?;
    if *raw {
        tokio::io::copy(&mut reader, stream).await?;
    } else {
        let data = ResumedFork {
            offset: transfer.resume_offset,
            len: transfer.size - transfer.resume_offset,
        };
        let resource = ResumedFork {
            offset: *resource_offset,
            len: resource_size - resource_offset,
        };
        skip_bytes(&mut reader, data.offset).await?;
        let resource_reader = store
            .open_resource_read(requester, dir, name, resource.offset)
            .await?;
        send_flat_file(
            stream,
            info,
            (reader, data.len),
            (resource_reader, resource.len),
        )
        .await?;
    }
    drain_stream(stream).await
}

/// Write a flattened file object: `info` as its `INFO` fork data, then
/// `len` bytes of each fork from its reader. A fork shorter than its
/// `len` is an error.
async fn send_flat_file(
    stream: &mut (impl AsyncWrite + Unpin),
    info: &[u8],
    (mut data, data_len): (impl AsyncRead + Unpin, u64),
    (mut resource, resource_len): (impl AsyncRead + Unpin, u64),
) -> Result<()> {
    stream
        .write_all(&htxf::flat_file_header_with_resource(info, data_len as u32))
        .await?;
    copy_fork(&mut data, data_len, stream).await?;
    let resource_header = ForkHeader {
        fork_type: *b"MACR",
        data_size: resource_len as u32,
    };
    stream.write_all(&resource_header.encode()).await?;
    copy_fork(&mut resource, resource_len, stream).await
}

/// Discard the next `n` bytes of `reader`.
async fn skip_bytes(reader: &mut (impl AsyncRead + Unpin), n: u64) -> Result<()> {
    copy_fork(reader, n, &mut tokio::io::sink()).await
}

/// Receive a [`myTran_UploadFile`] (203) upload: parse the client's
/// flattened-file-object framing with [`receive_flat_file`]. A resumed
/// upload writes each fork from its resume offset instead of creating the
/// file fresh. Renames the upload to its real name (see
/// [`FileProvider::finish_upload`]) on success, then stores its `INFO` fork
/// (see [`SharedState::apply_info_fork`]); an error leaves it under its
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
    let store = &*file.target.store;
    let requester = file.target.requester();
    let mut out = if transfer.resume_offset > 0 || file.resource_offset > 0 {
        store
            .open_write_resume(requester, dir, name, transfer.resume_offset)
            .await?
    } else {
        store.create_write(requester, dir, name).await?
    };
    let resource = ResourceSink {
        store,
        requester,
        dir,
        name,
        at: file.resource_offset,
    };
    let info = receive_flat_file(stream, &mut out, file.encoding, resource).await?;
    out.shutdown().await?;
    store.finish_upload(requester, dir, name).await?;
    if let Some(info) = info {
        state
            .apply_info_fork(store, requester, dir, name, &info)
            .await;
    }
    Ok(())
}

/// Where an upload's `MACR` fork goes: the resource fork of `name`'s
/// in-progress upload, from `at` onward.
struct ResourceSink<'a> {
    store: &'a dyn FileProvider,
    requester: FileRequester<'a>,
    dir: &'a HlPath,
    name: &'a str,
    at: u64,
}

/// Read one flattened file object. Writes its `DATA` fork to `out` and its
/// `MACR` fork to `resource`, and discards any other fork. Returns its
/// `INFO` fork, if it has one.
async fn receive_flat_file(
    stream: &mut (impl AsyncRead + Unpin),
    out: &mut (impl AsyncWrite + Unpin),
    encoding: &'static Encoding,
    resource: ResourceSink<'_>,
) -> Result<Option<FlatFileInfo>> {
    let mut prefix = [0u8; htxf::FLAT_FILE_PREFIX_LEN];
    stream.read_exact(&mut prefix).await?;
    let fork_count = htxf::decode_flat_file_prefix(&prefix)?;

    let mut info = None;
    for _ in 0..fork_count {
        let mut fork_header_buf = [0u8; ForkHeader::LEN];
        stream.read_exact(&mut fork_header_buf).await?;
        let fork_header = ForkHeader::decode(&fork_header_buf)?;

        let mut remaining = fork_header.data_size as u64;
        if &fork_header.fork_type == b"DATA" {
            copy_fork(stream, remaining, out).await?;
        } else if &fork_header.fork_type == b"MACR" {
            let ResourceSink {
                store,
                requester,
                dir,
                name,
                at,
            } = resource;
            let mut writer = store.open_resource_write(requester, dir, name, at).await?;
            copy_fork(stream, remaining, &mut writer).await?;
            writer.shutdown().await?;
        } else {
            if &fork_header.fork_type == b"INFO" {
                let mut data = vec![0u8; remaining.min(FlatFileInfo::MAX_LEN as u64) as usize];
                stream.read_exact(&mut data).await?;
                remaining -= data.len() as u64;
                info = Some(FlatFileInfo::decode(&data, encoding));
            }
            skip_bytes(stream, remaining).await?;
        }
    }
    Ok(info)
}

/// Copy exactly `len` bytes of `stream` to `out`.
async fn copy_fork(
    stream: &mut (impl AsyncRead + Unpin),
    len: u64,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<()> {
    let copied = tokio::io::copy(&mut stream.take(len), out).await?;
    if copied != len {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
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
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
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
    let store = &*target.store;
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
                let resume = if action == FolderAction::ResumeFile {
                    read_resume_data(stream).await?
                } else {
                    ResumeData::default()
                };
                let (dir, name) = split_item_path(&root, &item.path)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                let mut reader = store.open_read(requester, &dir, name).await?;
                let info = state
                    .info_fork(store, requester, &dir, name, encoding)
                    .await;
                let resource_size = store.resource_fork_size(requester, &dir, name).await;
                let data = ResumedFork::new(item.size, resume.data.into());
                let resource = ResumedFork::new(resource_size, resume.resource.into());
                skip_bytes(&mut reader, data.offset).await?;
                let resource_reader = store
                    .open_resource_read(requester, &dir, name, resource.offset)
                    .await?;
                let header_len = (htxf::flat_file_header_len(info.len()) + ForkHeader::LEN) as u64;
                let flat_size = header_len + data.len + resource.len;
                stream.write_all(&(flat_size as u32).to_be_bytes()).await?;
                send_flat_file(
                    stream,
                    &info,
                    (reader, data.len),
                    (resource_reader, resource.len),
                )
                .await?;
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
    let store = &*target.store;
    let requester = target.requester();
    let root = folder_root(dir, name);
    write_folder_action(stream, FolderAction::NextFile).await?;
    for remaining in (0..*item_count).rev() {
        let body = read_u16_prefixed(stream).await?;
        let received = match FolderItemHeader::decode_body(&body, encoding) {
            Ok(header) => match stored_item_path(&root, &header.path, store) {
                Some((dir, name)) => {
                    let is_folder = header.is_folder;
                    receive_folder_item(
                        stream, store, requester, &dir, &name, is_folder, encoding, state,
                    )
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
#[allow(clippy::too_many_arguments)]
async fn receive_folder_item(
    stream: &mut (impl AsyncRead + AsyncWrite + Unpin),
    store: &dyn FileProvider,
    requester: FileRequester<'_>,
    dir: &HlPath,
    name: &str,
    is_folder: bool,
    encoding: &'static Encoding,
    state: &SharedState,
) -> Result<bool> {
    let Some(resume_offset) = state
        .prepare_folder_upload_item(store, requester, dir, name, is_folder)
        .await
    else {
        return Ok(false);
    };
    let resource_offset = state
        .partial_resource_fork_size(store, requester, dir, name)
        .await;
    let resume = resume_offset > 0 || resource_offset > 0;
    let opened = if resume {
        store
            .open_write_resume(requester, dir, name, resume_offset)
            .await
    } else {
        store.create_write(requester, dir, name).await
    };
    let Ok(mut out) = opened else {
        return Ok(false);
    };
    if resume {
        let resume_data = ResumeData {
            data: resume_offset as u32,
            resource: resource_offset as u32,
        }
        .encode();
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
    let resource = ResourceSink {
        store,
        requester,
        dir,
        name,
        at: resource_offset,
    };
    let info = receive_flat_file(stream, &mut out, encoding, resource).await?;
    out.shutdown().await?;
    store.finish_upload(requester, dir, name).await?;
    if let Some(info) = info {
        state
            .apply_info_fork(store, requester, dir, name, &info)
            .await;
    }
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

/// Like [`split_item_path`] for an item uploaded into `store`, with
/// each name as [`FileProvider::stored_name`] stores it. `None` if a
/// name is invalid.
fn stored_item_path(
    root: &[String],
    path: &HlPath,
    store: &dyn FileProvider,
) -> Option<(HlPath, String)> {
    let mut names = path
        .components
        .iter()
        .map(|name| store.stored_name(name))
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    let name = names.pop()?;
    Some((HlPath::new(root.iter().chain(&names)), name))
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

/// A `ResumeFile` action's `size(2) | resume data`.
async fn read_resume_data(stream: &mut (impl AsyncRead + Unpin)) -> Result<ResumeData> {
    let buf = read_u16_prefixed(stream).await?;
    Ok(ResumeData::decode(&buf)?)
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
    use crate::file::{FileRequester, InMemoryFileProvider};
    use crate::providers::Providers;
    use hotline_proto::consts::Access;
    use hotline_proto::htxf::HtxfType;
    use hotline_proto::text_encoding;
    use hotline_proto::types::HlPath;
    use tokio::io::AsyncWriteExt;
    use tokio::task::JoinHandle;

    #[tokio::test]
    async fn htxf_listener_closes_a_connection_from_a_banned_address() {
        use tokio::io::AsyncReadExt;

        let state = Arc::<SharedState>::default();
        state
            .ban(crate::BanRecord::new("127.0.0.1".parse().unwrap(), None))
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(run_htxf_listener(listener, state));

        let mut client = TcpStream::connect(addr).await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut buf),
        )
        .await
        .expect("the server should close the socket");
        assert_eq!(read.unwrap(), 0);
    }

    #[tokio::test]
    async fn stored_item_path_validates_every_name() {
        let root = ["Uploads".to_string()];
        let state = state_with_files(InMemoryFileProvider::default());
        let store = shared_files(&state, "guest").await;
        let path = HlPath::new(["..", "a/b"]);
        let (dir, name) = stored_item_path(&root, &path, &*store).unwrap();
        assert_eq!(dir, HlPath::new(["Uploads", "--"]));
        assert_eq!(name, "a-b");
        let path = HlPath::new(["ok", ""]);
        assert!(stored_item_path(&root, &path, &*store).is_none());
    }

    fn state_with_files(files: InMemoryFileProvider) -> Arc<SharedState> {
        SharedState::new(Providers {
            files: Arc::new(files),
            ..Providers::default()
        })
    }

    /// The files store that `state` gives `login`.
    async fn shared_files(state: &SharedState, login: &str) -> Arc<dyn FileProvider> {
        state.files_for(login).await.unwrap()
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
                store: shared_files(&state, "alice").await,
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
            encoding: text_encoding::DEFAULT,
            info: Vec::new(),
            resource_size: 0,
            resource_offset: 0,
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
                .file_size(
                    &*file.target.store,
                    requester,
                    &HlPath::default(),
                    "a.txt.hpf",
                    Access::all()
                )
                .await,
            Ok(10)
        );
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    /// Own files for each login: `first` at the first lookup, and `later`
    /// after it, as when an account moves its folder.
    struct MovingStores {
        first: Arc<InMemoryFileProvider>,
        later: Arc<InMemoryFileProvider>,
        looked_up: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl crate::AccountStores for MovingStores {
        async fn files(
            &self,
            _login: &str,
        ) -> std::result::Result<Option<Arc<dyn FileProvider>>, &'static str> {
            let looked_up = self
                .looked_up
                .swap(true, std::sync::atomic::Ordering::SeqCst);
            let store = if looked_up { &self.later } else { &self.first };
            Ok(Some(store.clone()))
        }

        async fn threaded_news(
            &self,
            _login: &str,
        ) -> std::result::Result<Option<Arc<dyn crate::ThreadedNewsProvider>>, &'static str>
        {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn an_upload_finishes_in_the_store_of_its_request() {
        let first = Arc::new(InMemoryFileProvider::default());
        let later = Arc::new(InMemoryFileProvider::default());
        let state = SharedState::new(Providers {
            account_stores: Arc::new(MovingStores {
                first: first.clone(),
                later: later.clone(),
                looked_up: Default::default(),
            }),
            ..Providers::default()
        });
        let file = FileTransfer {
            target: TransferTarget {
                login: "alice".into(),
                store: shared_files(&state, "alice").await,
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
            encoding: text_encoding::DEFAULT,
            info: Vec::new(),
            resource_size: 0,
            resource_offset: 0,
        };
        let transfer = PendingTransfer {
            kind: TransferKind::File(file.clone()),
            direction: TransferDirection::Put,
            size: 5,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();
        client
            .write_all(&htxf::encode_flat_file(b"hello"))
            .await
            .unwrap();
        drop(client);
        receive_file(&mut server_stream, &file, &transfer, &state)
            .await
            .unwrap();

        let requester = FileRequester { login: "alice" };
        let root = HlPath::default();
        assert_eq!(first.size(requester, &root, "a.txt").await, Some(5));
        assert_eq!(later.list(requester, &root).await.unwrap_or_default(), []);
    }

    #[tokio::test]
    async fn an_upload_stores_its_info_fork() {
        let state = state_with_files(InMemoryFileProvider::default());
        let requester = FileRequester { login: "alice" };
        let file = FileTransfer {
            target: TransferTarget {
                login: "alice".into(),
                store: shared_files(&state, "alice").await,
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
            encoding: text_encoding::DEFAULT,
            info: Vec::new(),
            resource_size: 0,
            resource_offset: 0,
        };
        let transfer = PendingTransfer {
            kind: TransferKind::File(file.clone()),
            direction: TransferDirection::Put,
            size: 5,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };
        let info = FlatFileInfo {
            file_type: *b"TEXT",
            creator: *b"R*ch",
            platform_flags: 0x8501,
            comment: "a note".into(),
            ..FlatFileInfo::default()
        };
        let mut flat = htxf::flat_file_header(&info.encode(text_encoding::DEFAULT), 5);
        flat.extend_from_slice(b"hello");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();
        client.write_all(&flat).await.unwrap();
        drop(client);
        receive_file(&mut server_stream, &file, &transfer, &state)
            .await
            .unwrap();

        let root = HlPath::default();
        let stored = state
            .info_fork(
                &*file.target.store,
                requester,
                &root,
                "a.txt",
                text_encoding::DEFAULT,
            )
            .await;
        let stored = FlatFileInfo::decode(&stored, text_encoding::DEFAULT);
        assert_eq!(&stored.file_type, b"TEXT");
        assert_eq!(&stored.creator, b"R*ch");
        assert_eq!(stored.platform_flags, 0x0401);
        assert_eq!(stored.comment, "a note");
    }

    #[tokio::test]
    async fn an_upload_stores_its_resource_fork() {
        let state = state_with_files(InMemoryFileProvider::default());
        let file = FileTransfer {
            target: TransferTarget {
                login: "alice".into(),
                store: shared_files(&state, "alice").await,
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
            encoding: text_encoding::DEFAULT,
            info: Vec::new(),
            resource_size: 0,
            resource_offset: 0,
        };
        let transfer = PendingTransfer {
            kind: TransferKind::File(file.clone()),
            direction: TransferDirection::Put,
            size: 5,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };
        let flat = flat_file_with_resource(b"hello", b"rsrc");

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (mut server_stream, _) = listener.accept().await.unwrap();
        client.write_all(&flat).await.unwrap();
        drop(client);
        receive_file(&mut server_stream, &file, &transfer, &state)
            .await
            .unwrap();

        assert_eq!(read_file(&state, &[], "a.txt").await, b"hello");
        assert_eq!(read_resource_fork(&state, &[], "a.txt").await, b"rsrc");
    }

    /// `open_write_resume` seeks to `resume_offset` before the
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
                store: shared_files(&state, "alice").await,
                dir: HlPath::default(),
                name: "a.txt".into(),
            },
            raw: false,
            encoding: text_encoding::DEFAULT,
            info: Vec::new(),
            resource_size: 0,
            resource_offset: 0,
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
                .file_size(
                    &*file.target.store,
                    requester,
                    &HlPath::default(),
                    "a.txt",
                    Access::all()
                )
                .await,
            Ok(11)
        );
        let mut reader = file
            .target
            .store
            .open_read(requester, &HlPath::default(), "a.txt")
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
                        store: shared_files(&state, "alice").await,
                        dir: HlPath::default(),
                        name: "a.txt".into(),
                    },
                    raw: true,
                    encoding: text_encoding::DEFAULT,
                    info: Vec::new(),
                    resource_size: 0,
                    resource_offset: 0,
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
                        store: shared_files(&state, "bob").await,
                        dir: HlPath::default(),
                        name: "a.txt".into(),
                    },
                    raw: true,
                    encoding: text_encoding::DEFAULT,
                    info: Vec::new(),
                    resource_size: 0,
                    resource_offset: 0,
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

    #[tokio::test]
    async fn htxf_connection_of_the_wrong_type_is_closed_and_the_transfer_stays() {
        let state = state_with_files(InMemoryFileProvider::default());
        let reference = state
            .register_transfer(a_txt_transfer(&state, TransferDirection::Get, 1).await)
            .await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let header = HtxfHeader {
            reference: reference.0,
            len: 0,
            transfer_type: HtxfType::Folder,
        };
        client.write_all(&header.encode()).await.unwrap();

        handle_htxf_connection(server_stream, peer, &state)
            .await
            .unwrap();

        let mut byte = [0u8; 1];
        assert_eq!(client.read(&mut byte).await.unwrap(), 0);
        assert!(state.take_transfer(reference).await.is_some());
    }

    #[tokio::test]
    async fn send_flat_file_fails_on_a_short_fork() {
        let mut out = Vec::new();
        let short_data = send_flat_file(&mut out, b"", (&b"hey"[..], 5), (&b""[..], 0)).await;
        assert!(short_data.is_err());
        let mut out = Vec::new();
        let short_resource =
            send_flat_file(&mut out, b"", (&b"hello"[..], 5), (&b"rs"[..], 4)).await;
        assert!(short_resource.is_err());
    }

    #[tokio::test]
    async fn htxf_types_find_transfers_as_openline_does() {
        let state = Arc::<SharedState>::default();
        let get = a_txt_transfer(&state, TransferDirection::Get, 1).await;
        let put = a_txt_transfer(&state, TransferDirection::Put, 1).await;
        let banner = PendingTransfer {
            kind: TransferKind::Banner,
            ..get.clone()
        };
        assert!(get.accepts(HtxfType::File) && get.accepts(HtxfType::Unknown(7)));
        assert!(!get.accepts(HtxfType::Folder) && !get.accepts(HtxfType::Banner));
        assert!(put.accepts(HtxfType::File) && put.accepts(HtxfType::Banner));
        assert!(!put.accepts(HtxfType::Folder));
        assert!(banner.accepts(HtxfType::Banner) && !banner.accepts(HtxfType::File));
    }

    /// A transfer of `a.txt` in the root folder, from loopback.
    async fn a_txt_transfer(
        state: &SharedState,
        direction: TransferDirection,
        user_id: u16,
    ) -> PendingTransfer {
        PendingTransfer {
            kind: TransferKind::File(FileTransfer {
                target: TransferTarget {
                    login: "alice".into(),
                    store: shared_files(state, "alice").await,
                    dir: HlPath::default(),
                    name: "a.txt".into(),
                },
                raw: direction == TransferDirection::Get,
                encoding: text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
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
        connect_htxf_as(reference, HtxfType::File).await
    }

    /// Like [`connect_htxf`], with a header of `transfer_type`.
    async fn connect_htxf_as(
        reference: TransferRef,
        transfer_type: HtxfType,
    ) -> (TcpStream, TcpStream, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (server_stream, peer) = listener.accept().await.unwrap();
        let header = HtxfHeader {
            reference: reference.0,
            len: 0,
            transfer_type,
        };
        client.write_all(&header.encode()).await.unwrap();
        (client, server_stream, peer)
    }

    #[tokio::test]
    async fn an_upload_stops_when_its_session_ends() {
        let state = state_with_files(InMemoryFileProvider::default());
        let session = state.register_recipient(1).await;
        let reference = state
            .register_transfer(a_txt_transfer(&state, TransferDirection::Put, 1).await)
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
    async fn an_upload_stops_when_its_session_is_kicked() {
        let state = state_with_files(InMemoryFileProvider::default());
        let _session = state.register_recipient(1).await;
        let reference = state
            .register_transfer(a_txt_transfer(&state, TransferDirection::Put, 1).await)
            .await;
        let (mut client, server_stream, peer) = connect_htxf(reference).await;
        let full = htxf::encode_flat_file(b"hello world");
        client.write_all(&full[..full.len() - 1]).await.unwrap();

        state.kick(1, "Go away.").await;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            handle_htxf_connection(server_stream, peer, &state),
        )
        .await;

        assert!(result.expect("the upload should stop").is_ok());
        drop(client);
    }

    #[tokio::test]
    async fn a_banner_download_whose_session_has_ended_is_refused() {
        let state = state_with_files(InMemoryFileProvider::default());
        let banner = PendingTransfer {
            kind: TransferKind::Banner,
            ..a_txt_transfer(&state, TransferDirection::Get, 1).await
        };
        let reference = state.register_transfer(banner).await;
        let (_client, server_stream, peer) = connect_htxf_as(reference, HtxfType::Banner).await;

        let result = handle_htxf_connection(server_stream, peer, &state).await;

        assert!(result.is_ok());
        assert!(state.take_transfer(reference).await.is_none());
    }

    #[tokio::test]
    async fn an_upload_whose_session_has_ended_is_refused() {
        let state = state_with_files(InMemoryFileProvider::default());
        let reference = state
            .register_transfer(a_txt_transfer(&state, TransferDirection::Put, 1).await)
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
            .register_transfer(a_txt_transfer(&state, TransferDirection::Get, 1).await)
            .await;
        assert_eq!(state.admit_download(held, 1).await, 0);
        let queued = state
            .register_transfer(a_txt_transfer(&state, TransferDirection::Get, 2).await)
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
            .register_transfer(a_txt_transfer(&state, TransferDirection::Get, 3).await)
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
        state: &Arc<SharedState>,
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

    async fn folder_transfer(
        state: &SharedState,
        name: &str,
        items: Vec<crate::file::FolderItem>,
        item_count: u32,
    ) -> FolderTransfer {
        FolderTransfer {
            target: TransferTarget {
                login: "alice".into(),
                store: shared_files(state, "alice").await,
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
        read_sized_flat_file_forks(client).await.0
    }

    /// A file's `size(4)` then its flattened object, returning the object's
    /// `DATA` and `MACR` fork bytes.
    async fn read_sized_flat_file_forks(client: &mut TcpStream) -> (Vec<u8>, Vec<u8>) {
        let mut size = [0u8; 4];
        client.read_exact(&mut size).await.unwrap();
        let mut flat = vec![0u8; u32::from_be_bytes(size) as usize];
        client.read_exact(&mut flat).await.unwrap();
        let header_len = flat_header_len(&flat);
        let data_header = ForkHeader::decode(&flat[header_len - ForkHeader::LEN..]).unwrap();
        assert_eq!(&data_header.fork_type, b"DATA");
        let mut data = flat.split_off(header_len);
        let mut resource = data.split_off(data_header.data_size as usize);
        let resource_header = ForkHeader::decode(&resource).unwrap();
        assert_eq!(&resource_header.fork_type, b"MACR");
        let resource = resource.split_off(ForkHeader::LEN);
        assert_eq!(resource.len(), resource_header.data_size as usize);
        (data, resource)
    }

    /// The header length of the flattened object `flat`, from its `INFO`
    /// fork size.
    fn flat_header_len(flat: &[u8]) -> usize {
        let info = ForkHeader::decode(&flat[htxf::FLAT_FILE_PREFIX_LEN..]).unwrap();
        htxf::flat_file_header_len(info.data_size as usize)
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

    /// A flattened object with `data` and a `MACR` fork of `resource`.
    fn flat_file_with_resource(data: &[u8], resource: &[u8]) -> Vec<u8> {
        let info = htxf::empty_info();
        let mut flat = htxf::flat_file_header_with_resource(&info, data.len() as u32);
        flat.extend_from_slice(data);
        let resource_header = ForkHeader {
            fork_type: *b"MACR",
            data_size: resource.len() as u32,
        };
        flat.extend_from_slice(&resource_header.encode());
        flat.extend_from_slice(resource);
        flat
    }

    async fn read_resource_fork(state: &SharedState, dir: &[&str], name: &str) -> Vec<u8> {
        let requester = FileRequester { login: "alice" };
        let dir = HlPath::new(dir.iter().copied());
        let mut reader = shared_files(state, "alice")
            .await
            .open_resource_read(requester, &dir, name, 0)
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        buf
    }

    async fn read_file(state: &SharedState, dir: &[&str], name: &str) -> Vec<u8> {
        let requester = FileRequester { login: "alice" };
        let mut reader = shared_files(state, "alice")
            .await
            .open_read(requester, &HlPath::new(dir.iter().copied()), name)
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
        let store = shared_files(&state, "alice").await;
        let items = state
            .walk_folder(&*store, requester, &HlPath::default(), "Top", Access::all())
            .await
            .unwrap();
        let folder = folder_transfer(&state, "Top", items, 3).await;
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
        let store = shared_files(&state, "alice").await;
        let items = state
            .walk_folder(&*store, requester, &HlPath::default(), "Top", Access::all())
            .await
            .unwrap();
        let folder = folder_transfer(&state, "Top", items, 1).await;
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Get).await;

        send_action(&mut client, FolderAction::NextFile).await;
        read_item_header(&mut client).await;
        let resume_data = ResumeData {
            data: 2,
            resource: 0,
        }
        .encode();
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

    #[tokio::test]
    async fn folder_download_sends_and_resumes_the_resource_fork() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        files.seed_resource_fork(&HlPath::new(["Top"]), "a.txt", *b"rsrc");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let store = shared_files(&state, "alice").await;
        let items = state
            .walk_folder(&*store, requester, &HlPath::default(), "Top", Access::all())
            .await
            .unwrap();
        let folder = folder_transfer(&state, "Top", items, 1).await;
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Get).await;

        send_action(&mut client, FolderAction::NextFile).await;
        read_item_header(&mut client).await;
        let resume_data = ResumeData {
            data: 2,
            resource: 1,
        }
        .encode();
        send_action(&mut client, FolderAction::ResumeFile).await;
        client
            .write_all(&(resume_data.len() as u16).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&resume_data).await.unwrap();
        let (data, resource) = read_sized_flat_file_forks(&mut client).await;
        assert_eq!(data, b"llo");
        assert_eq!(resource, b"src");

        drop(client);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn folder_upload_resumes_both_forks() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt.hpf", *b"hello ");
        files.seed_resource_fork(&HlPath::new(["Top"]), "a.txt.hpf", *b"rs");
        let state = state_with_files(files);
        let folder = folder_transfer(&state, "Top", Vec::new(), 1).await;
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Put).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["a.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::ResumeFile);
        let resume = read_resume_data(&mut client).await.unwrap();
        assert_eq!(
            resume,
            ResumeData {
                data: 6,
                resource: 2
            }
        );
        let flat = flat_file_with_resource(b"world", b"rc");
        client
            .write_all(&(flat.len() as u32).to_be_bytes())
            .await
            .unwrap();
        client.write_all(&flat).await.unwrap();

        server.await.unwrap();
        assert_eq!(read_file(&state, &["Top"], "a.txt").await, b"hello world");
        assert_eq!(read_resource_fork(&state, &["Top"], "a.txt").await, b"rsrc");
    }

    /// Every item counts toward the declared total, a skipped one too.
    /// A skipped last item still gets a `NextFile` after it.
    #[tokio::test]
    async fn folder_upload_creates_items_skips_existing_files_and_ends_at_the_item_count() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "exists.txt", *b"old");
        let state = state_with_files(files);
        let folder = folder_transfer(&state, "Top", Vec::new(), 3).await;
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
        let folder = folder_transfer(&state, "Top", Vec::new(), 1).await;
        let (mut client, server) =
            start_folder_transfer(&state, folder, TransferDirection::Put).await;

        assert_eq!(read_action(&mut client).await, FolderAction::NextFile);
        send_item_header(&mut client, false, &["a.txt"]).await;
        assert_eq!(read_action(&mut client).await, FolderAction::ResumeFile);
        assert_eq!(read_resume_data(&mut client).await.unwrap().data, 6);
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
        let folder = folder_transfer(&state, "Top", Vec::new(), 2).await;
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
