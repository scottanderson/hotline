use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tokio_util::sync::CancellationToken;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::types::{
    FileNameWithInfo, HlPath, NewsArticleListData, NewsArticleSummary, NewsCategoryListEntry,
};
use hotline_proto::{Field, FieldId, Transaction};

use crate::account::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
use crate::ban::{BanProvider, BanRecord, InMemoryBanProvider, TEMP_BAN_DURATION};
use crate::banner::{BannerKind, BannerProvider, NoBanner};
use crate::console::fields::{
    format_access, format_access_extra, parse_0wn_fields, parse_access_fields,
};
use crate::console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply, DefaultChatCommands};
use crate::file::{
    BoxedReader, BoxedWriter, FileProvider, FileRequester, FolderItem, InMemoryFileProvider,
};
use crate::flood::{FloodDecision, FloodEvent, FloodPolicy, NoFloodPolicy};
use crate::news::{
    FlatNewsProvider, InMemoryNewsProvider, NewsArticleDetail, NewsItemKind, NewsListKind,
    ThreadedNewsProvider,
};
use crate::providers::Providers;
use crate::server::{DEFAULT_MAX_DOWNLOADS, DEFAULT_MAX_DOWNLOADS_PER_CLIENT};
use crate::state::download_queue::{DownloadQueue, Limits, QueueUpdate};
use crate::state::{
    dropbox_access, dropbox_blocked, is_upload_folder, AccessUpdate, ChatRoom, ConsoleUpdate,
    PendingTransfer, RecipientMessage, TransferDirection, TransferKind, TransferRef, UserEntry,
};

/// How long an active download or an upload may wait for its `HTXF`
/// connection, openline's 500 s.
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(500);

/// State shared by every [`crate::Session`], behind cheap `Arc` clones.
#[derive(Clone)]
pub struct SharedState {
    users: Arc<RwLock<HashMap<u16, UserEntry>>>,
    next_user_id: Arc<AtomicU16>,
    /// One inbound channel per currently connected recipient.
    recipients: Arc<RwLock<HashMap<u16, mpsc::Sender<RecipientMessage>>>>,
    /// File storage backing the file listing, [`myTran_DownloadFile`] (202),
    /// and [`myTran_UploadFile`] (203); see [`crate::FileProvider`].
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    files: Arc<dyn FileProvider>,
    /// Classic news storage, see [`crate::FlatNewsProvider`].
    flat_news: Arc<dyn FlatNewsProvider>,
    /// Threaded news storage, see [`crate::ThreadedNewsProvider`].
    threaded_news: Arc<dyn ThreadedNewsProvider>,
    /// Chat ID `0` is always present: the main room every connected
    /// user is implicitly in. Other entries are private rooms.
    chat_rooms: Arc<RwLock<HashMap<u32, ChatRoom>>>,
    next_chat_id: Arc<AtomicU32>,
    /// Account storage; see [`crate::AccountProvider`].
    accounts: Arc<dyn AccountProvider>,
    /// Ban storage; see [`crate::BanProvider`].
    bans: Arc<dyn BanProvider>,
    /// Flood/spam policy; see [`crate::FloodPolicy`].
    flood: Arc<dyn FloodPolicy>,
    /// Banner storage; see [`crate::BannerProvider`].
    banner: Arc<dyn BannerProvider>,
    /// Transfers awaiting their `HTXF` data connection, keyed by the
    /// reference number handed to the client.
    transfers: Arc<RwLock<HashMap<TransferRef, PendingTransfer>>>,
    next_transfer_ref: Arc<AtomicU32>,
    /// Concurrent-download admission; see
    /// [`download_queue::DownloadQueue`](crate::state::download_queue::DownloadQueue).
    download_queue: Arc<DownloadQueue>,
    /// Console command dispatch; see [`crate::ChatCommandDispatcher`].
    commands: Arc<dyn ChatCommandDispatcher>,
}

impl Default for SharedState {
    fn default() -> Self {
        let news = Arc::new(InMemoryNewsProvider::default());
        Self::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(InMemoryFileProvider::default()),
            flat_news: news.clone(),
            threaded_news: news,
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(DefaultChatCommands),
        })
    }
}

impl SharedState {
    /// Build fresh shared state backed by `providers`.
    pub fn new(providers: Providers) -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            next_user_id: Arc::new(AtomicU16::new(1)),
            recipients: Arc::new(RwLock::new(HashMap::new())),
            files: providers.files,
            flat_news: providers.flat_news,
            threaded_news: providers.threaded_news,
            chat_rooms: Arc::new(RwLock::new(HashMap::from([(0, ChatRoom::default())]))),
            next_chat_id: Arc::new(AtomicU32::new(1)),
            accounts: providers.accounts,
            bans: providers.bans,
            flood: providers.flood,
            banner: providers.banner,
            transfers: Arc::new(RwLock::new(HashMap::new())),
            next_transfer_ref: Arc::new(AtomicU32::new(1)),
            download_queue: Arc::new(DownloadQueue::default()),
            commands: providers.commands,
        }
    }

    /// Set the download limits. A `0` falls back to its default.
    pub fn set_download_limits(&self, max_downloads: usize, max_downloads_per_client: usize) {
        let or_default = |value, default| if value == 0 { default } else { value };
        self.download_queue.set_limits(Limits {
            total: or_default(max_downloads, DEFAULT_MAX_DOWNLOADS),
            per_client: or_default(max_downloads_per_client, DEFAULT_MAX_DOWNLOADS_PER_CLIENT),
        });
    }
}

impl SharedState {
    /// Allocate the next free user/socket ID.
    pub fn next_user_id(&self) -> u16 {
        self.next_user_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Register a newly logged-in (or info-changed) user.
    pub async fn insert_user(&self, entry: UserEntry) {
        let user_id = entry.user_id;
        self.users.write().await.insert(user_id, entry);
        self.add_chat_member(0, user_id).await;
    }

    /// Remove a user on disconnect.
    pub async fn remove_user(&self, user_id: u16) {
        self.users.write().await.remove(&user_id);
        self.leave_chat_room(0, user_id).await;
        self.release_downloads_for_user(user_id).await;
    }

    /// Snapshot of all currently connected users, in connection order
    /// (`user_id` is assigned monotonically, so sorting by it avoids the
    /// underlying map's arbitrary iteration order).
    pub async fn users(&self) -> Vec<UserEntry> {
        let mut users: Vec<UserEntry> = self.users.read().await.values().cloned().collect();
        users.sort_by_key(|u| u.user_id);
        users
    }

    /// How many users are currently connected.
    pub async fn user_count(&self) -> usize {
        self.users.read().await.len()
    }

    /// Look up one connected user by ID.
    pub async fn user(&self, user_id: u16) -> Option<UserEntry> {
        self.users.read().await.get(&user_id).cloned()
    }

    /// Whether `user_id` is currently connected, without cloning its entry.
    pub async fn has_user(&self, user_id: u16) -> bool {
        self.users.read().await.contains_key(&user_id)
    }

    /// Every currently connected user ID logged in under `login`, e.g. to
    /// disconnect them after their account is deleted or renamed away.
    pub async fn user_ids_for_login(&self, login: &str) -> Vec<u16> {
        self.users
            .read()
            .await
            .values()
            .filter(|entry| entry.login == login)
            .map(|entry| entry.user_id)
            .collect()
    }

    /// `Err("no such user")` unless `user_id` is currently connected.
    pub async fn require_connected_user(&self, user_id: u16) -> Result<(), &'static str> {
        self.has_user(user_id)
            .await
            .then_some(())
            .ok_or("no such user")
    }

    /// Whether `target_id` may be kicked: `Err` if that user holds
    /// `CANT_BE_DISCONNECTED`. A target that isn't connected at all is
    /// allowed through, since kicking one is already a no-op (`send_to`
    /// silently drops a push to nobody).
    pub async fn authorize_kick(&self, target_id: u16) -> Result<(), &'static str> {
        if self
            .user(target_id)
            .await
            .is_some_and(|u| u.access.contains(Access::CANNOT_BE_DISCON))
        {
            return Err("that user can't be disconnected");
        }
        Ok(())
    }

    /// Record that `user_id` just sent a request, resetting their idle
    /// timer. Called once per received frame in the session loop.
    pub async fn touch_user(&self, user_id: u16) {
        if let Some(entry) = self.users.write().await.get_mut(&user_id) {
            entry.last_active = std::time::Instant::now();
        }
    }

    /// Register a newly connected socket as a recipient, returning its
    /// inbound channel. Called once per connection, before login.
    pub async fn register_recipient(&self, user_id: u16) -> mpsc::Receiver<RecipientMessage> {
        let (tx, rx) = mpsc::channel(256);
        self.recipients.write().await.insert(user_id, tx);
        rx
    }

    /// Drop a recipient's channel on disconnect.
    pub async fn remove_recipient(&self, user_id: u16) {
        self.recipients.write().await.remove(&user_id);
    }

    /// Resolves once `user_id`'s session drops its inbound channel. `None`
    /// if it has already gone.
    pub(crate) async fn session_ended(&self, user_id: u16) -> Option<impl Future<Output = ()>> {
        let sender = self.recipients.read().await.get(&user_id)?.clone();
        Some(async move { sender.closed().await })
    }

    /// How many recipients are registered.
    #[cfg(test)]
    pub(crate) async fn recipient_count(&self) -> usize {
        self.recipients.read().await.len()
    }

    /// Publish a transaction to every session (e.g. a chat message or a
    /// user list change notification). Sessions that are slow/gone just
    /// miss it; delivery is best-effort like the real protocol's chat.
    /// Not privilege-gated here - see `Session::may_receive`,
    /// checked by each recipient using privileges it already holds in
    /// memory rather than a shared-state lookup on this hot path.
    pub async fn publish(&self, tx: Transaction) {
        for sender in self.recipients.read().await.values() {
            let _ = sender.try_send(RecipientMessage::Push(tx.clone()));
        }
    }

    /// Send a transaction to exactly one connected user, unfiltered. A
    /// no-op if that user isn't connected.
    pub async fn send_to(&self, target_user_id: u16, tx: Transaction) {
        if let Some(sender) = self.recipients.read().await.get(&target_user_id) {
            let _ = sender.try_send(RecipientMessage::Push(tx));
        }
    }

    /// Disconnect exactly one connected user with no wire-visible notice.
    /// A no-op if that user isn't connected.
    pub async fn disconnect(&self, target_user_id: u16) {
        if let Some(sender) = self.recipients.read().await.get(&target_user_id) {
            let _ = sender.try_send(RecipientMessage::Disconnect);
        }
    }

    /// Like [`Self::publish`], except `exclude_user_id` never receives it.
    pub async fn announce(&self, exclude_user_id: u16, tx: Transaction) {
        for (&user_id, sender) in self.recipients.read().await.iter() {
            if user_id != exclude_user_id {
                let _ = sender.try_send(RecipientMessage::Push(tx.clone()));
            }
        }
    }

    /// Push a live privilege change for every session logged in under `login`.
    pub async fn push_access_update(&self, login: String, access: Access, name: Option<String>) {
        let update = AccessUpdate {
            login,
            access,
            name,
        };
        for sender in self.recipients.read().await.values() {
            let _ = sender.try_send(RecipientMessage::AccessUpdate(update.clone()));
        }
    }

    /// Send a console-command-driven update to exactly one connected
    /// session. A no-op if that user isn't connected.
    pub async fn send_console_update(&self, target_user_id: u16, update: ConsoleUpdate) {
        if let Some(sender) = self.recipients.read().await.get(&target_user_id) {
            let _ = sender.try_send(RecipientMessage::Console(update));
        }
    }

    /// `target_id`'s [`UserEntry`], if connected.
    async fn console_target(&self, target_id: u32) -> Result<UserEntry, Cow<'static, str>> {
        let user = match u16::try_from(target_id) {
            Ok(user_id) => self.user(user_id).await,
            Err(_) => None,
        };
        let user = user.ok_or_else(|| format!("no such user (uid:{target_id})"))?;
        Ok(user)
    }

    /// `target_id`'s [`UserEntry`], if connected and holding
    /// [`AccessExtra::ACCESS_VOLATILE`].
    async fn access_volatile_target(&self, target_id: u32) -> Result<UserEntry, Cow<'static, str>> {
        let target = self.console_target(target_id).await?;
        if !target.access_extra.contains(AccessExtra::ACCESS_VOLATILE) {
            Err("user cannot be modified")?;
        }
        Ok(target)
    }

    /// `/access <uid>` with no fields: `target_id`'s current privileges
    /// as text. The target must hold [`AccessExtra::ACCESS_VOLATILE`].
    pub async fn describe_access(&self, target_id: u32) -> Result<String, Cow<'static, str>> {
        let target = self.access_volatile_target(target_id).await?;
        Ok(format!(
            "{} (uid {target_id})\rAccess: {}\rAccessExtra: {}",
            target.name,
            format_access(target.access),
            format_access_extra(target.access_extra),
        ))
    }

    /// `/access <uid> field=0|1 ...`: applies each field to `target_id`'s
    /// live privileges, all or nothing, parsed like mhxd's `cmd_access`. A
    /// field is an [`Access`] or [`AccessExtra`] console key. The target
    /// must hold [`AccessExtra::ACCESS_VOLATILE`].
    pub async fn apply_access_fields(
        &self,
        target_id: u32,
        fields: &str,
    ) -> Result<String, Cow<'static, str>> {
        let target = self.access_volatile_target(target_id).await?;

        let mut access = target.access;
        let mut access_extra = target.access_extra;
        parse_access_fields(fields, &mut access, &mut access_extra)?;

        self.send_console_update(
            target.user_id,
            ConsoleUpdate {
                access: Some(access),
                access_extra: Some(access_extra),
                icon: None,
                name: None,
            },
        )
        .await;
        Ok(target.name)
    }

    /// `/0wn <uid> icon=<n> name=<text>`: overwrites `target_id`'s live icon
    /// and display name, parsed like mhxd's `cmd_0wn`. A key matches by
    /// prefix, and `name=` takes the rest of the line. The target must hold
    /// [`AccessExtra::IS_0WN3D`]. `None` if nothing changed.
    pub async fn apply_0wn_fields(
        &self,
        target_id: u32,
        fields: &str,
    ) -> Result<Option<String>, Cow<'static, str>> {
        let target = self.console_target(target_id).await?;
        if !target.access_extra.contains(AccessExtra::IS_0WN3D) {
            Err("User cannot be 0wned")?;
        }

        let (icon, name) = parse_0wn_fields(fields);
        if icon.is_none() && name.is_none() {
            return Ok(None);
        }
        self.send_console_update(
            target.user_id,
            ConsoleUpdate {
                access: None,
                access_extra: None,
                icon,
                name,
            },
        )
        .await;
        Ok(Some(target.name))
    }

    /// Runs `command` (lowercased, without the prefix) via the
    /// configured [`ChatCommandDispatcher`]. `None` if unrecognized.
    pub async fn run_console_command(
        &self,
        command: &str,
        args: &str,
        actor: &ChatCommandActor,
    ) -> Option<ConsoleReply> {
        self.commands.run(command, args, actor, self).await
    }

    /// List a directory as seen by `requester`. `dir` is empty for the
    /// root folder. A blocked drop box lists as empty, not an error.
    /// `None` if the directory doesn't exist or isn't visible.
    pub async fn list_files(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        access: Access,
    ) -> Option<Vec<FileNameWithInfo>> {
        if dropbox_blocked(dir, None, access) {
            return Some(Vec::new());
        }
        self.files.list(requester, dir).await
    }

    /// A file's size in bytes, for starting a [`myTran_DownloadFile`] (202)
    /// download. `Err` if it doesn't exist, is a folder, or is a drop box
    /// `access` can't view.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub async fn file_size(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<u64, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let size = self
            .files
            .size(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        Ok(size)
    }

    /// Every item under `dir`/`name`, for a [`myTran_DownloadFldr`] (210)
    /// download. Skips any drop box `access` can't view, with everything
    /// under it. `Err` if missing or itself drop-box-blocked.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    pub async fn walk_folder(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<Vec<FolderItem>, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let items = self
            .files
            .walk(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        Ok(items
            .into_iter()
            .filter(|item| !dropbox_blocked(&item.path, None, access))
            .collect())
    }

    /// Whether a file or folder already exists at this path.
    pub async fn file_exists(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> bool {
        self.files.exists(requester, dir, name).await
    }

    /// Whether `requester` may start (or resume) a [`myTran_UploadFile`] (203)
    /// upload of `name` under `dir`: the destination must be under an
    /// "Upload"/"Drop Box" folder unless `upload_anywhere`, and a finished file
    /// already at `name` always rejects the request. With `resume` false that's
    /// the only check; with `resume` true, an in-progress upload is also
    /// required, and its current size is returned as the resume offset (`0`
    /// otherwise).
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    pub async fn authorize_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
        resume: bool,
    ) -> Result<u64, Cow<'static, str>> {
        if !is_upload_folder(dir) {
            access.require(Access::UPLOAD_ANYWHERE)?;
        }
        if self.files.exists(requester, dir, name).await {
            Err("file already exists.")?;
        }
        if resume {
            Ok(self
                .files
                .partial_upload_size(requester, dir, name)
                .await
                .ok_or("file or folder not found.")?)
        } else {
            Ok(0)
        }
    }

    /// Start (or resume) a [`myTran_UploadFldr`] (213) upload of folder
    /// `name` under `dir`, creating the folder if it's missing. `dir` must
    /// exist, with the same destination rule as [`Self::authorize_upload`].
    /// Resuming needs a folder already at `name`; otherwise nothing may be
    /// there yet.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    pub async fn start_folder_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
        resume: bool,
    ) -> Result<(), Cow<'static, str>> {
        self.files
            .list(requester, dir)
            .await
            .ok_or("file or folder not found.")?;
        if !is_upload_folder(dir) {
            access.require(Access::UPLOAD_ANYWHERE)?;
        }
        let existing = self.files.info(requester, dir, name).await;
        if resume {
            let info = existing.ok_or("file or folder not found.")?;
            if info.file_type != *b"fldr" {
                Err("file already exists.")?;
            }
            return Ok(());
        }
        if existing.is_some() {
            Err("file already exists.")?;
        }
        self.files
            .mkdir(requester, dir, name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Prepare one item of a [`myTran_UploadFldr`] (213) upload at
    /// `dir`/`name`: create a missing folder, or pick where a file's data
    /// starts. `Some` byte offset to resume a file's upload from (`0` for a
    /// new file); `None` to skip the item, including a finished file
    /// already there and any `dir` that isn't a folder.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    pub async fn prepare_folder_upload_item(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        is_folder: bool,
    ) -> Option<u64> {
        self.files.list(requester, dir).await?;
        if is_folder {
            if !self.files.exists(requester, dir, name).await {
                let _ = self.files.mkdir(requester, dir, name).await;
            }
            return None;
        }
        if let Some(offset) = self.files.partial_upload_size(requester, dir, name).await {
            return Some(offset);
        }
        (!self.files.exists(requester, dir, name).await).then_some(0)
    }

    /// Open a file for reading, for a [`myTran_DownloadFile`] (202) download.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub async fn open_file_for_read(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> std::io::Result<BoxedReader> {
        self.files.open_read(requester, dir, name).await
    }

    /// Create a file for writing, for a [`myTran_UploadFile`] (203) upload.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    pub async fn create_file_for_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> std::io::Result<BoxedWriter> {
        self.files.create_write(requester, dir, name).await
    }

    /// Open an existing file for writing from `at` onward, continuing a
    /// resumed [`myTran_UploadFile`] (203) upload.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    pub async fn resume_file_for_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> std::io::Result<BoxedWriter> {
        self.files.open_write_resume(requester, dir, name, at).await
    }

    /// One file or folder's listing metadata. `Err` if missing or
    /// drop-box-blocked.
    pub async fn file_info(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<FileNameWithInfo, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = self
            .files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        Ok(info)
    }

    /// Delete a file, or a folder and everything under it, after
    /// checking the matching privilege.
    pub async fn delete_file(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        let info = self
            .files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let is_dir = info.file_type == *b"fldr";
        let required = if is_dir {
            Access::DELETE_FOLDER
        } else {
            Access::DELETE_FILE
        };
        access.require(required)?;
        self.files
            .delete(requester, dir, name, is_dir)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Create a folder, after checking drop box visibility.
    pub async fn mkdir(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        self.files
            .mkdir(requester, dir, name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Rename a file or folder in place, after checking the matching
    /// privilege and drop box visibility.
    pub async fn rename_file(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        new_name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = self
            .files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let is_folder = info.file_type == *b"fldr";
        access.require(if is_folder {
            Access::RENAME_FOLDER
        } else {
            Access::RENAME_FILE
        })?;
        self.files
            .rename(requester, dir, name, new_name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Move a file or folder into another directory, keeping its name,
    /// after checking the matching privilege.
    pub async fn move_file(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        to_dir: &HlPath,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        let info = self
            .files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let required = if info.file_type == *b"fldr" {
            Access::MOVE_FOLDER
        } else {
            Access::MOVE_FILE
        };
        access.require(required)?;
        self.files
            .r#move(requester, dir, name, to_dir)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Create an alias at `link_dir`/`link_name` pointing at
    /// `target_dir`/`target_name`.
    pub async fn symlink_file(
        &self,
        requester: FileRequester<'_>,
        target_dir: &HlPath,
        target_name: &str,
        link_dir: &HlPath,
        link_name: &str,
    ) -> Result<(), Cow<'static, str>> {
        self.files
            .symlink(requester, target_dir, target_name, link_dir, link_name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A file or folder's stored comment, or `None` if it's never had
    /// one set. `Err` if missing or drop-box-blocked.
    pub async fn file_comment(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<Option<String>, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        Ok(self.files.comment(requester, dir, name).await)
    }

    /// Set a file or folder's comment, after checking the matching
    /// privilege and drop box visibility.
    pub async fn set_file_comment(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = self
            .files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let required = if info.file_type == *b"fldr" {
            Access::SET_FOLDER_COMMENT
        } else {
            Access::SET_FILE_COMMENT
        };
        access.require(required)?;
        self.files
            .set_comment(requester, dir, name, comment)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Register a pending transfer, returning the reference number to
    /// hand the client. Retries on the vanishingly unlikely case the
    /// counter wraps into a still-pending ref. An upload expires after
    /// `TRANSFER_IDLE_TIMEOUT`.
    pub async fn register_transfer(&self, transfer: PendingTransfer) -> TransferRef {
        let is_upload = transfer.direction == TransferDirection::Put;
        let reference = {
            let mut transfers = self.transfers.write().await;
            loop {
                let reference = TransferRef(self.next_transfer_ref.fetch_add(1, Ordering::Relaxed));
                if reference.0 != 0 && !transfers.contains_key(&reference) {
                    transfers.insert(reference, transfer);
                    break reference;
                }
            }
        };
        if is_upload {
            self.spawn_upload_idle_timeout(reference);
        }
        reference
    }

    /// Drop upload `reference` if no `HTXF` connection arrives within
    /// [`TRANSFER_IDLE_TIMEOUT`].
    fn spawn_upload_idle_timeout(&self, reference: TransferRef) {
        let state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TRANSFER_IDLE_TIMEOUT).await;
            state.take_transfer(reference).await;
        });
    }

    /// Remove and return a pending transfer by reference number, if one
    /// is still waiting. Consumes it: a reference is only good once.
    pub async fn take_transfer(&self, reference: TransferRef) -> Option<PendingTransfer> {
        self.transfers.write().await.remove(&reference)
    }

    /// Admit a registered download. Returns its [`myField_WaitingCount`] (116),
    /// `0` if it is active now.
    ///
    /// [`myField_WaitingCount`]: hotline_proto::FieldId::WaitingCount
    pub async fn admit_download(&self, reference: TransferRef, user_id: u16) -> u32 {
        let waiting_count = self.download_queue.admit(reference, user_id);
        if waiting_count == 0 {
            self.spawn_download_idle_timeout(reference);
        }
        waiting_count
    }

    /// Free `reference`'s slot if no `HTXF` connection arrives within
    /// [`TRANSFER_IDLE_TIMEOUT`]. Restarts on promotion.
    fn spawn_download_idle_timeout(&self, reference: TransferRef) {
        let state = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(TRANSFER_IDLE_TIMEOUT).await;
            if state.take_transfer(reference).await.is_some() {
                state.release_download_slot(reference).await;
            }
        });
    }

    /// Wait until `reference` holds an active download slot. `false` if
    /// it left the queue without one, such as on a disconnect.
    pub async fn wait_for_download_slot(&self, reference: TransferRef) -> bool {
        self.download_queue.wait_for_slot(reference).await
    }

    /// Drop `reference`'s download, active or queued, and push
    /// [`myTran_DownloadInfo`] (211) to each download it promotes or moves.
    ///
    /// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
    pub async fn release_download_slot(&self, reference: TransferRef) {
        let updates = self.download_queue.release(reference);
        self.push_download_updates(updates).await;
    }

    /// Cancel `user_id`'s download `reference`, queued or running, like
    /// openline's `KillDownloadByRefNum`. Another user's download is left
    /// alone.
    pub async fn kill_download(&self, reference: TransferRef, user_id: u16) {
        let Some(updates) = self.download_queue.kill(reference, user_id) else {
            return;
        };
        self.take_transfer(reference).await;
        self.push_download_updates(updates).await;
    }

    /// `reference`'s cancel token, `None` once the download has left the
    /// queue.
    pub(crate) fn download_cancel_token(
        &self,
        reference: TransferRef,
    ) -> Option<CancellationToken> {
        self.download_queue.cancel_token(reference)
    }

    /// Drop a disconnected user's active and waiting downloads, promoting
    /// and notifying any others that now fit.
    async fn release_downloads_for_user(&self, user_id: u16) {
        let updates = self.download_queue.remove_user(user_id);
        self.push_download_updates(updates).await;
    }

    /// Push one [`myTran_DownloadInfo`] (211) per update, starting a freshly
    /// promoted entry's idle timeout.
    ///
    /// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
    async fn push_download_updates(&self, updates: Vec<QueueUpdate>) {
        for QueueUpdate {
            user_id,
            reference,
            waiting_count,
        } in updates
        {
            if waiting_count == 0 {
                self.spawn_download_idle_timeout(reference);
            }
            let tx = Transaction::push(
                Opcode::DownloadInfo,
                vec![
                    Field::u32(FieldId::RefNum, reference.0),
                    Field::u32(FieldId::WaitingCount, waiting_count),
                ],
            );
            self.send_to(user_id, tx).await;
        }
    }

    /// Finish `dir`/`name`'s upload, once every byte has arrived.
    pub async fn finish_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> std::io::Result<()> {
        self.files.finish_upload(requester, dir, name).await
    }

    /// The currently configured banner, for the login-time
    /// [`myTran_ServerBanner`] (122) push.
    ///
    /// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
    pub async fn banner_kind(&self) -> Option<BannerKind> {
        self.banner.kind().await
    }

    /// Start a banner-image download over `HTXF`, returning `(byte size,
    /// reference)` to reply with. `Err` if no image banner is configured
    /// (a URL banner has nothing to fetch this way).
    pub async fn start_banner_transfer(
        &self,
        client_ip: IpAddr,
        user_id: u16,
    ) -> Result<(u64, TransferRef), &'static str> {
        match self.banner.kind().await {
            Some(BannerKind::Image { .. }) => {}
            _ => return Err("no banner is configured."),
        }
        let size = self
            .banner
            .size()
            .await
            .map_err(|_| "banner is unavailable.")?;
        let reference = self
            .register_transfer(PendingTransfer {
                kind: TransferKind::Banner,
                direction: TransferDirection::Get,
                size,
                resume_offset: 0,
                client_ip,
                user_id,
            })
            .await;
        Ok((size, reference))
    }

    /// Open the banner image for reading, for a [`myTran_DownloadBanner`] (212)
    /// download.
    ///
    /// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
    pub async fn open_banner_read(&self) -> std::io::Result<BoxedReader> {
        self.banner.open_read().await
    }

    /// The full classic news text.
    pub async fn news(&self) -> String {
        self.flat_news.load().await.to_string()
    }

    /// Prepend a new post to the classic news. Returns the formatted entry.
    ///
    /// Matches openline's `AddToMsgBoard` (`HotlineServ.cp`) byte for byte,
    /// including its missing space between month and day.
    pub async fn post_news(&self, poster: &str, text: &str) -> String {
        use chrono::{Datelike, Timelike};

        const MONTHS: [&str; 12] = [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ];
        const FOOTER_DIVIDER: &str = "__________________________________________________________";

        let now = chrono::Local::now();
        let month = MONTHS[now.month0() as usize];
        let entry = format!(
            "From {poster} ({month}{day:02} {hour:02}:{minute:02}):\r\r{text}\r{FOOTER_DIVIDER}\r",
            day = now.day(),
            hour = now.hour(),
            minute = now.minute(),
        );
        self.flat_news.post(&entry).await;
        entry
    }

    /// List a threaded news bundle's own contents at `path`. `Err` if it
    /// doesn't resolve to a bundle.
    pub async fn news_list_dir(
        &self,
        path: &HlPath,
    ) -> Result<Vec<NewsCategoryListEntry>, &'static str> {
        let entries = self
            .threaded_news
            .list_dir(&path.components)
            .await
            .ok_or("news bundle not found")?;
        Ok(entries
            .into_iter()
            .map(|(name, kind)| match kind {
                NewsListKind::Bundle { count } => NewsCategoryListEntry::Bundle { count, name },
                NewsListKind::Category(summary) => NewsCategoryListEntry::Category {
                    count: summary.count,
                    guid: summary.guid,
                    add_sn: summary.add_sn,
                    del_sn: summary.del_sn,
                    name,
                },
            })
            .collect())
    }

    /// List a threaded news category's articles at `path`. `Err` if it
    /// doesn't resolve to a category.
    pub async fn news_list_articles(
        &self,
        path: &HlPath,
    ) -> Result<NewsArticleListData, &'static str> {
        let articles = self
            .threaded_news
            .list_articles(&path.components)
            .await
            .ok_or("news category not found")?;
        Ok(NewsArticleListData {
            articles: articles
                .into_iter()
                .map(|(id, article)| {
                    let (mime_type, data) = article.primary_flavor();
                    NewsArticleSummary {
                        id,
                        date: article.date,
                        parent_id: article.parent_id,
                        flags: article.flags,
                        title: article.title.clone(),
                        poster: article.poster.clone(),
                        flavor: mime_type.to_string(),
                        flavor_size: data.len() as u16,
                    }
                })
                .collect(),
        })
    }

    /// One article's full data at `path`. `Err` if the category or the
    /// article itself doesn't exist.
    pub async fn news_article(
        &self,
        path: &HlPath,
        id: u32,
    ) -> Result<NewsArticleDetail, &'static str> {
        self.threaded_news
            .article(&path.components, id)
            .await
            .ok_or("news article not found")
    }

    /// Create a news bundle (folder) named `name` under `path`.
    pub async fn news_mkdir(&self, path: &HlPath, name: &str) -> Result<(), &'static str> {
        self.threaded_news.mkdir(&path.components, name).await
    }

    /// Create a news category named `name` under `path`.
    pub async fn news_mkcategory(&self, path: &HlPath, name: &str) -> Result<(), &'static str> {
        self.threaded_news.mkcategory(&path.components, name).await
    }

    /// Delete the news bundle or category named by `path` (its last
    /// component is the item's own name), after checking the matching
    /// bundle/category privilege. `Err` for the root or a path that
    /// doesn't resolve.
    pub async fn news_delete(
        &self,
        path: &HlPath,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        if path.components.is_empty() {
            Err("news item not found")?;
        }
        let kind = self
            .threaded_news
            .item_kind(&path.components)
            .await
            .ok_or("news item not found")?;
        let required = match kind {
            NewsItemKind::Bundle => Access::NEWS_DELETE_FLDR,
            NewsItemKind::Category => Access::NEWS_DELETE_CAT,
        };
        access.require(required)?;
        self.threaded_news.delete(&path.components).await?;
        Ok(())
    }

    /// Post a new article into the news category at `path`.
    #[allow(clippy::too_many_arguments)]
    pub async fn news_post_article(
        &self,
        path: &HlPath,
        parent_id: u32,
        title: String,
        poster: String,
        flags: u32,
        mime_type: String,
        data: Vec<u8>,
    ) -> Result<u32, &'static str> {
        self.threaded_news
            .post_article(
                &path.components,
                parent_id,
                title,
                poster,
                flags,
                mime_type,
                data,
            )
            .await
    }

    /// Delete an article from the news category at `path`. If
    /// `recursive`, every reply under it is deleted too; otherwise its
    /// first reply is promoted into its place.
    pub async fn news_delete_article(
        &self,
        path: &HlPath,
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str> {
        self.threaded_news
            .delete_article(&path.components, id, recursive)
            .await
    }

    /// Create a new private chat room with `creator` as its only member,
    /// returning the new room's ID.
    pub async fn create_chat_room(&self, creator: u16) -> u32 {
        let id = self.next_chat_id.fetch_add(1, Ordering::Relaxed);
        self.chat_rooms.write().await.insert(
            id,
            ChatRoom {
                members: vec![creator],
                ..Default::default()
            },
        );
        id
    }

    /// Current members of a chat room, in join order, if it exists.
    pub async fn chat_room_members(&self, chat_id: u32) -> Option<Vec<u16>> {
        self.chat_rooms
            .read()
            .await
            .get(&chat_id)
            .map(|r| r.members.clone())
    }

    /// Snapshot of a chat room (members and subject), if it exists.
    pub async fn chat_room(&self, chat_id: u32) -> Option<ChatRoom> {
        self.chat_rooms.read().await.get(&chat_id).cloned()
    }

    /// Add `user_id` to a chat room. Requires a pending invite, or the
    /// room's password if one is set.
    pub async fn join_chat_room(
        &self,
        chat_id: u32,
        user_id: u16,
        password: &str,
    ) -> Result<(), &'static str> {
        let mut rooms = self.chat_rooms.write().await;
        let room = rooms.get_mut(&chat_id).ok_or("no such chat room")?;
        let invited = room.invited.remove(&user_id);
        if !invited {
            if room.password.is_empty() {
                return Err("not invited");
            }
            if password != room.password {
                return Err("wrong password");
            }
        }
        if !room.members.contains(&user_id) {
            room.members.push(user_id);
        }
        Ok(())
    }

    /// Add `user_id` to a chat room directly, no invite or password
    /// check. For internal auto-membership (room 0 on login) and test
    /// setup, not the [`myTran_JoinChat`] (115) request path.
    ///
    /// [`myTran_JoinChat`]: hotline_proto::Opcode::JoinChat
    pub(crate) async fn add_chat_member(&self, chat_id: u32, user_id: u16) {
        if let Some(room) = self.chat_rooms.write().await.get_mut(&chat_id) {
            if !room.members.contains(&user_id) {
                room.members.push(user_id);
            }
            room.invited.remove(&user_id);
        }
    }

    /// Mark `user_id` as invited to a chat room. Returns whether this was
    /// a new invite (`false` if already invited, or if the room doesn't
    /// exist).
    pub async fn invite(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .is_some_and(|room| room.invited.insert(user_id))
    }

    /// Invite `target_id` to a chat room on `inviter`'s behalf: `target_id`
    /// must be connected, the room must exist, `inviter` must already be a
    /// member, and `target_id` must be neither a member nor already
    /// invited. One atomic operation, like [`Self::join_chat_room`].
    pub async fn invite_to_chat_room(
        &self,
        chat_id: u32,
        inviter: u16,
        target_id: u16,
    ) -> Result<(), &'static str> {
        if !self.has_user(target_id).await {
            return Err("no such user");
        }
        let mut rooms = self.chat_rooms.write().await;
        let room = rooms.get_mut(&chat_id).ok_or("no such chat room")?;
        if !room.members.contains(&inviter) {
            return Err("you are not in that chat room");
        }
        if room.members.contains(&target_id) {
            return Err("that user is already in the chat");
        }
        if !room.invited.insert(target_id) {
            return Err("that user is already invited to the chat");
        }
        Ok(())
    }

    /// Clear a pending invite without joining, e.g. on decline. Returns
    /// whether there was one to clear.
    pub async fn invite_declined(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .is_some_and(|room| room.invited.remove(&user_id))
    }

    /// Whether `user_id` has a pending invite to a chat room.
    pub async fn is_invited(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms
            .read()
            .await
            .get(&chat_id)
            .is_some_and(|r| r.invited.contains(&user_id))
    }

    /// Remove `user_id` from a chat room, deleting the room once its
    /// last member leaves. Chat ID `0` (the main room) is never deleted.
    pub async fn leave_chat_room(&self, chat_id: u32, user_id: u16) {
        let mut rooms = self.chat_rooms.write().await;
        if let Some(room) = rooms.get_mut(&chat_id) {
            room.members.retain(|&id| id != user_id);
            if chat_id != 0 && room.members.is_empty() {
                rooms.remove(&chat_id);
            }
        }
    }

    /// Remove `user_id` from every chat room, deleting any that
    /// becomes empty. Returns each room's ID and remaining members.
    pub async fn leave_all_chat_rooms(&self, user_id: u16) -> Vec<(u32, Vec<u16>)> {
        let mut rooms = self.chat_rooms.write().await;
        let mut left = Vec::new();
        rooms.retain(|&chat_id, room| {
            if !room.members.contains(&user_id) {
                return true;
            }
            room.members.retain(|&id| id != user_id);
            left.push((chat_id, room.members.clone()));
            !room.members.is_empty()
        });
        left
    }

    /// Set a chat room's subject. `false` if the room doesn't exist.
    pub async fn set_chat_subject(&self, chat_id: u32, subject: String) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .map(|room| {
                room.subject = subject;
            })
            .is_some()
    }

    /// Set a chat room's join password. `false` if the room doesn't exist.
    pub async fn set_chat_password(&self, chat_id: u32, password: String) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .map(|room| {
                room.password = password;
            })
            .is_some()
    }

    /// Whether `user_id` may change a chat room's subject/password: the
    /// global room (`chat_id == 0`) needs `AccessExtra::SET_SUBJECT`; any
    /// other room just needs `user_id` to already be a member.
    pub async fn authorize_chat_subject_change(
        &self,
        chat_id: u32,
        user_id: u16,
        is_global: bool,
        access_extra: AccessExtra,
    ) -> Result<(), Cow<'static, str>> {
        if is_global {
            access_extra.require(AccessExtra::SET_SUBJECT)?;
            return Ok(());
        }
        match self.chat_room_members(chat_id).await {
            Some(members) if members.contains(&user_id) => Ok(()),
            Some(_) => Err("you are not in that chat room")?,
            None => Err("no such chat room")?,
        }
    }

    /// Look up an account by login.
    pub async fn account(&self, login: &str) -> Option<Account> {
        self.accounts.get(login).await
    }

    /// Create or overwrite an account.
    pub async fn put_account(
        &self,
        login: String,
        account: Account,
    ) -> Result<(), Cow<'static, str>> {
        self.accounts.put(login, account).await
    }

    /// The active ban on `ip`, if any; see [`crate::BanProvider::is_banned`].
    pub async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord> {
        self.bans.is_banned(ip).await
    }

    /// Ban `ip` until `until`, or permanently if `None`.
    pub async fn ban(
        &self,
        ip: IpAddr,
        until: Option<std::time::SystemTime>,
    ) -> Result<(), Cow<'static, str>> {
        self.bans.ban(ip, until).await
    }

    /// Ban `ip` for [`TEMP_BAN_DURATION`], logging a failure.
    pub(crate) async fn temp_ban(&self, ip: IpAddr) {
        let until = std::time::SystemTime::now() + TEMP_BAN_DURATION;
        if let Err(error) = self.ban(ip, Some(until)).await {
            tracing::warn!(%ip, %error, "failed to persist ban");
        }
    }

    /// Lift `ip`'s ban. `Err` if it isn't banned.
    pub async fn unban(&self, ip: IpAddr) -> Result<(), Cow<'static, str>> {
        self.bans.unban(ip).await?.ok_or("address is not banned")?;
        Ok(())
    }

    /// Record one `event` from `user_id`; see [`crate::FloodPolicy::check`].
    pub async fn check_flood(&self, user_id: u16, event: FloodEvent<'_>) -> FloodDecision {
        self.flood.check(user_id, event).await
    }

    /// Record one new connection from `ip`; see
    /// [`crate::FloodPolicy::check_connection`].
    pub async fn check_connection_flood(&self, ip: IpAddr) -> FloodDecision {
        self.flood.check_connection(ip).await
    }

    /// Forget `user_id`'s accumulated flood state, called on disconnect.
    pub async fn reset_flood(&self, user_id: u16) {
        self.flood.reset(user_id).await;
    }

    /// Delete an account. `Err("no such account")` if it didn't exist.
    pub async fn delete_account(&self, login: &str) -> Result<(), Cow<'static, str>> {
        if !self.accounts.delete(login).await? {
            Err("no such account")?;
        }
        Ok(())
    }

    /// Every account, login and details together, sorted by login for a
    /// deterministic listing order, [`myTran_GetUserList`] (348).
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub async fn list_accounts(&self) -> Vec<(String, Account)> {
        let mut accounts = self.accounts.list().await;
        accounts.sort_by(|(a, _), (b, _)| a.cmp(b));
        accounts
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Two `SharedState`s sharing a news provider see each other's writes.
    #[tokio::test]
    async fn news_is_visible_across_shared_states_backed_by_the_same_provider() {
        let provider = Arc::new(InMemoryNewsProvider::default());
        let state = SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(InMemoryFileProvider::default()),
            flat_news: provider.clone(),
            threaded_news: provider.clone(),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
        });

        state.post_news("alice", "hello").await;
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();
        state
            .news_post_article(
                &HlPath::new(["General"]),
                0,
                "Hi".to_string(),
                "alice".to_string(),
                0,
                "text/plain".to_string(),
                b"hello".to_vec(),
            )
            .await
            .unwrap();

        let other = SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(InMemoryFileProvider::default()),
            flat_news: provider.clone(),
            threaded_news: provider,
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
        });

        assert!(other.news().await.contains("hello"));
        let articles = other
            .news_list_articles(&HlPath::new(["General"]))
            .await
            .unwrap();
        assert_eq!(articles.articles.len(), 1);
        assert_eq!(articles.articles[0].title, "Hi");
    }

    /// Matches openline's `AddToMsgBoard` header, body, and footer shape,
    /// with no `<` byte.
    #[tokio::test]
    async fn post_news_matches_openlines_header_and_footer_shape() {
        let state = SharedState::default();

        let entry = state.post_news("alice", "hello").await;

        let footer = "_".repeat(58);
        assert!(entry.starts_with("From alice ("), "{entry:?}");
        assert!(entry.contains("):\r\rhello\r"), "{entry:?}");
        assert!(entry.ends_with(&format!("\r{footer}\r")), "{entry:?}");
        assert!(!entry.contains('<'), "{entry:?}");
    }

    /// `HashMap`'s iteration order has no relation to insertion order, so
    /// `users()` must sort explicitly rather than rely on it.
    #[tokio::test]
    async fn users_are_ordered_by_user_id_not_insertion_order() {
        let state = SharedState::default();
        for id in [3, 1, 2] {
            state
                .insert_user(UserEntry {
                    user_id: id,
                    login: String::new(),
                    account_name: String::new(),
                    name: format!("user{id}"),
                    icon: 0,
                    client_version: None,
                    access: Access::empty(),
                    access_extra: AccessExtra::default(),
                    remote_addr: "127.0.0.1:0".parse().unwrap(),
                    connected_at: Instant::now(),
                    last_active: Instant::now(),
                    refuses_pm: false,
                    refuses_chat: false,
                    automatic_response: None,
                })
                .await;
        }
        let ids: Vec<u16> = state.users().await.into_iter().map(|u| u.user_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn leave_all_chat_rooms_notifies_remaining_and_deletes_empty_rooms() {
        let state = SharedState::default();
        let solo = state.create_chat_room(1).await;
        let shared = state.create_chat_room(2).await;
        state.add_chat_member(shared, 1).await;

        let left = state.leave_all_chat_rooms(1).await;

        assert_eq!(left.len(), 2);
        assert!(left.contains(&(solo, vec![])));
        assert!(left.contains(&(shared, vec![2])));
        assert!(state.chat_room(solo).await.is_none());
        assert_eq!(state.chat_room_members(shared).await, Some(vec![2]));
    }

    #[tokio::test]
    async fn global_room_members_are_tracked_in_join_order() {
        let state = SharedState::default();
        for id in [3, 1, 2] {
            state
                .insert_user(UserEntry {
                    user_id: id,
                    login: String::new(),
                    account_name: String::new(),
                    name: format!("user{id}"),
                    icon: 0,
                    client_version: None,
                    access: Access::empty(),
                    access_extra: AccessExtra::default(),
                    remote_addr: "127.0.0.1:0".parse().unwrap(),
                    connected_at: Instant::now(),
                    last_active: Instant::now(),
                    refuses_pm: false,
                    refuses_chat: false,
                    automatic_response: None,
                })
                .await;
        }
        assert_eq!(state.chat_room_members(0).await, Some(vec![3, 1, 2]));

        state.remove_user(3).await;
        assert_eq!(state.chat_room_members(0).await, Some(vec![1, 2]));
    }

    #[tokio::test]
    async fn leaving_the_global_room_never_deletes_it() {
        let state = SharedState::default();
        state.leave_chat_room(0, 1).await;
        assert!(state.chat_room(0).await.is_some());
    }

    #[tokio::test]
    async fn list_files_lists_the_root_directory() {
        let files = Arc::new(InMemoryFileProvider::default());
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["sub"]), "inner.txt", *b"x");
        let state = SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files,
            flat_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            threaded_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
        });

        let requester = FileRequester { login: "alice" };
        let mut entries = state
            .list_files(requester, &HlPath::default(), Access::all())
            .await
            .unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
        assert_eq!(entries[1].name, "sub");
        assert_eq!(&entries[1].file_type, b"fldr");
    }

    fn state_with_files(files: InMemoryFileProvider) -> SharedState {
        SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files: Arc::new(files),
            flat_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            threaded_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
        })
    }

    #[tokio::test]
    async fn walk_folder_skips_a_drop_box_and_its_contents_without_view_drop_boxes() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"a");
        files.seed(&HlPath::new(["Top", "My Drop Box"]), "secret.txt", *b"s");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let root = HlPath::default();
        let walk = |access| state.walk_folder(requester, &root, "Top", access);

        let paths = |items: Vec<FolderItem>| -> Vec<Vec<String>> {
            items.into_iter().map(|i| i.path.components).collect()
        };
        assert_eq!(
            paths(walk(Access::empty()).await.unwrap()),
            [vec!["a.txt".to_string()]]
        );
        assert_eq!(walk(Access::VIEW_DROP_BOXES).await.unwrap().len(), 3);
    }

    #[tokio::test]
    async fn walk_folder_refuses_a_drop_box_root_without_view_drop_boxes() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "a.txt", *b"a");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        assert!(state
            .walk_folder(requester, &HlPath::default(), "Drop Box", Access::empty())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn an_in_progress_upload_is_listed_under_its_real_hpf_name() {
        let files = Arc::new(InMemoryFileProvider::default());
        let state = SharedState::new(Providers {
            accounts: Arc::new(InMemoryAccountProvider::default()),
            files,
            flat_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            threaded_news: Arc::new(crate::news::InMemoryNewsProvider::default()),
            bans: Arc::new(InMemoryBanProvider::default()),
            flood: Arc::new(NoFloodPolicy),
            banner: Arc::new(NoBanner),
            commands: Arc::new(crate::console::DefaultChatCommands),
        });
        let requester = FileRequester { login: "alice" };

        {
            let mut writer = state
                .create_file_for_write(requester, &HlPath::default(), "a.txt")
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, b"hi")
                .await
                .unwrap();
        }

        let entries = state
            .list_files(requester, &HlPath::default(), Access::all())
            .await
            .unwrap();
        assert_eq!(entries[0].name, "a.txt.hpf");
        assert_eq!(entries[0].file_type, crate::file::PARTIAL_TYPE);
        assert_eq!(entries[0].creator, crate::file::PARTIAL_CREATOR);

        let info = state
            .file_info(requester, &HlPath::default(), "a.txt.hpf", Access::all())
            .await
            .unwrap();
        assert_eq!(info.file_type, crate::file::PARTIAL_TYPE);
        assert_eq!(info.creator, crate::file::PARTIAL_CREATOR);

        state
            .finish_upload(requester, &HlPath::default(), "a.txt")
            .await
            .unwrap();
        let info = state
            .file_info(requester, &HlPath::default(), "a.txt", Access::all())
            .await
            .unwrap();
        assert_ne!(info.file_type, crate::file::PARTIAL_TYPE);
    }

    #[tokio::test]
    async fn register_transfer_allocates_distinct_refs_and_take_consumes_them() {
        let state = SharedState::default();
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".to_string(),
                    dir: HlPath::default(),
                    name: "x".to_string(),
                },
                raw: false,
            }),
            direction: TransferDirection::Get,
            size: 0,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };

        let ref_a = state.register_transfer(make()).await;
        let ref_b = state.register_transfer(make()).await;
        assert_ne!(ref_a, ref_b);

        assert!(state.take_transfer(ref_a).await.is_some());
        assert!(state.take_transfer(ref_a).await.is_none());
        assert!(state.take_transfer(ref_b).await.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn an_upload_whose_htxf_connection_never_arrives_expires() {
        let state = SharedState::default();
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(crate::state::FileTransfer {
                    target: crate::state::TransferTarget {
                        login: "alice".to_string(),
                        dir: HlPath::default(),
                        name: "x".to_string(),
                    },
                    raw: false,
                }),
                direction: TransferDirection::Put,
                size: 0,
                resume_offset: 0,
                client_ip: "127.0.0.1".parse().unwrap(),
                user_id: 1,
            })
            .await;

        tokio::task::yield_now().await;
        tokio::time::advance(TRANSFER_IDLE_TIMEOUT + Duration::from_secs(1)).await;
        tokio::task::yield_now().await;

        assert!(state.take_transfer(reference).await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn an_admitted_download_whose_htxf_connection_never_arrives_frees_its_slot() {
        let state = SharedState::default();
        state.set_download_limits(1, 10);
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".to_string(),
                    dir: HlPath::default(),
                    name: "x".to_string(),
                },
                raw: false,
            }),
            direction: TransferDirection::Get,
            size: 0,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };

        let abandoned = state.register_transfer(make()).await;
        assert_eq!(state.admit_download(abandoned, 1).await, 0);

        // The second download queues behind the never-connected first.
        let second = state.register_transfer(make()).await;
        assert_eq!(state.admit_download(second, 2).await, 1);

        // Let both timeout tasks reach `sleep` before the clock advances.
        tokio::task::yield_now().await;
        tokio::time::advance(TRANSFER_IDLE_TIMEOUT + Duration::from_secs(1)).await;
        // Let the abandoned slot's timeout task run and promote `second`.
        tokio::task::yield_now().await;

        assert!(state.take_transfer(abandoned).await.is_none());
        let promoted =
            tokio::time::timeout(Duration::from_secs(1), state.wait_for_download_slot(second))
                .await
                .expect("second download should have been promoted once the idle slot freed");
        assert!(promoted);
    }

    #[tokio::test]
    async fn a_killed_download_refuses_its_htxf_connection_and_promotes_the_next() {
        let state = SharedState::default();
        state.set_download_limits(1, 10);
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".to_string(),
                    dir: HlPath::default(),
                    name: "x".to_string(),
                },
                raw: false,
            }),
            direction: TransferDirection::Get,
            size: 0,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
            user_id: 1,
        };
        let first = state.register_transfer(make()).await;
        assert_eq!(state.admit_download(first, 1).await, 0);
        let second = state.register_transfer(make()).await;
        assert_eq!(state.admit_download(second, 2).await, 1);

        state.kill_download(first, 2).await;
        assert!(state.download_cancel_token(first).is_some());

        state.kill_download(first, 1).await;
        assert!(state.take_transfer(first).await.is_none());
        assert!(state.download_cancel_token(first).is_none());
        let promoted =
            tokio::time::timeout(Duration::from_secs(1), state.wait_for_download_slot(second))
                .await
                .expect("second download should have been promoted");
        assert!(promoted);
    }

    fn test_user_entry(user_id: u16, access_extra: AccessExtra) -> UserEntry {
        UserEntry {
            user_id,
            login: "bob".to_string(),
            account_name: "Bob".to_string(),
            name: "Bob".to_string(),
            icon: 0,
            client_version: None,
            access: Access::default(),
            access_extra,
            remote_addr: "127.0.0.1:0".parse().unwrap(),
            connected_at: Instant::now(),
            last_active: Instant::now(),
            refuses_pm: false,
            refuses_chat: false,
            automatic_response: None,
        }
    }

    #[tokio::test]
    async fn apply_access_fields_grants_a_flag_live() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;
        let mut rx = state.register_recipient(2).await;

        let name = state
            .apply_access_fields(2, "delete_files=1")
            .await
            .unwrap();

        assert_eq!(name, "Bob");
        let update = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_console_update)
            .expect("expected a console update");
        assert_eq!(update.access, Some(Access::default() | Access::DELETE_FILE));
    }

    #[tokio::test]
    async fn apply_access_fields_rejects_broadcast_which_has_no_console_key() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_access_fields(2, "broadcast=1").await;

        assert_eq!(result, Err("unknown argument \"broadcast\"".into()));
    }

    /// mhxd matches console keys with `strcmp`.
    #[tokio::test]
    async fn apply_access_fields_console_key_is_case_sensitive() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_access_fields(2, "DELETE_FILES=1").await;

        assert_eq!(result, Err("unknown argument \"DELETE_FILES\"".into()));
    }

    /// mhxd stops at a word with no `=` and still applies what came before.
    #[tokio::test]
    async fn apply_access_fields_stops_at_a_word_without_equals() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;
        let mut rx = state.register_recipient(2).await;

        let result = state.apply_access_fields(2, "delete_files=1 junk").await;

        assert_eq!(result, Ok("Bob".to_string()));
        let update = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_console_update)
            .expect("expected a console update");
        assert_eq!(update.access, Some(Access::default() | Access::DELETE_FILE));
    }

    #[tokio::test]
    async fn apply_access_fields_refuses_a_target_without_access_volatile() {
        let state = SharedState::default();
        let non_volatile = AccessExtra::default() - AccessExtra::ACCESS_VOLATILE;
        state.insert_user(test_user_entry(2, non_volatile)).await;

        let result = state.apply_access_fields(2, "broadcast=1").await;

        assert_eq!(result, Err("user cannot be modified".into()));
    }

    #[tokio::test]
    async fn apply_0wn_fields_sets_icon_and_name() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;
        let mut rx = state.register_recipient(2).await;

        let name = state
            .apply_0wn_fields(2, "icon=42 name=New Name")
            .await
            .unwrap();

        assert_eq!(name.as_deref(), Some("Bob"));
        let update = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_console_update)
            .expect("expected a console update");
        assert_eq!(update.icon, Some(42));
        assert_eq!(update.name.as_deref(), Some("New Name"));
    }

    #[tokio::test]
    async fn apply_0wn_fields_refuses_a_target_without_is_0wn3d() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::empty()))
            .await;

        let result = state.apply_0wn_fields(2, "icon=5").await;

        assert_eq!(result, Err("User cannot be 0wned".into()));
    }

    /// mhxd's `user_0wn` ignores a key it doesn't know.
    #[tokio::test]
    async fn apply_0wn_fields_ignores_an_unknown_key() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_0wn_fields(2, "color=1").await;

        assert_eq!(result, Ok(None));
    }

    /// mhxd reads the icon with `strtoul`, so a non-number is `0`.
    #[tokio::test]
    async fn apply_0wn_fields_reads_the_icon_like_strtoul() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;
        let mut rx = state.register_recipient(2).await;

        state.apply_0wn_fields(2, "iconic=0x10").await.unwrap();

        let update = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_console_update)
            .expect("expected a console update");
        assert_eq!(update.icon, Some(16));
    }

    #[tokio::test]
    async fn apply_0wn_fields_caps_a_name_at_31_bytes() {
        let state = SharedState::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;
        let mut rx = state.register_recipient(2).await;

        let long = "x".repeat(40);
        state
            .apply_0wn_fields(2, &format!("name={long}"))
            .await
            .unwrap();

        let update = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_console_update)
            .expect("expected a console update");
        assert_eq!(update.name.as_deref(), Some(&long[..31]));
    }
}
