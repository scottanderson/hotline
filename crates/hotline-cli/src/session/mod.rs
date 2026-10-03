//! [`ClientSession`] state: what the client knows about the connection,
//! decoupled from the terminal. Data plus pure update logic.

mod accounts;
mod chat;
mod chat_room;
mod downloads;
mod event_outcome;
mod file_ops;
mod keep_alive;
mod mode;
#[cfg(test)]
mod test_support;
mod threaded_news;

use std::collections::VecDeque;
use std::io::{ErrorKind, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use hotline_client::{
    Ban, ClientInfoText, ClientUserInfo, FolderDownload, FolderUpload, HotlineClient, UserAccount,
};
use hotline_proto::htxf::FolderItemHeader;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::EncodingHandle;
use hotline_proto::types::{FileNameWithInfo, HlPath, UserChangeOptions, UserNameWithInfo};
use hotline_proto::FieldId;
use hotline_proto::{Access, Field, Transaction};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, BufReader, BufWriter};
use tokio::sync::mpsc::UnboundedSender;

use crate::line::{Line, LineKind};
use crate::transfer::{Conflict, Start};

pub use chat_room::{ChatInvite, ChatRoom};
use downloads::{FirstWrite, RunningDownload};
pub use event_outcome::EventOutcome;
pub use keep_alive::KEEP_ALIVE;
pub use mode::Mode;
pub use threaded_news::NewsItem;

pub struct ClientSession {
    pub nick: Box<str>,
    pub icon: u16,
    pub options: UserChangeOptions,
    /// The reply to a private message while [`UserChangeOptions::AUTO_RESPONSE`] is on.
    pub automatic_response: Box<str>,
    pub mode: Mode,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Vec<Line>,
    /// The classic news, `None` until it is fetched.
    pub news: Option<String>,
    /// The listing of the folder at `file_path`, `None` until it is fetched.
    pub files: Option<Vec<FileNameWithInfo>>,
    pub file_path: HlPath,
    /// The folder that downloads are saved in.
    pub download_dir: PathBuf,
    pub access: Access,
    /// The server's version from the login reply.
    pub server_version: Option<u16>,
    /// The accounts on the Admin tab, `None` until they are fetched.
    pub accounts: Option<Vec<UserAccount>>,
    /// The threaded news bundle or category on the Threaded News tab.
    pub threaded_path: HlPath,
    /// `true` if `threaded_path` is a category, which holds articles.
    pub threaded_in_category: bool,
    /// The items at `threaded_path`, `None` until they are fetched.
    pub threaded: Option<Vec<NewsItem>>,
    /// The private chat rooms this client is in, oldest first.
    pub rooms: Vec<ChatRoom>,
    /// Invitations that wait for an answer, oldest first.
    pub invites: VecDeque<ChatInvite>,
    /// The downloads that run, oldest first.
    downloads: Vec<RunningDownload>,
    /// The ID of the last download kept in `downloads`.
    next_download_id: u64,
    /// This connection's active text encoding, shared with the client's codec.
    encoding: EncodingHandle,
}

impl ClientSession {
    pub fn new(nick: String, icon: u16, encoding: EncodingHandle) -> Self {
        Self {
            nick: nick.into(),
            icon,
            options: UserChangeOptions::empty(),
            automatic_response: Box::default(),
            mode: Mode::Joining,
            users: Vec::new(),
            lines: vec![Line {
                text: "connected - type a message and press Enter, Esc to quit".into(),
                kind: LineKind::System,
            }],
            news: None,
            files: None,
            file_path: HlPath::default(),
            download_dir: PathBuf::from("."),
            access: Access::empty(),
            server_version: None,
            accounts: None,
            threaded_path: HlPath::default(),
            threaded_in_category: false,
            threaded: None,
            rooms: Vec::new(),
            invites: VecDeque::new(),
            downloads: Vec::new(),
            next_download_id: 0,
            encoding,
        }
    }

    /// Show the server's agreement text and wait for the user to accept it.
    pub fn show_agreement(&mut self, text: String) {
        self.mode = Mode::Agreement(text);
    }

    /// Accept the agreement, [`myTran_Agreed`] (121), and switch to chat.
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
    pub async fn accept_agreement(&mut self, client: &HotlineClient) {
        if let Err(e) = client.agree(&self.nick, self.icon).await {
            self.push(format!("failed to complete login: {e}"), LineKind::Error);
        }
        match client.get_user_name_list().await {
            Ok(users) => self.users = users,
            Err(e) => self.push(format!("failed to fetch user list: {e}"), LineKind::Error),
        }
        self.mode = Mode::Chat;
    }

    fn push(&mut self, text: impl Into<Box<str>>, kind: LineKind) {
        self.lines.push(Line {
            text: text.into(),
            kind,
        });
    }

    /// Show the error text of a reply that no request waits for.
    fn show_unmatched_error(&mut self, tx: &Transaction) {
        if tx.is_error() {
            let text = tx
                .string(FieldId::ErrorText, self.encoding.get())
                .unwrap_or_else(|| "unknown error".to_string());
            self.push(text, LineKind::Error);
        }
    }

    /// Push one line for each `\r` in `text`.
    fn push_lines(&mut self, text: &str, kind: LineKind) {
        for line in text.split('\r') {
            self.push(line, kind);
        }
    }

    /// Apply one server-initiated transaction (chat relay, user list
    /// change, permission denial, ...) to the session state.
    pub fn apply_server_event(&mut self, tx: &Transaction) -> EventOutcome {
        let TransactionType::Push(opcode) = tx.r#type() else {
            self.show_unmatched_error(tx);
            return EventOutcome::Applied;
        };
        match opcode {
            /* 102 */ Opcode::NewMsg => self.apply_new_msg(tx),
            /* 104 */ Opcode::ServerMsg => self.apply_server_msg(tx),
            /* 106 */ Opcode::ChatMsg => self.apply_chat_msg(tx),
            /* 109 */ Opcode::ShowAgreement => return self.apply_show_agreement(tx),
            /* 111 */ Opcode::DisconnectMsg => return self.apply_disconnect_msg(tx),
            /* 113 */ Opcode::InviteToChat => self.apply_invite_to_chat(tx),
            /* 117 */ Opcode::NotifyChatChangeUser => self.apply_notify_chat_change_user(tx),
            /* 118 */ Opcode::NotifyChatDeleteUser => self.apply_notify_chat_delete_user(tx),
            /* 119 */ Opcode::NotifyChatSubject => self.apply_notify_chat_subject(tx),
            /* 122 */ Opcode::ServerBanner => self.apply_server_banner(tx),
            /* 211 */ Opcode::DownloadInfo => self.apply_download_info(tx),
            /* 301 */ Opcode::NotifyChangeUser => self.apply_notify_change_user(tx),
            /* 302 */ Opcode::NotifyDeleteUser => self.apply_notify_delete_user(tx),
            /* 354 */ Opcode::UserAccess => self.apply_user_access(tx),
            /* 355 */ Opcode::UserBroadcast => self.apply_user_broadcast(tx),
            _ => {
                for line in format!("Unrecognized opcode for transaction: {tx:#?}").lines() {
                    self.push(line, LineKind::Error);
                }
            }
        }
        EventOutcome::Applied
    }

    /// [`myTran_ShowAgreement`] (109): show the text and wait for acceptance.
    /// With no text, the caller accepts right away.
    ///
    /// [`myTran_ShowAgreement`]: hotline_proto::Opcode::ShowAgreement
    fn apply_show_agreement(&mut self, tx: &Transaction) -> EventOutcome {
        let encoding = self.encoding.get();
        match tx.string(FieldId::Data, encoding) {
            Some(text) => {
                self.show_agreement(text);
                EventOutcome::Applied
            }
            None => EventOutcome::NeedsAgreementAccept,
        }
    }

    /// [`myTran_ServerMsg`] (104).
    ///
    /// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
    fn apply_server_msg(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let text = tx.string(FieldId::Data, encoding);
        let from = tx.string(FieldId::UserName, encoding);
        match (from, text) {
            // A private message carries the sender's name; a
            // permission-denied or admin broadcast notice
            // doesn't.
            (Some(from), Some(text)) => {
                self.push_lines(
                    &format!("[PM] {from}: {}", display_text(&text)),
                    LineKind::Chat,
                );
            }
            (None, Some(text)) => {
                self.push_lines(&display_text(&text), LineKind::Error);
            }
            _ => {}
        }
    }

    /// [`myTran_NewMsg`] (102): a new classic news post.
    ///
    /// [`myTran_NewMsg`]: hotline_proto::Opcode::NewMsg
    fn apply_new_msg(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        if let Some(entry) = tx.string(FieldId::Data, encoding) {
            if let Some(news) = &mut self.news {
                news.insert_str(0, &entry);
            }
            self.push("new news post", LineKind::System);
        }
    }

    /// [`myTran_ChatMsg`] (106).
    ///
    /// [`myTran_ChatMsg`]: hotline_proto::Opcode::ChatMsg
    fn apply_chat_msg(&mut self, tx: &Transaction) {
        if self.room_chat_msg(tx) {
            return;
        }
        let encoding = self.encoding.get();
        if let Some(text) = tx.string(FieldId::Data, encoding) {
            self.push_lines(&display_text(&text), LineKind::Chat);
        }
    }

    /// [`myTran_DisconnectMsg`] (111).
    ///
    /// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
    fn apply_disconnect_msg(&mut self, tx: &Transaction) -> EventOutcome {
        let encoding = self.encoding.get();
        let reason = tx
            .string(FieldId::Data, encoding)
            .unwrap_or_else(|| "disconnected by the server".to_string());
        self.push(display_text(&reason), LineKind::Error);
        EventOutcome::Quit
    }

    /// [`myTran_ServerBanner`] (122): show the banner's link, if it has one.
    /// A banner image is not shown.
    ///
    /// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
    fn apply_server_banner(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        if let Some(url) = tx.string(FieldId::ServerBannerUrl, encoding) {
            self.push(format!("server banner: {url}"), LineKind::System);
        }
    }

    /// [`myTran_DownloadInfo`] (211): a queued download's place in the queue.
    ///
    /// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
    fn apply_download_info(&mut self, tx: &Transaction) {
        let waiting = tx
            .field(FieldId::WaitingCount)
            .and_then(Field::as_u32)
            .unwrap_or_default();
        let name = tx
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .and_then(|reference| self.set_queue_place(reference, waiting));
        let name = name.as_deref().unwrap_or("a download");
        self.push(
            format!("{name} waits, {waiting} ahead in the queue"),
            LineKind::System,
        );
    }

    /// [`myTran_NotifyChangeUser`] (301).
    ///
    /// [`myTran_NotifyChangeUser`]: hotline_proto::Opcode::NotifyChangeUser
    fn apply_notify_change_user(&mut self, tx: &Transaction) {
        let Some(user) = tx.user_entry(self.encoding.get()) else {
            return;
        };
        match self.users.iter().position(|u| u.user_id == user.user_id) {
            Some(pos) => {
                let (old_name, name) = (self.users[pos].name.clone(), &user.name);
                if !old_name.is_empty() && !name.is_empty() && old_name != *name {
                    self.push(
                        format!(" <<< {old_name} is now known as {name} >>>"),
                        LineKind::System,
                    );
                }
                self.users[pos] = user;
            }
            None => {
                self.push(format!("{} joined", user.name), LineKind::System);
                self.users.push(user);
            }
        }
    }

    /// [`myTran_NotifyDeleteUser`] (302).
    ///
    /// [`myTran_NotifyDeleteUser`]: hotline_proto::Opcode::NotifyDeleteUser
    fn apply_notify_delete_user(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return;
        };
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
        for room in &mut self.rooms {
            room.users.retain(|u| u.user_id != user_id);
        }
    }

    /// [`myTran_UserAccess`] (354).
    ///
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
    fn apply_user_access(&mut self, tx: &Transaction) {
        self.access = tx.access().unwrap_or(self.access);

        // This session's own uid/icon/name.
        let encoding = self.encoding.get();
        if let Some(result) = tx.user_list_entry(encoding) {
            match result {
                Ok(entry) => match self.users.iter_mut().find(|u| u.user_id == entry.user_id) {
                    Some(existing) => *existing = entry,
                    None => self.users.push(entry),
                },
                Err(e) => self.push(
                    format!("failed to parse own user info: {e}"),
                    LineKind::Error,
                ),
            }
        }
    }

    /// [`myTran_UserBroadcast`] (355): a message from an administrator to every user.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    fn apply_user_broadcast(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let Some(text) = tx.string(FieldId::Data, encoding) else {
            return;
        };
        let from = match tx.string(FieldId::UserName, encoding) {
            Some(name) => format!(" from {name}"),
            None => String::new(),
        };
        self.push_lines(
            &format!("[Administrator Message{from}] {}", display_text(&text)),
            LineKind::Chat,
        );
    }

    /// `true` if the user may send a broadcast.
    pub const fn can_broadcast(&self) -> bool {
        self.access.contains(Access::BROADCAST)
    }

    /// [`myTran_UserBroadcast`] (355), showing an error line if it fails.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    pub async fn user_broadcast(&mut self, client: &HotlineClient, text: &str) {
        if let Err(e) = client.user_broadcast(&wire_text(text)).await {
            self.push(format!("failed to broadcast: {e}"), LineKind::Error);
        }
    }

    /// Send `line` as chat, reporting a failed send as a local error line
    /// (a transport error may never come back as a server push).
    pub async fn send_chat(&mut self, client: &HotlineClient, line: &str) {
        if let Err(e) = client.send_chat(&wire_text(line)).await {
            self.push(format!("failed to send: {e}"), LineKind::Error);
        }
    }

    /// [`myTran_GetClientInfoText`] (303), showing an error line if it fails.
    ///
    /// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
    pub async fn get_client_info_text(
        &mut self,
        client: &HotlineClient,
        user_id: u16,
    ) -> Option<ClientInfoText> {
        client
            .get_client_info_text(user_id)
            .await
            .inspect_err(|e| self.push(format!("failed to get info: {e}"), LineKind::Error))
            .ok()
    }

    pub async fn get_msgs(&mut self, client: &HotlineClient) {
        match client.get_msgs().await {
            Ok(news) => self.news = Some(news),
            Err(e) => self.push(format!("failed to get news: {e}"), LineKind::Error),
        }
    }

    /// [`myTran_GetFileNameList`] (200) for `path`. On success, `path` becomes
    /// the current folder and this returns `true`.
    ///
    /// [`myTran_GetFileNameList`]: hotline_proto::Opcode::GetFileNameList
    pub async fn get_file_name_list(&mut self, client: &HotlineClient, path: HlPath) -> bool {
        match client.get_file_name_list(&path).await {
            Ok(files) => {
                self.files = Some(files);
                self.file_path = path;
                true
            }
            Err(e) => {
                self.push(format!("failed to list files: {e}"), LineKind::Error);
                false
            }
        }
    }

    /// Start to download the file `name` in the current folder into
    /// `download_dir` as `save_as`, [`myTran_DownloadFile`] (202). The data
    /// goes into `save_as.hpf`, which becomes `save_as` when the download
    /// ends. The transfer runs on its own task and sends its result line to
    /// `done`. With [`Start::New`], a local `save_as` or `save_as.hpf` stops
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
        if start == Start::New {
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
        let queue = match download.waiting_count {
            0 => String::new(),
            n => format!(", {n} ahead in the queue"),
        };
        let from = match offset {
            0 => String::new(),
            n => format!(" from byte {n}"),
        };
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
                Ok(bytes) => Line {
                    text: format!("downloaded {name} ({bytes} bytes) to {}", local.display())
                        .into(),
                    kind: LineKind::System,
                },
                Err(e) => Line {
                    text: format!("failed to download {name}: {e}").into(),
                    kind: LineKind::Error,
                },
            };
            let _ = done.send(line);
        });
        self.track_download(name, reference, queued, task.abort_handle());
        None
    }

    /// Start to download the folder `name` in the current folder, with
    /// everything inside it, into `download_dir`, [`myTran_DownloadFldr`] (210).
    /// The transfer runs on its own task and sends its result line to `done`.
    /// With [`Start::New`], an existing local folder stops the download and
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
            Start::New => tokio::fs::create_dir(&root).await,
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
                if start == Start::New {
                    let _ = tokio::fs::remove_dir(&root).await;
                }
                self.push(format!("failed to download {name}: {e}"), LineKind::Error);
                return None;
            }
        };
        let queue = match download.waiting_count {
            0 => String::new(),
            n => format!(", {n} ahead in the queue"),
        };
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
                Ok((files, bytes)) => Line {
                    text: format!(
                        "downloaded {name} ({files} files, {bytes} bytes) to {}",
                        root.display()
                    )
                    .into(),
                    kind: LineKind::System,
                },
                Err(e) => Line {
                    text: format!("failed to download {name}: {e}").into(),
                    kind: LineKind::Error,
                },
            };
            let _ = done.send(line);
        });
        self.track_download(name, reference, queued, task.abort_handle());
        None
    }

    /// Start to upload the local file or folder at `local` to the current
    /// folder under its own name. The transfer runs on its own task and sends
    /// its result line to `done`. With [`Start::New`], a partial upload or a
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
                if start == Start::New && self.lists(&name, true) {
                    return Some(Conflict::RemoteFolderExists {
                        local: conflict_local(),
                    });
                }
                self.upload_folder(client, local, name, start, done).await;
            }
            Ok(_) => {
                if start == Start::New && self.lists(&format!("{name}.hpf"), false) {
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

    /// `true` if the current folder's listing has a file, or a folder if
    /// `is_folder`, called `name`.
    fn lists(&self, name: &str, is_folder: bool) -> bool {
        self.files
            .iter()
            .flatten()
            .any(|f| &*f.name == name && f.is_folder() == is_folder)
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
        let from = match offset {
            0 => String::new(),
            n => format!(" from byte {n}"),
        };
        self.push(format!("uploading {name}{from}"), LineKind::System);

        let done = done.clone();
        tokio::spawn(async move {
            let line = match upload.send(&mut BufReader::new(file)).await {
                Ok(bytes) => Line {
                    text: format!("uploaded {name} ({bytes} bytes)").into(),
                    kind: LineKind::System,
                },
                Err(e) => Line {
                    text: format!("failed to upload {name}: {e}").into(),
                    kind: LineKind::Error,
                },
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
        let items = match tokio::task::spawn_blocking(move || walk_local(&root)).await {
            Ok(Ok(items)) => items,
            Ok(Err(e)) => {
                let text = format!("cannot read {}: {e}", local.display());
                return self.push(text, LineKind::Error);
            }
            Err(e) => {
                return self.push(
                    format!("cannot read {}: {e}", local.display()),
                    LineKind::Error,
                )
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
                Ok((files, bytes)) => Line {
                    text: format!("uploaded {name} ({files} files, {bytes} bytes)").into(),
                    kind: LineKind::System,
                },
                Err(e) => Line {
                    text: format!("failed to upload {name}: {e}").into(),
                    kind: LineKind::Error,
                },
            };
            let _ = done.send(line);
        });
    }

    /// The path of the folder `name` in the current folder.
    pub fn child_folder(&self, name: &str) -> HlPath {
        let mut path = self.file_path.clone();
        path.components.push(name.to_string());
        path
    }

    /// The path of the current folder's parent, `None` at the root.
    pub fn parent_folder(&self) -> Option<HlPath> {
        let mut path = self.file_path.clone();
        path.components.pop()?;
        Some(path)
    }

    pub async fn old_post_news(&mut self, client: &HotlineClient, text: &str) {
        if let Err(e) = client.old_post_news(&wire_text(text)).await {
            self.push(format!("failed to post news: {e}"), LineKind::Error);
        }
    }

    /// This user's name, icon, and options.
    pub fn user_info(&self) -> ClientUserInfo {
        ClientUserInfo {
            name: self.nick.clone(),
            icon: self.icon,
            options: self.options,
            automatic_response: self.automatic_response.clone(),
        }
    }

    /// Change this user's name, icon, and options,
    /// [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub async fn set_client_user_info(&mut self, client: &HotlineClient, info: ClientUserInfo) {
        if let Err(e) = client.set_client_user_info(&info).await {
            return self.push(format!("failed to change options: {e}"), LineKind::Error);
        }
        self.nick = info.name;
        self.icon = info.icon;
        self.options = info.options;
        self.automatic_response = info.automatic_response;
    }

    /// [`myTran_DisconnectUser`] (110), showing an error line if it fails.
    ///
    /// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
    pub async fn disconnect_user(&mut self, client: &HotlineClient, user_id: u16, ban: Ban) {
        let name = match self.users.iter().find(|u| u.user_id == user_id) {
            Some(user) => user.name.clone(),
            None => format!("#{user_id}").into(),
        };
        if let Err(e) = client.disconnect_user(user_id, &name, ban).await {
            self.push(format!("failed to disconnect {name}: {e}"), LineKind::Error);
        }
    }

    /// `true` if the user may disconnect users.
    pub const fn can_disconnect(&self) -> bool {
        self.access.contains(Access::DISCON_USER)
    }

    pub async fn send_instant_msg(&mut self, client: &HotlineClient, user_id: u16, text: &str) {
        let text = wire_text(text);
        match client.send_instant_msg(user_id, &text).await {
            Ok(()) => {
                let to = match self.users.iter().find(|u| u.user_id == user_id) {
                    Some(user) => user.name.to_string(),
                    None => format!("#{user_id}"),
                };
                self.push_lines(&format!("[PM to {to}] {text}"), LineKind::Chat);
            }
            Err(e) => self.push(format!("failed to send: {e}"), LineKind::Error),
        }
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
/// it and also returns its size; [`Start::New`] creates it.
async fn open_partial(partial: &Path, start: Start) -> std::io::Result<(File, u32)> {
    let mut options = OpenOptions::new();
    match start {
        Start::Resume => options.append(true),
        Start::New => options.write(true).create_new(true),
        Start::Replace => options.write(true).create(true).truncate(true),
    };
    let file = options.open(partial).await?;
    let offset = match start {
        Start::Resume => u32::try_from(file.metadata().await?.len())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "the file is too large"))?,
        Start::New | Start::Replace => 0,
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

/// `text` with each line break as `\r`, the Hotline line separator.
fn wire_text(text: &str) -> String {
    text.replace('\n', "\r")
}

/// Strip a leading `\r`/`\n` some servers prepend to chat/status text;
/// each `Line` here is already its own row.
fn display_text(s: &str) -> String {
    s.trim_start_matches(['\r', '\n']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::types::UserFlags;

    #[test]
    fn new_msg_adds_the_post_to_the_top_of_the_news() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        session.news = Some("From bob\rold".to_string());
        let post = Transaction::push(
            Opcode::NewMsg,
            vec![Field::string(FieldId::Data, "From alice\rhello")],
        );
        let _ = session.apply_server_event(&post);
        assert_eq!(
            session.news.as_deref(),
            Some("From alice\rhelloFrom bob\rold")
        );
        assert_eq!(&*session.lines.last().unwrap().text, "new news post");
    }

    #[test]
    fn child_and_parent_folder_walk_the_path() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        assert_eq!(session.parent_folder(), None);
        session.file_path = session.child_folder("Games");
        assert_eq!(session.child_folder("Doom"), HlPath::new(["Games", "Doom"]));
        assert_eq!(session.parent_folder(), Some(HlPath::default()));
    }

    #[test]
    fn server_banner_shows_its_url() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let banner = Transaction::push(
            Opcode::ServerBanner,
            vec![
                Field::bytes(FieldId::ServerBannerType, b"URL ".to_vec()),
                Field::string(FieldId::ServerBannerUrl, "http://example.com"),
            ],
        );
        let _ = session.apply_server_event(&banner);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "server banner: http://example.com"
        );
    }

    #[test]
    fn show_unmatched_error_shows_the_error_text() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let invite = Transaction::request(Opcode::InviteToChat, 7, vec![]);
        let _ = session.apply_server_event(&invite.reply_error("no such user"));
        assert_eq!(&*session.lines.last().unwrap().text, "no such user");
        assert_eq!(session.lines.last().unwrap().kind, LineKind::Error);
    }

    #[test]
    fn server_banner_ignores_an_image_banner() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let before = session.lines.len();
        let banner = Transaction::push(
            Opcode::ServerBanner,
            vec![Field::bytes(FieldId::ServerBannerType, b"JPEG".to_vec())],
        );
        let _ = session.apply_server_event(&banner);
        assert_eq!(session.lines.len(), before);
    }

    #[test]
    fn user_broadcast_names_the_sender() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let broadcast = Transaction::push(
            Opcode::UserBroadcast,
            vec![
                Field::u16(FieldId::UserID, 1),
                Field::string(FieldId::Data, "going down"),
                Field::string(FieldId::UserName, "root"),
            ],
        );
        let _ = session.apply_server_event(&broadcast);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "[Administrator Message from root] going down"
        );
    }

    #[test]
    fn download_info_shows_the_place_in_the_queue() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let info = Transaction::push(
            Opcode::DownloadInfo,
            vec![
                Field::u32(FieldId::RefNum, 9),
                Field::u32(FieldId::WaitingCount, 2),
            ],
        );
        let _ = session.apply_server_event(&info);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "a download waits, 2 ahead in the queue"
        );
    }

    #[tokio::test]
    async fn download_info_marks_its_download_queued() {
        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let task = tokio::spawn(std::future::pending::<()>());
        session.track_download(
            "doom.wad",
            Some(9),
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            task.abort_handle(),
        );
        let info = Transaction::push(
            Opcode::DownloadInfo,
            vec![
                Field::u32(FieldId::RefNum, 9),
                Field::u32(FieldId::WaitingCount, 2),
            ],
        );
        let _ = session.apply_server_event(&info);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "doom.wad waits, 2 ahead in the queue"
        );
        assert_eq!(&*session.running_downloads()[0].1, "doom.wad (queued)");
        task.abort();
    }

    #[test]
    fn walk_local_lists_each_folder_before_its_items() {
        let root = std::env::temp_dir().join(format!("hlc-walk-{}", std::process::id()));
        std::fs::create_dir_all(root.join("Sub")).unwrap();
        std::fs::write(root.join("a.txt"), b"alpha").unwrap();
        std::fs::write(root.join("Sub").join("b.txt"), b"be").unwrap();

        let items = walk_local(&root);
        std::fs::remove_dir_all(&root).unwrap();

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
            .download_file(&client, "a.txt", "a.txt", Start::New, &tx)
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
            .download_file(&client, "a.txt", "a.txt", Start::New, &tx)
            .await;
        assert!(matches!(conflict, Some(Conflict::LocalFileExists { .. })));

        let conflict = session
            .download_file(&client, "a.txt", "b.txt", Start::New, &tx)
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
            .download_folder(&client, "Games", Start::New, &tx)
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

        let conflict = session.upload(&client, &local, Start::New, &tx).await;
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

    #[tokio::test]
    async fn set_client_user_info_keeps_the_new_name_and_options() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, mut events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());
        let info = ClientUserInfo {
            name: "you".into(),
            icon: 3,
            options: UserChangeOptions::REFUSE_CHAT,
            automatic_response: Box::default(),
        };

        session.set_client_user_info(&client, info.clone()).await;
        assert_eq!(session.user_info(), info);
        let change = test_support::next_push(&mut events, Opcode::NotifyChangeUser).await;
        let user = change.user_entry(client.encoding().get()).unwrap();
        assert_eq!((&*user.name, user.icon), ("you", 3));
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

    #[test]
    fn wire_text_ends_each_line_with_cr() {
        assert_eq!(wire_text("one\ntwo"), "one\rtwo");
    }

    #[test]
    fn chat_msg_shows_each_line_on_its_own() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let chat = Transaction::push(
            Opcode::ChatMsg,
            vec![Field::string(FieldId::Data, "\rAlice: one\rtwo")],
        );
        let _ = session.apply_server_event(&chat);
        let texts: Vec<&str> = session.lines.iter().map(|l| &*l.text).collect();
        assert!(texts.ends_with(&["Alice: one", "two"]));
    }

    #[test]
    fn display_text_strips_leading_cr() {
        assert_eq!(display_text("\rAlice: hi"), "Alice: hi");
    }

    #[test]
    fn display_text_leaves_normal_text_alone() {
        assert_eq!(display_text("Alice: hi"), "Alice: hi");
    }

    /// [`myTran_InviteToChat`] (113) is both the client's invite request
    /// and the server's resulting push; a value-based classification
    /// would misread this push and silently drop it here.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    #[test]
    fn chat_invite_push_is_recognized_despite_sharing_its_wire_value_with_a_client_opcode() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let invite = Transaction::push(
            Opcode::InviteToChat,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u32(FieldId::ChatID, 7),
            ],
        );
        let _ = session.apply_server_event(&invite);
        assert!(session
            .lines
            .iter()
            .any(|line| line.text.contains("Alice invited you to a private chat")));
        assert_eq!(
            session.invites.front(),
            Some(&ChatInvite {
                chat_id: 7,
                from: "Alice".into()
            })
        );
    }

    #[test]
    fn notify_change_user_carries_admin_color_for_new_and_existing_users() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let admin_change = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, UserFlags::ADMIN.bits()),
            ],
        );

        // New user: flags should carry the admin bit from the start.
        let _ = session.apply_server_event(&admin_change);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(user.flags.contains(UserFlags::ADMIN));

        // Existing user: a later update should still reflect current flags.
        let demoted = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&demoted);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(!user.flags.contains(UserFlags::ADMIN));
    }

    #[test]
    fn notify_change_user_notes_a_rename() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let joined = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&joined);

        let renamed = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alicia"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&renamed);

        assert_eq!(
            &*session.lines.last().unwrap().text,
            " <<< Alice is now known as Alicia >>>"
        );
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert_eq!(&*user.name, "Alicia");
    }
}
