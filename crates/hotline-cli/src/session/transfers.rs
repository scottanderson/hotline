//! File and folder downloads and uploads.

use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use hotline_client::{FolderDownload, FolderUpload, HotlineClient};
use hotline_proto::htxf::FolderItemHeader;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, BufReader, BufWriter};
use tokio::sync::mpsc::UnboundedSender;

use super::downloads::FirstWrite;
use super::ClientSession;
use crate::line::{Line, LineKind};
use crate::transfer::{Conflict, Start};

impl ClientSession {
    /// Start to download the file `name` in the current folder into
    /// `download_dir` as `save_as`, [`myTran_DownloadFile`] (202). The data
    /// goes into `save_as.hpf`, which becomes `save_as` when the download
    /// ends. The transfer runs on its own task and sends its result line to
    /// `done`. With [`Start::Ask`], a local `save_as` or `save_as.hpf` stops
    /// the download and comes back as a [`Conflict`].
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub async fn download_file(
        &mut self,
        client: &HotlineClient,
        name: &str,
        save_as: &str,
        start: Start,
        done: &UnboundedSender<Line>,
    ) -> Option<Conflict> {
        let local = self.download_dir.join(local_name(save_as));
        let partial = partial_path(&local);
        if start == Start::Ask {
            let (name, save_as) = (name.into(), save_as.into());
            if exists(&local).await {
                return Some(Conflict::LocalFileExists { name, save_as });
            }
            if exists(&partial).await {
                return Some(Conflict::PartialDownload { name, save_as });
            }
        }
        let (file, offset) = match open_partial(&partial, start).await {
            Ok(opened) => opened,
            Err(e) => {
                let text = format!("cannot save {}: {e}", partial.display());
                self.push(text, LineKind::Error);
                return None;
            }
        };
        let download = match client.download_file(&self.file_path, name, offset).await {
            Ok(download) => download,
            Err(e) => {
                if start != Start::Resume {
                    let _ = tokio::fs::remove_file(&partial).await;
                }
                self.push(format!("failed to download {name}: {e}"), LineKind::Error);
                return None;
            }
        };
        let queue = queue_text(download.waiting_count);
        let from = from_text(offset);
        self.push(format!("downloading {name}{from}{queue}"), LineKind::System);

        let queued = self.queued_flag(download.waiting_count);
        let reference = Some(download.reference());
        let mut out = FirstWrite::new(BufWriter::new(file), queued.clone());
        let task_name = name.to_string();
        let done = done.clone();
        let task = tokio::spawn(async move {
            let name = task_name;
            let received = async {
                let bytes = download.receive(&mut out).await?;
                tokio::fs::rename(&partial, &local).await?;
                Ok::<_, anyhow::Error>(bytes)
            };
            let line = match received.await {
                Ok(bytes) => Line::system(format!(
                    "downloaded {name} ({bytes} bytes) to {}",
                    local.display()
                )),
                Err(e) => Line::error(format!("failed to download {name}: {e}")),
            };
            let _ = done.send(line);
        });
        self.track_download(name, reference, queued, task.abort_handle());
        None
    }

    /// Start to download the folder `name` in the current folder, with
    /// everything inside it, into `download_dir`, [`myTran_DownloadFldr`] (210).
    /// The transfer runs on its own task and sends its result line to `done`.
    /// With [`Start::Ask`], an existing local folder stops the download and
    /// comes back as a [`Conflict`]. [`Start::Resume`] skips each file that
    /// is there and continues each `.hpf` file.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    pub async fn download_folder(
        &mut self,
        client: &HotlineClient,
        name: &str,
        start: Start,
        done: &UnboundedSender<Line>,
    ) -> Option<Conflict> {
        let root = self.download_dir.join(local_name(name));
        let created = match start {
            Start::Ask => tokio::fs::create_dir(&root).await,
            Start::Resume | Start::Replace => tokio::fs::create_dir_all(&root).await,
        };
        match created {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                return Some(Conflict::LocalFolderExists { name: name.into() });
            }
            Err(e) => {
                let text = format!("cannot save {}: {e}", root.display());
                self.push(text, LineKind::Error);
                return None;
            }
        }
        let download = match client.download_folder(&self.file_path, name).await {
            Ok(download) => download,
            Err(e) => {
                if start == Start::Ask {
                    let _ = tokio::fs::remove_dir(&root).await;
                }
                self.push(format!("failed to download {name}: {e}"), LineKind::Error);
                return None;
            }
        };
        let queue = queue_text(download.waiting_count);
        let items = download.item_count;
        self.push(
            format!("downloading {name} ({items} items{queue})"),
            LineKind::System,
        );

        let queued = self.queued_flag(download.waiting_count);
        let reference = download.reference();
        let task_queued = queued.clone();
        let task_name = name.to_string();
        let done = done.clone();
        let task = tokio::spawn(async move {
            let name = task_name;
            let line = match receive_folder(download, &root, start, &task_queued).await {
                Ok((files, bytes)) => Line::system(format!(
                    "downloaded {name} ({files} files, {bytes} bytes) to {}",
                    root.display()
                )),
                Err(e) => Line::error(format!("failed to download {name}: {e}")),
            };
            let _ = done.send(line);
        });
        self.track_download(name, reference, queued, task.abort_handle());
        None
    }

    /// Start to upload the local file or folder at `local` to the current
    /// folder under its own name. The transfer runs on its own task and sends
    /// its result line to `done`. With [`Start::Ask`], a partial upload or a
    /// folder of that name in the listing stops the upload and comes back as
    /// a [`Conflict`].
    pub async fn upload(
        &mut self,
        client: &HotlineClient,
        local: &Path,
        start: Start,
        done: &UnboundedSender<Line>,
    ) -> Option<Conflict> {
        let Some(name) = local.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            self.push(
                format!("{} names no file", local.display()),
                LineKind::Error,
            );
            return None;
        };
        let conflict_local = || local.to_string_lossy().into();
        match tokio::fs::metadata(local).await {
            Ok(metadata) if metadata.is_dir() => {
                if start == Start::Ask && self.lists_folder(&name) {
                    return Some(Conflict::RemoteFolderExists {
                        local: conflict_local(),
                    });
                }
                self.upload_folder(client, local, name, start, done).await;
            }
            Ok(_) => {
                let partial = self
                    .files
                    .as_deref()
                    .is_some_and(|files| lists_partial_upload(files, &name));
                if start == Start::Ask && partial {
                    return Some(Conflict::PartialUpload {
                        local: conflict_local(),
                    });
                }
                self.upload_file(client, local, name, start, done).await;
            }
            Err(e) => self.push(
                format!("cannot read {}: {e}", local.display()),
                LineKind::Error,
            ),
        }
        None
    }

    /// `true` if the current folder's listing has a folder called `name`.
    fn lists_folder(&self, name: &str) -> bool {
        self.files
            .iter()
            .flatten()
            .any(|f| &*f.name == name && f.is_folder())
    }

    /// [`myTran_UploadFile`] (203) for the local file at `local`.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    async fn upload_file(
        &mut self,
        client: &HotlineClient,
        local: &Path,
        name: String,
        start: Start,
        done: &UnboundedSender<Line>,
    ) {
        let opened = async {
            let file = File::open(local).await?;
            let size = file.metadata().await?.len();
            Ok::<_, std::io::Error>((file, size))
        };
        let (mut file, size) = match opened.await {
            Ok(opened) => opened,
            Err(e) => {
                let text = format!("cannot read {}: {e}", local.display());
                return self.push(text, LineKind::Error);
            }
        };
        let resume = start == Start::Resume;
        let upload = match client
            .upload_file(&self.file_path, &name, size, resume)
            .await
        {
            Ok(upload) => upload,
            Err(e) => return self.push(format!("failed to upload {name}: {e}"), LineKind::Error),
        };
        let offset = upload.offset();
        if let Err(e) = file.seek(SeekFrom::Start(offset.into())).await {
            let text = format!("cannot read {}: {e}", local.display());
            return self.push(text, LineKind::Error);
        }
        let from = from_text(offset);
        self.push(format!("uploading {name}{from}"), LineKind::System);

        let done = done.clone();
        tokio::spawn(async move {
            let line = match upload.send(&mut BufReader::new(file)).await {
                Ok(bytes) => Line::system(format!("uploaded {name} ({bytes} bytes)")),
                Err(e) => Line::error(format!("failed to upload {name}: {e}")),
            };
            let _ = done.send(line);
        });
    }

    /// [`myTran_UploadFldr`] (213) for the local folder at `local`, with
    /// everything inside it.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    async fn upload_folder(
        &mut self,
        client: &HotlineClient,
        local: &Path,
        name: String,
        start: Start,
        done: &UnboundedSender<Line>,
    ) {
        let root = local.to_path_buf();
        let walked = tokio::task::spawn_blocking(move || walk_local(&root))
            .await
            .map_err(std::io::Error::other)
            .and_then(|items| items);
        let items = match walked {
            Ok(items) => items,
            Err(e) => {
                let text = format!("cannot read {}: {e}", local.display());
                return self.push(text, LineKind::Error);
            }
        };
        let item_count = u32::try_from(items.len()).unwrap_or(u32::MAX);
        let total_size = items
            .iter()
            .filter_map(|item| item.file.as_ref())
            .map(|(_, size)| size)
            .sum();
        let resume = start == Start::Resume;
        let upload = match client
            .upload_folder(&self.file_path, &name, item_count, total_size, resume)
            .await
        {
            Ok(upload) => upload,
            Err(e) => return self.push(format!("failed to upload {name}: {e}"), LineKind::Error),
        };
        self.push(
            format!("uploading {name} ({item_count} items)"),
            LineKind::System,
        );

        let done = done.clone();
        tokio::spawn(async move {
            let line = match send_folder(upload, items).await {
                Ok((files, bytes)) => {
                    Line::system(format!("uploaded {name} ({files} files, {bytes} bytes)"))
                }
                Err(e) => Line::error(format!("failed to upload {name}: {e}")),
            };
            let _ = done.send(line);
        });
    }
}

/// Save each item of `download` under the local folder `root`. With
/// [`Start::Resume`], a file that is there is skipped and a `.hpf` file is
/// continued. The first item clears `queued`. Returns the number of files
/// and bytes saved.
async fn receive_folder(
    download: FolderDownload,
    root: &Path,
    start: Start,
    queued: &AtomicBool,
) -> anyhow::Result<(u32, u64)> {
    let mut receiver = download.open().await?;
    let (mut files, mut bytes) = (0, 0);
    while let Some(item) = receiver.next_item().await? {
        queued.store(false, Ordering::Relaxed);
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
        bytes += match offset {
            0 => receiver.receive_file(&mut out).await?,
            offset => receiver.resume_file(&mut out, offset).await?,
        };
        tokio::fs::rename(&partial, &local).await?;
        files += 1;
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

/// Send `items` on `upload`. Returns the number of files and bytes sent.
async fn send_folder(upload: FolderUpload, items: Vec<LocalItem>) -> anyhow::Result<(u32, u64)> {
    let Some(mut sender) = upload.open().await? else {
        return Ok((0, 0));
    };
    let (mut files, mut bytes) = (0, 0);
    for item in items {
        let header = FolderItemHeader {
            is_folder: item.file.is_none(),
            path: HlPath::new(item.path),
        };
        let offset = sender.send_item(&header).await?;
        if let (Some(offset), Some((local, size))) = (offset, item.file) {
            let mut file = File::open(&local).await?;
            file.seek(SeekFrom::Start(offset)).await?;
            let mut data = BufReader::new(file);
            bytes += sender
                .send_file(&mut data, size.saturating_sub(offset))
                .await?;
            files += 1;
        }
    }
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
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());
        session.download_dir = dir.to_path_buf();
        (session, client)
    }

    /// The next line a transfer sends when it ends.
    async fn transfer_done(done: &mut tokio::sync::mpsc::UnboundedReceiver<Line>) -> Line {
        tokio::time::timeout(test_support::WAIT, done.recv())
            .await
            .expect("transfer ends")
            .expect("channel open")
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
            .await;
        assert!(matches!(conflict, Some(Conflict::LocalFileExists { .. })));

        let conflict = session
            .download_file(&client, "a.txt", "b.txt", Start::Ask, &tx)
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
            .await;
        assert_eq!(
            conflict,
            Some(Conflict::LocalFolderExists {
                name: "Games".into()
            })
        );

        let conflict = session
            .download_folder(&client, "Games", Start::Resume, &tx)
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
    async fn upload_asks_before_it_resumes_a_listed_partial_upload() {
        let files = hotline_server::InMemoryFileProvider::default();
        let uploads = HlPath::new(["Uploads"]);
        files.seed(&uploads, "a.txt.hpf", *b"hel");
        let dir = tempfile::tempdir().unwrap();
        let local = dir.path().join("a.txt");
        std::fs::write(&local, b"hello").unwrap();
        let (mut session, client) = file_session(files, dir.path()).await;
        session.get_file_name_list(&client, uploads.clone()).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

        let conflict = session.upload(&client, &local, Start::Ask, &tx).await;
        assert!(matches!(conflict, Some(Conflict::PartialUpload { .. })));

        let conflict = session.upload(&client, &local, Start::Resume, &tx).await;
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
