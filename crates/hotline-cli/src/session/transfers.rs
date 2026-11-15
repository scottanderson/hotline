//! File and folder downloads and uploads.

use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures::future::BoxFuture;
use hotline_client::{FolderDownload, FolderUpload, HotlineClient};
use hotline_proto::Access;
use hotline_proto::htxf::FolderItemHeader;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, BufReader, BufWriter};
use tokio::sync::mpsc::UnboundedSender;

use super::running::{Counted, Kind, Progress};
use super::{ClientSession, Request};
use crate::line::{Line, LineKind};
use crate::transfer::{Conflict, Start};

/// Why a transfer does not start.
enum Stop {
    /// It waits for the user to choose how to start.
    Ask(Conflict),
    /// It failed, with this line.
    Fail(String),
}

/// A line from a transfer that runs on its own task.
pub enum TransferLine {
    /// The server took the transfer.
    Started(Line),
    /// The transfer ended, well or not.
    Ended(Line),
}

impl ClientSession {
    /// Start to download the file `name` in the current folder into
    /// `download_dir` as `save_as`, [`myTran_DownloadFile`] (202). The data
    /// goes into `save_as.hpf`, which becomes `save_as` when the download
    /// ends. The transfer runs on its own task when its turn comes, and sends
    /// its lines to `lines`. With [`Start::Ask`], a local `save_as` or
    /// `save_as.hpf` stops the download and comes back as a [`Conflict`].
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub fn download_file(
        &self,
        client: &HotlineClient,
        name: &str,
        save_as: &str,
        start: Start,
        lines: &UnboundedSender<TransferLine>,
    ) -> Request<Option<Conflict>> {
        let started_at = Instant::now();
        let local = self.download_dir.join(local_name(save_as));
        let partial = partial_path(&local);
        let client = client.clone();
        let path = self.file_path.clone();
        let (name, save_as) = (name.to_string(), save_as.to_string());
        let lines = lines.clone();
        Request::new(async move {
            let opened = async {
                if start == Start::Ask {
                    let (name, save_as) = (name.as_str().into(), save_as.as_str().into());
                    if exists(&local).await {
                        return Err(Stop::Ask(Conflict::LocalFileExists { name, save_as }));
                    }
                    if exists(&partial).await {
                        return Err(Stop::Ask(Conflict::PartialDownload { name, save_as }));
                    }
                }
                open_partial(&partial, start)
                    .await
                    .map_err(|e| Stop::Fail(format!("cannot save {}: {e}", partial.display())))
            }
            .await;
            move |session: &mut Self| {
                let (file, offset) = match opened {
                    Ok(opened) => opened,
                    Err(stop) => return session.stop(stop),
                };
                let progress = Arc::new(Progress::default());
                let task_progress = progress.clone();
                let queues = session.server_queues();
                let (start_turn, turn) = session.turn();
                let task_name = name.clone();
                let task = tokio::spawn(async move {
                    let (name, progress) = (task_name, task_progress);
                    turn.wait().await;
                    let download = match client.download_file(&path, &name, offset).await {
                        Ok(download) => download,
                        Err(e) => {
                            if start != Start::Resume {
                                let _ = tokio::fs::remove_file(&partial).await;
                            }
                            let line = Line::error(format!("failed to download {name}: {e}"));
                            let _ = lines.send(TransferLine::Ended(line));
                            return;
                        }
                    };
                    progress.set_size(offset.into(), download.file_size.into());
                    progress.set_reference(download.reference());
                    if queues {
                        progress.set_waiting(download.waiting_count);
                    }
                    let queue = queue_text(download.waiting_count);
                    let from = from_text(offset);
                    let line = Line::system(format!("downloading {name}{from}{queue}"));
                    let _ = lines.send(TransferLine::Started(line));
                    let mut out = Counted::writes(BufWriter::new(file), progress.clone());
                    let received = async {
                        let bytes = download.receive(&mut out).await?;
                        tokio::fs::rename(&partial, &local).await?;
                        Ok::<_, anyhow::Error>(bytes)
                    };
                    let line = match received.await {
                        Ok(bytes) => {
                            progress.succeed();
                            Line::system(format!(
                                "downloaded {name} ({bytes} bytes) to {}",
                                local.display()
                            ))
                        }
                        Err(e) => Line::error(format!("failed to download {name}: {e}")),
                    };
                    let _ = lines.send(TransferLine::Ended(line));
                });
                session.track(
                    Kind::Download,
                    &name,
                    started_at,
                    progress,
                    task.abort_handle(),
                    start_turn,
                );
                None
            }
        })
    }

    /// Start to download the folder `name` in the current folder, with
    /// everything inside it, into `download_dir`, [`myTran_DownloadFldr`] (210).
    /// The transfer runs on its own task when its turn comes, and sends its
    /// lines to `lines`. With [`Start::Ask`], an existing local folder stops
    /// the download and comes back as a [`Conflict`]. [`Start::Resume`] skips
    /// each file that is there and continues each `.hpf` file.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    pub fn download_folder(
        &self,
        client: &HotlineClient,
        name: &str,
        start: Start,
        lines: &UnboundedSender<TransferLine>,
    ) -> Request<Option<Conflict>> {
        let started_at = Instant::now();
        let root = self.download_dir.join(local_name(name));
        let client = client.clone();
        let path = self.file_path.clone();
        let name = name.to_string();
        let lines = lines.clone();
        Request::new(async move {
            let created = match start {
                Start::Ask => tokio::fs::create_dir(&root).await,
                Start::Resume | Start::Replace => tokio::fs::create_dir_all(&root).await,
            };
            let created = match created {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                    let name = name.as_str().into();
                    Err(Stop::Ask(Conflict::LocalFolderExists { name }))
                }
                Err(e) => Err(Stop::Fail(format!("cannot save {}: {e}", root.display()))),
            };
            move |session: &mut Self| {
                if let Err(stop) = created {
                    return session.stop(stop);
                }
                let progress = Arc::new(Progress::default());
                let task_progress = progress.clone();
                let queues = session.server_queues();
                let (start_turn, turn) = session.turn();
                let task_name = name.clone();
                let task = tokio::spawn(async move {
                    let (name, progress) = (task_name, task_progress);
                    turn.wait().await;
                    let download = match client.download_folder(&path, &name).await {
                        Ok(download) => download,
                        Err(e) => {
                            if start == Start::Ask {
                                let _ = tokio::fs::remove_dir(&root).await;
                            }
                            let line = Line::error(format!("failed to download {name}: {e}"));
                            let _ = lines.send(TransferLine::Ended(line));
                            return;
                        }
                    };
                    let items = download.item_count;
                    progress.set_item_count(items);
                    progress.set_size(0, download.transfer_size.into());
                    if let Some(reference) = download.reference() {
                        progress.set_reference(reference);
                    }
                    if queues {
                        progress.set_waiting(download.waiting_count);
                    }
                    let queue = queue_text(download.waiting_count);
                    let line = Line::system(format!("downloading {name} ({items} items{queue})"));
                    let _ = lines.send(TransferLine::Started(line));
                    let line = match receive_folder(download, &root, start, &progress).await {
                        Ok((files, bytes)) => {
                            progress.succeed();
                            Line::system(format!(
                                "downloaded {name} ({files} files, {bytes} bytes) to {}",
                                root.display()
                            ))
                        }
                        Err(e) => Line::error(format!("failed to download {name}: {e}")),
                    };
                    let _ = lines.send(TransferLine::Ended(line));
                });
                let kind = Kind::DownloadFolder;
                session.track(
                    kind,
                    &name,
                    started_at,
                    progress,
                    task.abort_handle(),
                    start_turn,
                );
                None
            }
        })
    }

    /// Show why a transfer did not start. Gives the conflict to ask about.
    fn stop(&mut self, stop: Stop) -> Option<Conflict> {
        match stop {
            Stop::Ask(conflict) => Some(conflict),
            Stop::Fail(text) => {
                self.push(text, LineKind::Error);
                None
            }
        }
    }

    /// Start to upload the local file or folder at `local` to the current
    /// folder under its own name. The transfer runs on its own task when its
    /// turn comes, and sends its lines to `lines`. With [`Start::Ask`], a
    /// partial upload or a folder of that name in the listing stops the
    /// upload and comes back as a [`Conflict`].
    pub fn upload(
        &mut self,
        client: &HotlineClient,
        local: &Path,
        start: Start,
        lines: &UnboundedSender<TransferLine>,
    ) -> Request<Option<Conflict>> {
        let Some(name) = local.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            self.push(
                format!("{} names no file", local.display()),
                LineKind::Error,
            );
            return Request::ready(None);
        };
        let started_at = Instant::now();
        let access = self.granted();
        let lists_folder = self.lists_folder(&name);
        let lists_partial = self
            .files
            .as_deref()
            .is_some_and(|files| lists_partial_upload(files, &name));
        let client = client.clone();
        let path = self.file_path.clone();
        let local = local.to_path_buf();
        let lines = lines.clone();
        Request::new(async move {
            let conflict_local = || local.to_string_lossy().into();
            let kind = match tokio::fs::metadata(&local).await {
                Ok(metadata) if metadata.is_dir() => {
                    if !access.contains(Access::UPLOAD_FOLDER) {
                        Err(Stop::Fail("You are not allowed to upload folders.".into()))
                    } else if start == Start::Ask && lists_folder {
                        let local = conflict_local();
                        Err(Stop::Ask(Conflict::RemoteFolderExists { local }))
                    } else {
                        Ok(Kind::UploadFolder)
                    }
                }
                Ok(_) => {
                    if !access.contains(Access::UPLOAD_FILE) {
                        Err(Stop::Fail("You are not allowed to upload files.".into()))
                    } else if start == Start::Ask && lists_partial {
                        let local = conflict_local();
                        Err(Stop::Ask(Conflict::PartialUpload { local }))
                    } else {
                        Ok(Kind::Upload)
                    }
                }
                Err(e) => Err(Stop::Fail(format!("cannot read {}: {e}", local.display()))),
            };
            move |session: &mut Self| {
                let kind = match kind {
                    Ok(kind) => kind,
                    Err(stop) => return session.stop(stop),
                };
                let progress = Arc::new(Progress::default());
                let task_progress = progress.clone();
                let (start_turn, turn) = session.turn();
                let task_name = name.clone();
                let task = tokio::spawn(async move {
                    let (name, progress) = (task_name, task_progress);
                    turn.wait().await;
                    let upload = match kind {
                        Kind::UploadFolder => {
                            start_folder_upload(&client, &path, &local, &name, start, progress)
                                .await
                        }
                        _ => {
                            start_file_upload(&client, &path, &local, &name, start, progress).await
                        }
                    };
                    let line = match upload {
                        Ok(upload) => {
                            let _ = lines.send(TransferLine::Started(Line::system(upload.text)));
                            upload.transfer.await
                        }
                        Err(text) => Line::error(text),
                    };
                    let _ = lines.send(TransferLine::Ended(line));
                });
                session.track(
                    kind,
                    &name,
                    started_at,
                    progress,
                    task.abort_handle(),
                    start_turn,
                );
                None
            }
        })
    }

    /// `true` if the current folder's listing has a folder called `name`.
    pub fn lists_folder(&self, name: &str) -> bool {
        self.files
            .iter()
            .flatten()
            .any(|f| &*f.name == name && f.is_folder())
    }
}

/// A started upload.
struct Upload {
    /// The line that tells of the start.
    text: String,
    /// The transfer, which gives its result line.
    transfer: BoxFuture<'static, Line>,
}

/// [`myTran_UploadFile`] (203) for the local file at `local`, as `name` in
/// the folder `path`.
///
/// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
async fn start_file_upload(
    client: &HotlineClient,
    path: &HlPath,
    local: &Path,
    name: &str,
    start: Start,
    progress: Arc<Progress>,
) -> Result<Upload, String> {
    let cannot_read = |e: std::io::Error| format!("cannot read {}: {e}", local.display());
    let mut file = File::open(local).await.map_err(cannot_read)?;
    let size = file.metadata().await.map_err(cannot_read)?.len();
    let resume = start == Start::Resume;
    let upload = client
        .upload_file(path, name, size, resume)
        .await
        .map_err(|e| format!("failed to upload {name}: {e}"))?;
    let offset = upload.offset();
    file.seek(SeekFrom::Start(offset.into()))
        .await
        .map_err(cannot_read)?;
    let from = from_text(offset);
    let name = name.to_string();
    let text = format!("uploading {name}{from}");
    progress.set_size(offset.into(), size);
    let mut data = Counted::reads(BufReader::new(file), progress.clone());
    let transfer = async move {
        match upload.send(&mut data).await {
            Ok(bytes) => {
                progress.succeed();
                Line::system(format!("uploaded {name} ({bytes} bytes)"))
            }
            Err(e) => Line::error(format!("failed to upload {name}: {e}")),
        }
    };
    Ok(Upload {
        text,
        transfer: Box::pin(transfer),
    })
}

/// [`myTran_UploadFldr`] (213) for the local folder at `local`, with
/// everything inside it, as `name` in the folder `path`.
///
/// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
async fn start_folder_upload(
    client: &HotlineClient,
    path: &HlPath,
    local: &Path,
    name: &str,
    start: Start,
    progress: Arc<Progress>,
) -> Result<Upload, String> {
    let root = local.to_path_buf();
    let items = tokio::task::spawn_blocking(move || walk_local(&root))
        .await
        .map_err(std::io::Error::other)
        .and_then(|items| items)
        .map_err(|e| format!("cannot read {}: {e}", local.display()))?;
    let item_count = u32::try_from(items.len()).unwrap_or(u32::MAX);
    let total_size = items
        .iter()
        .filter_map(|item| item.file.as_ref())
        .map(|(_, size)| size)
        .sum();
    let resume = start == Start::Resume;
    let upload = client
        .upload_folder(path, name, item_count, total_size, resume)
        .await
        .map_err(|e| format!("failed to upload {name}: {e}"))?;
    let name = name.to_string();
    let text = format!("uploading {name} ({item_count} items)");
    progress.set_item_count(item_count);
    progress.set_size(0, total_size);
    let transfer = async move {
        match send_folder(upload, items, &progress).await {
            Ok((files, bytes)) => {
                progress.succeed();
                Line::system(format!("uploaded {name} ({files} files, {bytes} bytes)"))
            }
            Err(e) => Line::error(format!("failed to upload {name}: {e}")),
        }
    };
    Ok(Upload {
        text,
        transfer: Box::pin(transfer),
    })
}

/// Save each item of `download` under the local folder `root`. With
/// [`Start::Resume`], a file that is there is skipped and a `.hpf` file is
/// continued. Each item and each byte of the connection counts in
/// `progress`. Returns the number of files and bytes saved.
async fn receive_folder(
    download: FolderDownload,
    root: &Path,
    start: Start,
    progress: &Arc<Progress>,
) -> anyhow::Result<(u32, u64)> {
    let mut receiver = download
        .open_with(|stream| Counted::reads(stream, progress.clone()))
        .await?;
    let (mut files, mut bytes) = (0_u32, 0_u64);
    while let Some(item) = receiver.next_item().await? {
        progress.add_item();
        let local = item
            .path
            .components
            .iter()
            .fold(root.to_path_buf(), |path, name| path.join(local_name(name)));
        if item.is_folder {
            tokio::fs::create_dir_all(&local).await?;
            continue;
        }
        if start == Start::Resume && exists(&local).await {
            continue;
        }
        if let Some(parent) = local.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let partial = partial_path(&local);
        let item_start = match start {
            Start::Resume if exists(&partial).await => Start::Resume,
            _ => Start::Replace,
        };
        let (file, offset) = open_partial(&partial, item_start).await?;
        let mut out = BufWriter::new(file);
        let received = match offset {
            0 => receiver.receive_file(&mut out).await?,
            offset => receiver.resume_file(&mut out, offset).await?,
        };
        bytes = bytes.saturating_add(received);
        tokio::fs::rename(&partial, &local).await?;
        files = files.saturating_add(1);
    }
    Ok((files, bytes))
}

/// The working copy of the local file `local` while its download runs.
fn partial_path(local: &Path) -> PathBuf {
    let mut name = local.file_name().unwrap_or_default().to_os_string();
    name.push(".hpf");
    local.with_file_name(name)
}

/// Open the working copy `partial` for a download. [`Start::Resume`] adds to
/// it and also returns its size; [`Start::Ask`] creates it.
async fn open_partial(partial: &Path, start: Start) -> std::io::Result<(File, u32)> {
    let mut options = OpenOptions::new();
    match start {
        Start::Resume => options.append(true),
        Start::Ask => options.write(true).create_new(true),
        Start::Replace => options.write(true).create(true).truncate(true),
    };
    let file = options.open(partial).await?;
    let offset = match start {
        Start::Resume => u32::try_from(file.metadata().await?.len())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "the file is too large"))?,
        Start::Ask | Start::Replace => 0,
    };
    Ok((file, offset))
}

async fn exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

/// One item inside a local folder to upload.
#[derive(Debug, PartialEq, Eq)]
struct LocalItem {
    /// The names from inside the folder to the item.
    path: Vec<String>,
    /// A file's local path and size, `None` for a folder.
    file: Option<(PathBuf, u64)>,
}

/// Every item inside the local folder `root`, each folder before the items
/// inside it. Symbolic links are skipped.
fn walk_local(root: &Path) -> std::io::Result<Vec<LocalItem>> {
    let mut items = Vec::new();
    let mut folders = vec![(root.to_path_buf(), Vec::new())];
    while let Some((folder, prefix)) = folders.pop() {
        let mut entries = std::fs::read_dir(&folder)?.collect::<std::io::Result<Vec<_>>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let kind = entry.file_type()?;
            let mut path = prefix.clone();
            path.push(entry.file_name().to_string_lossy().into_owned());
            if kind.is_dir() {
                folders.push((entry.path(), path.clone()));
                items.push(LocalItem { path, file: None });
            } else if kind.is_file() {
                let file = Some((entry.path(), entry.metadata()?.len()));
                items.push(LocalItem { path, file });
            }
        }
    }
    Ok(items)
}

/// Send `items` on `upload`. Each item counts in `progress`. Returns the
/// number of files and bytes sent.
async fn send_folder(
    upload: FolderUpload,
    items: Vec<LocalItem>,
    progress: &Arc<Progress>,
) -> anyhow::Result<(u32, u64)> {
    let Some(mut sender) = upload.open().await? else {
        return Ok((0, 0));
    };
    let (mut files, mut bytes) = (0_u32, 0_u64);
    for item in items {
        let header = FolderItemHeader {
            is_folder: item.file.is_none(),
            path: HlPath::new(item.path),
        };
        let offset = sender.send_item(&header).await?;
        progress.add_item();
        if let (Some(offset), Some((local, size))) = (offset, item.file) {
            let mut file = File::open(&local).await?;
            file.seek(SeekFrom::Start(offset)).await?;
            let mut data = Counted::reads(BufReader::new(file), progress.clone());
            let sent = sender
                .send_file(&mut data, size.saturating_sub(offset))
                .await?;
            bytes = bytes.saturating_add(sent);
            files = files.saturating_add(1);
        }
    }
    progress.finish();
    sender.finish().await?;
    Ok((files, bytes))
}

/// The server file name `name` as one local path component.
fn local_name(name: &str) -> String {
    let name = name.replace(['/', '\\', ':'], "_");
    match name.as_str() {
        "" | "." | ".." => format!("_{name}"),
        _ => name,
    }
}

/// `, <n> ahead in the queue` for a queued transfer, or nothing.
fn queue_text(waiting_count: u32) -> String {
    match waiting_count {
        0 => String::new(),
        n => format!(", {n} ahead in the queue"),
    }
}

/// ` from byte <n>` for a resumed transfer, or nothing.
fn from_text(offset: u32) -> String {
    match offset {
        0 => String::new(),
        n => format!(" from byte {n}"),
    }
}

/// `true` if `files` has a partial upload of `name`: a file `name.hpf`,
/// or a file `name` with type `HTft` and creator `HTLC`.
fn lists_partial_upload(files: &[FileNameWithInfo], name: &str) -> bool {
    let partial = format!("{name}.hpf");
    files
        .iter()
        .filter(|f| !f.is_folder())
        .any(|f| *f.name == partial || (&*f.name == name && f.is_partial_upload()))
}

#[cfg(test)]
mod tests {
    use hotline_proto::types::{PARTIAL_CREATOR, PARTIAL_TYPE};

    use super::*;
    use crate::session::test_support;

    #[test]
    fn walk_local_lists_each_folder_before_its_items() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir(root.join("Sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("Sub").join("b.txt"), b"be").unwrap();

        let items = walk_local(root);

        let items: Vec<_> = items
            .unwrap()
            .into_iter()
            .map(|item| (item.path.join("/"), item.file.map(|(_, size)| size)))
            .collect();
        assert_eq!(
            items,
            [
                ("Sub".to_string(), None),
                ("a.txt".to_string(), Some(5)),
                ("Sub/b.txt".to_string(), Some(2)),
            ]
        );
    }

    /// A session logged in as admin to a server with `files`, saving
    /// downloads in `dir`.
    async fn file_session(
        files: hotline_server::InMemoryFileProvider,
        dir: &Path,
    ) -> (ClientSession, HotlineClient) {
        let providers = hotline_server::Providers {
            files: std::sync::Arc::new(files),
            ..hotline_server::Providers::default()
        };
        let addr = test_support::start_server(providers).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        session.download_dir = dir.to_path_buf();
        session.access = Access::all();
        (session, client)
    }

    /// The line a transfer sends when it ends.
    async fn transfer_done(lines: &mut tokio::sync::mpsc::UnboundedReceiver<TransferLine>) -> Line {
        loop {
            let line = tokio::time::timeout(test_support::WAIT, lines.recv())
                .await
                .expect("transfer ends")
                .expect("channel open");
            if let TransferLine::Ended(line) = line {
                return line;
            }
        }
    }

    #[tokio::test]
    async fn download_file_asks_before_it_resumes_a_partial_download() {
        let files = hotline_server::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt.hpf"), b"hello ").unwrap();
        let (mut session, client) = file_session(files, dir.path()).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let conflict = session
            .download_file(&client, "a.txt", "a.txt", Start::Ask, &tx)
            .run(&mut session)
            .await;
        assert_eq!(
            conflict,
            Some(Conflict::PartialDownload {
                name: "a.txt".into(),
                save_as: "a.txt".into()
            })
        );

        let conflict = session
            .download_file(&client, "a.txt", "a.txt", Start::Resume, &tx)
            .run(&mut session)
            .await;
        assert_eq!(conflict, None);
        let line = transfer_done(&mut rx).await;
        assert_eq!(line.kind, LineKind::System, "{}", line.text);
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"hello world"
        );
        assert!(!dir.path().join("a.txt.hpf").exists());
    }

    #[tokio::test]
    async fn download_file_asks_for_a_new_name_when_the_file_is_there() {
        let files = hotline_server::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"new");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"old").unwrap();
        let (mut session, client) = file_session(files, dir.path()).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let conflict = session
            .download_file(&client, "a.txt", "a.txt", Start::Ask, &tx)
            .run(&mut session)
            .await;
        assert!(matches!(conflict, Some(Conflict::LocalFileExists { .. })));

        let conflict = session
            .download_file(&client, "a.txt", "b.txt", Start::Ask, &tx)
            .run(&mut session)
            .await;
        assert_eq!(conflict, None);
        transfer_done(&mut rx).await;
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(dir.path().join("b.txt")).unwrap(), b"new");
    }

    #[tokio::test]
    async fn download_folder_resume_skips_whole_files_and_continues_partial_ones() {
        let files = hotline_server::InMemoryFileProvider::default();
        let games = HlPath::new(["Games"]);
        files.seed(&games, "a.txt", *b"aaaa");
        files.seed(&games, "b.txt", *b"bbbb");
        files.seed(&games, "c.txt", *b"cccc");
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Games");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("a.txt"), b"old").unwrap();
        std::fs::write(root.join("b.txt.hpf"), b"bb").unwrap();
        let (mut session, client) = file_session(files, dir.path()).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let conflict = session
            .download_folder(&client, "Games", Start::Ask, &tx)
            .run(&mut session)
            .await;
        assert_eq!(
            conflict,
            Some(Conflict::LocalFolderExists {
                name: "Games".into()
            })
        );

        let conflict = session
            .download_folder(&client, "Games", Start::Resume, &tx)
            .run(&mut session)
            .await;
        assert_eq!(conflict, None);
        let line = transfer_done(&mut rx).await;
        assert_eq!(line.kind, LineKind::System, "{}", line.text);
        assert_eq!(std::fs::read(root.join("a.txt")).unwrap(), b"old");
        assert_eq!(std::fs::read(root.join("b.txt")).unwrap(), b"bbbb");
        assert_eq!(std::fs::read(root.join("c.txt")).unwrap(), b"cccc");
        assert!(!root.join("b.txt.hpf").exists());
    }

    #[tokio::test]
    async fn upload_needs_the_privilege_for_a_file_or_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        std::fs::write(&local, b"hello").unwrap();
        let files = hotline_server::InMemoryFileProvider::default();
        let (mut session, client) = file_session(files, dir.path()).await;
        session.server_version = Some(190);
        session.access = Access::UPLOAD_FOLDER;
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        assert_eq!(
            session
                .upload(&client, &local, Start::Ask, &tx)
                .run(&mut session)
                .await,
            None
        );
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "You are not allowed to upload files."
        );
        session.access = Access::UPLOAD_FILE;
        assert_eq!(
            session
                .upload(&client, dir.path(), Start::Ask, &tx)
                .run(&mut session)
                .await,
            None
        );
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "You are not allowed to upload folders."
        );
    }

    #[tokio::test]
    async fn upload_asks_before_it_resumes_a_listed_partial_upload() {
        let files = hotline_server::InMemoryFileProvider::default();
        let uploads = HlPath::new(["Uploads"]);
        files.seed(&uploads, "a.txt.hpf", *b"hel");
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        std::fs::write(&local, b"hello").unwrap();
        let (mut session, client) = file_session(files, dir.path()).await;
        ClientSession::get_file_name_list(&client, uploads.clone())
            .run(&mut session)
            .await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let conflict = session
            .upload(&client, &local, Start::Ask, &tx)
            .run(&mut session)
            .await;
        assert!(matches!(conflict, Some(Conflict::PartialUpload { .. })));

        let conflict = session
            .upload(&client, &local, Start::Resume, &tx)
            .run(&mut session)
            .await;
        assert_eq!(conflict, None);
        let line = transfer_done(&mut rx).await;
        assert_eq!(line.kind, LineKind::System, "{}", line.text);

        let download = client.download_file(&uploads, "a.txt", 0).await.unwrap();
        let mut data = Vec::new();
        download.receive(&mut data).await.unwrap();
        assert_eq!(data, b"hello");
    }

    fn listed(name: &str, file_type: &[u8; 4], creator: &[u8; 4]) -> FileNameWithInfo {
        FileNameWithInfo {
            file_type: *file_type,
            creator: *creator,
            size: 0,
            name: name.into(),
        }
    }

    #[test]
    fn lists_partial_upload_matches_an_hpf_name_of_any_type() {
        let files = [listed("a.txt.hpf", b"TEXT", b"ttxt")];
        assert!(lists_partial_upload(&files, "a.txt"));
    }

    #[test]
    fn lists_partial_upload_matches_a_partial_type_and_creator() {
        let files = [listed("a.txt", &PARTIAL_TYPE, &PARTIAL_CREATOR)];
        assert!(lists_partial_upload(&files, "a.txt"));
    }

    #[test]
    fn lists_partial_upload_skips_a_finished_file_and_a_folder() {
        let files = [
            listed("a.txt", b"TEXT", b"ttxt"),
            listed("a.txt.hpf", b"fldr", b"\0\0\0\0"),
        ];
        assert!(!lists_partial_upload(&files, "a.txt"));
    }

    #[test]
    fn partial_path_adds_hpf_to_the_file_name() {
        assert_eq!(
            partial_path(Path::new("dl/a.txt")),
            Path::new("dl/a.txt.hpf")
        );
    }

    #[test]
    fn local_name_stays_inside_the_download_folder() {
        assert_eq!(local_name("doom.wad"), "doom.wad");
        assert_eq!(local_name("../etc/passwd"), ".._etc_passwd");
        assert_eq!(local_name("a\\b"), "a_b");
        assert_eq!(local_name("C:x"), "C_x");
        assert_eq!(local_name(".."), "_..");
        assert_eq!(local_name(""), "_");
    }
}
