use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::future::Future;
use std::net::IpAddr;
use std::num::NonZero;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, SystemTime};

use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::htxf::{FlatFileInfo, HtxfType};
use hotline_proto::opcode::Opcode;
use hotline_proto::text_encoding::Encoding;
use hotline_proto::types::{
    FileNameWithInfo, HlDate, HlPath, NewsArticleListData, NewsArticleSummary,
    NewsCategoryListEntry, NewsFlavor,
};
use hotline_proto::{Field, FieldId, Transaction};

use crate::account::{AccessExtra, Account, AccountProvider, AccountStores};
use crate::activity::{ActivityEntry, ActivityLog};
use crate::ban::{BanProvider, BanRecord, TEMP_BAN_DURATION};
use crate::banner::{BannerKind, BannerProvider};
use crate::chat_room::{ChatRoom, ChatRoomProvider};
use crate::console::fields::{
    format_access, format_access_extra, parse_0wn_fields, parse_access_fields,
};
use crate::console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply};
use crate::file::{
    BoxedReader, FileProvider, FileRequester, FinderInfo, FolderItem, PARTIAL_TYPE,
    partial_upload_name,
};
use crate::flood::{FloodDecision, FloodEvent, FloodPolicy};
use crate::news::{
    FlatNewsProvider, NewsArticleDetail, NewsItemKind, NewsListKind, PostError,
    ThreadedNewsProvider, body_to_wire,
};
use crate::policy::{ChatRooms, ServerPolicy, Silent, TakenLogin, TimeZone};
use crate::providers::Providers;
use crate::random::random_u32;
use crate::server::{DEFAULT_MAX_DOWNLOADS, DEFAULT_MAX_DOWNLOADS_PER_CLIENT};
use crate::state::download_queue::{DownloadQueue, Limits, QueueUpdate};
use crate::state::progress::RunningTransfers;
use crate::state::recipient::{self, Recipient};
use crate::state::{
    AccessUpdate, Connection, ConsoleUpdate, NotReplaced, PendingTransfer, ProgressGuard,
    RecipientMessage, ReplacedAccount, TransferDirection, TransferKind, TransferProgress,
    TransferRef, TransferReport, UserEntry, dropbox_access, dropbox_blocked,
    upload_destination_access,
};
use crate::user_list::UserListProvider;

/// How long an active download or an upload may wait for its `HTXF`
/// connection, openline's 500 s.
const TRANSFER_IDLE_TIMEOUT: Duration = Duration::from_secs(500);

/// The bytes an upload must leave free on the disk.
const UPLOAD_SPACE_MARGIN: u64 = 512_000;

/// The error for a join of a chat room that is not open.
const CLOSED_CHAT: &str = "Cannot join private chat because it has been closed already.";

/// A counter that gives out 1 up to `u16::MAX`, then 1 again, and never 0.
struct NonZeroCounter(AtomicU16);

impl NonZeroCounter {
    /// A counter whose first value is 1.
    const fn new() -> Self {
        Self(AtomicU16::new(1))
    }

    /// The next value. A 0 from the counter goes to the next one.
    fn next(&self) -> NonZero<u16> {
        loop {
            let value = self.0.fetch_add(1, Ordering::Relaxed);
            if let Some(value) = NonZero::new(value) {
                return value;
            }
        }
    }
}

/// State shared by every [`crate::Session`], behind one `Arc`.
pub struct SharedState {
    /// Gives out the user IDs.
    user_id_counter: NonZeroCounter,
    /// One inbound channel per currently connected recipient.
    recipients: RwLock<HashMap<u16, Recipient>>,
    /// The storage and the behavior choices.
    providers: Providers,
    /// Held across each check and write of the accounts.
    account_writes: Mutex<()>,
    /// Transfers awaiting their `HTXF` data connection, keyed by the
    /// reference number handed to the client.
    transfers: RwLock<HashMap<TransferRef, PendingTransfer>>,
    /// Transfers whose data connection is open.
    running: Arc<RunningTransfers>,
    /// The high 16 bits of the next transfer reference number.
    transfer_counter: NonZeroCounter,
    /// Concurrent-download admission; see
    /// [`download_queue::DownloadQueue`](crate::state::download_queue::DownloadQueue).
    download_queue: DownloadQueue,
    /// Set by [`SharedState::start_quit`].
    quitting: AtomicBool,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            user_id_counter: NonZeroCounter::new(),
            recipients: RwLock::new(HashMap::new()),
            providers: Providers::default(),
            account_writes: Mutex::default(),
            transfers: RwLock::new(HashMap::new()),
            running: Arc::default(),
            transfer_counter: NonZeroCounter::new(),
            download_queue: DownloadQueue::default(),
            quitting: AtomicBool::new(false),
        }
    }
}

impl SharedState {
    /// Build fresh shared state backed by `providers`.
    pub fn new(providers: Providers) -> Arc<Self> {
        Arc::new(Self {
            providers,
            ..Self::default()
        })
    }

    /// `time` as a wire date in the policy's time zone, or the zero date
    /// for none.
    pub(crate) fn wire_date(&self, time: Option<SystemTime>) -> HlDate {
        time.map_or(HlDate::default(), |time| {
            self.policy().time_zone().convert(HlDate::from(time))
        })
    }

    /// An article's stored UTC `date` as a wire date in the policy's time
    /// zone.
    pub(crate) fn wire_article_date(&self, date: HlDate) -> HlDate {
        self.policy().time_zone().convert(date)
    }

    /// The server's behavior choices.
    pub(crate) fn policy(&self) -> &dyn ServerPolicy {
        &*self.providers.policy
    }

    /// The users that are online.
    fn user_list(&self) -> &dyn UserListProvider {
        &*self.providers.user_list
    }

    /// The chat rooms.
    fn chat_rooms(&self) -> &dyn ChatRoomProvider {
        &*self.providers.chat_rooms
    }

    /// The account storage.
    fn accounts(&self) -> &dyn AccountProvider {
        &*self.providers.accounts
    }

    /// The flat news storage.
    fn flat_news(&self) -> &dyn FlatNewsProvider {
        &*self.providers.flat_news
    }

    /// The storage of each account's own files and news.
    fn account_stores(&self) -> &dyn AccountStores {
        &*self.providers.account_stores
    }

    /// The ban list.
    fn bans(&self) -> &dyn BanProvider {
        &*self.providers.bans
    }

    /// The flood policy.
    fn flood(&self) -> &dyn FloodPolicy {
        &*self.providers.flood
    }

    /// The banner.
    fn banner(&self) -> &dyn BannerProvider {
        &*self.providers.banner
    }

    /// The chat commands.
    fn commands(&self) -> &dyn ChatCommandDispatcher {
        &*self.providers.commands
    }

    /// The activity log.
    fn activity(&self) -> &dyn ActivityLog {
        &*self.providers.activity
    }

    /// Report `entry` to the activity log.
    pub async fn record_activity(&self, entry: ActivityEntry) {
        self.activity().record(entry).await;
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
    /// Register a newly logged-in (or info-changed) user.
    pub async fn insert_user(&self, entry: UserEntry) {
        let user_id = entry.user_id;
        self.user_list().insert(entry).await;
        self.add_chat_member(0, user_id).await;
    }

    /// Remove a user on disconnect.
    pub async fn remove_user(self: &Arc<Self>, user_id: u16) {
        self.user_list().remove(user_id).await;
        self.leave_chat_room(0, user_id).await;
        self.release_downloads_for_user(user_id).await;
    }

    /// Snapshot of all currently connected users, in user ID order.
    pub async fn users(&self) -> Vec<UserEntry> {
        self.user_list().users().await
    }

    /// Look up one connected user by ID.
    pub async fn user(&self, user_id: u16) -> Option<UserEntry> {
        self.user_list().user(user_id).await
    }

    /// Whether `user_id` is currently connected.
    pub async fn has_user(&self, user_id: u16) -> bool {
        self.user(user_id).await.is_some()
    }

    /// Every currently connected user ID logged in under `login`, e.g. to
    /// disconnect them after their account is deleted or renamed away.
    pub async fn user_ids_for_login(&self, login: &str) -> Vec<u16> {
        self.users()
            .await
            .into_iter()
            .filter(|entry| &*entry.login == login)
            .map(|entry| entry.user_id)
            .collect()
    }

    /// The connected user `user_id` that a private message goes to.
    pub async fn require_connected_user(&self, user_id: u16) -> Result<UserEntry, &'static str> {
        self.user(user_id)
            .await
            .ok_or("Couldn't send your message because the client is not connected to this server.")
    }

    /// Whether `target_id` may be kicked. The `Err` text names a connected
    /// target that has [`Access::CANNOT_BE_DISCON`].
    pub async fn authorize_kick(&self, target_id: u16) -> Result<(), Cow<'static, str>> {
        if let Some(user) = self.user(target_id).await
            && user.access.contains(Access::CANNOT_BE_DISCON)
        {
            Err(format!(
                "You are not allowed to disconnect user “{}” (user is protected).",
                user.name
            ))?;
        }
        Ok(())
    }

    /// Record that `user_id` just sent a request, resetting their idle
    /// timer. Called once per received frame in the session loop.
    pub async fn touch_user(&self, user_id: u16) {
        self.user_list().touch(user_id).await;
    }

    /// Register a new connection as a recipient. `None` when each user ID is
    /// taken.
    ///
    /// The ID counts up from 1 and skips 0. It also skips an ID that a
    /// connection still holds, which openline's server does not do.
    pub(crate) async fn connect(&self) -> Option<Connection> {
        let mut recipients = self.recipients.write().await;
        let user_id = (0..u16::MAX)
            .map(|_| self.user_id_counter.next().get())
            .find(|user_id| !recipients.contains_key(user_id))?;
        let (recipient, inbox) = recipient::channel();
        recipients.insert(user_id, recipient);
        Some(Connection { user_id, inbox })
    }

    /// Register `user_id` as a recipient, returning its inbound channel.
    #[cfg(test)]
    pub(crate) async fn register_recipient(&self, user_id: u16) -> recipient::Inbox {
        let (recipient, inbox) = recipient::channel();
        self.recipients.write().await.insert(user_id, recipient);
        inbox
    }

    /// Drop a recipient's channel on disconnect.
    pub async fn remove_recipient(&self, user_id: u16) {
        self.recipients.write().await.remove(&user_id);
    }

    /// Resolves once `user_id`'s session ends. `None` if it has already
    /// gone.
    pub(crate) async fn session_ended(&self, user_id: u16) -> Option<impl Future<Output = ()>> {
        let recipient = self.recipients.read().await.get(&user_id)?.clone();
        Some(async move { recipient.ended().await })
    }

    /// How many connections have finished the handshake, logged in or not.
    pub async fn connection_count(&self) -> usize {
        self.recipients.read().await.len()
    }

    /// Tell each logged-in user that the server quits, with
    /// [`myTran_DisconnectMsg`] (111). openline's client then shows
    /// `message` and disconnects.
    ///
    /// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
    pub async fn announce_quit(&self, message: &str) {
        let data = vec![Field::string(FieldId::Data, message)];
        self.publish(Transaction::push(Opcode::DisconnectMsg, data))
            .await;
    }

    /// Make each listener close a new connection at once.
    pub fn start_quit(&self) {
        self.quitting.store(true, Ordering::Relaxed);
    }

    /// `true` after [`SharedState::start_quit`].
    pub fn is_quitting(&self) -> bool {
        self.quitting.load(Ordering::Relaxed)
    }

    /// Resolves once each connection that is open now has closed.
    pub async fn connections_closed(&self) {
        let recipients: Vec<Recipient> = self.recipients.read().await.values().cloned().collect();
        futures::future::join_all(recipients.iter().map(Recipient::closed)).await;
    }

    /// Send a transaction to every session, such as a chat line or a user
    /// list change. A slow or closed session misses it. Each recipient
    /// filters it with `Session::may_receive_broadcast`.
    pub async fn publish(&self, tx: Transaction) {
        for recipient in self.recipients.read().await.values() {
            recipient.try_send(RecipientMessage::Broadcast(tx.clone()));
        }
    }

    /// Send a transaction to exactly one connected user, unfiltered. A
    /// no-op if that user isn't connected.
    pub async fn send_to(&self, target_user_id: u16, tx: Transaction) {
        if let Some(recipient) = self.recipients.read().await.get(&target_user_id) {
            recipient.try_send(RecipientMessage::Push(tx));
        }
    }

    /// Queue a notice of `reason` for one connected user, then end the
    /// session. A full queue drops the notice. A no-op if that user is not
    /// connected.
    pub async fn kick(&self, target_user_id: u16, reason: &str) {
        if let Some(recipient) = self.recipients.read().await.get(&target_user_id) {
            if !recipient.try_send(RecipientMessage::Kick(reason.into())) {
                tracing::debug!(user_id = target_user_id, "kick notice dropped: queue full");
            }
            recipient.end();
        }
    }

    /// End one connected user's session with no notice. A no-op if that
    /// user isn't connected.
    pub async fn disconnect(&self, target_user_id: u16) {
        if let Some(recipient) = self.recipients.read().await.get(&target_user_id) {
            recipient.end();
        }
    }

    /// Like [`Self::publish`], except `exclude_user_id` never receives it.
    pub async fn announce(&self, exclude_user_id: u16, tx: Transaction) {
        for (&user_id, recipient) in self.recipients.read().await.iter() {
            if user_id != exclude_user_id {
                recipient.try_send(RecipientMessage::Broadcast(tx.clone()));
            }
        }
    }

    /// Push `update` to every session; each one applies it if it is logged in
    /// under `update.login`.
    pub(crate) async fn push_access_update(&self, update: AccessUpdate) {
        for recipient in self.recipients.read().await.values() {
            recipient.try_send(RecipientMessage::AccessUpdate(update.clone()));
        }
    }

    /// Send a console-command-driven update to exactly one connected
    /// session. A no-op if that user isn't connected.
    pub async fn send_console_update(&self, target_user_id: u16, update: ConsoleUpdate) {
        if let Some(recipient) = self.recipients.read().await.get(&target_user_id) {
            recipient.try_send(RecipientMessage::Console(update));
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
        Ok(target.name.into())
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
                name: name.map(Into::into),
            },
        )
        .await;
        Ok(Some(target.name.into()))
    }

    /// Runs `command` (lowercased, without the prefix) via the
    /// configured [`ChatCommandDispatcher`]. `None` if unrecognized.
    pub async fn run_console_command(
        &self,
        command: &str,
        args: &str,
        actor: &ChatCommandActor,
    ) -> Option<ConsoleReply> {
        self.commands().run(command, args, actor, self).await
    }

    /// `login`'s own files, or the shared files. `Err` if its own files do
    /// not open.
    pub(crate) async fn files_for(
        &self,
        login: &str,
    ) -> Result<Arc<dyn FileProvider>, &'static str> {
        Ok(self
            .account_stores()
            .files(login)
            .await?
            .unwrap_or_else(|| self.providers.files.clone()))
    }

    /// `login`'s own threaded news, or the shared threaded news. `Err` if
    /// its own threaded news does not open.
    async fn threaded_news_for(
        &self,
        login: &str,
    ) -> Result<Arc<dyn ThreadedNewsProvider>, &'static str> {
        Ok(self
            .account_stores()
            .threaded_news(login)
            .await?
            .unwrap_or_else(|| self.providers.threaded_news.clone()))
    }

    /// List a directory as seen by `requester`. `dir` is empty for the
    /// root folder. A blocked drop box lists only its partial uploads.
    /// `Err` if the directory doesn't exist or isn't visible.
    pub async fn list_files(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        access: Access,
    ) -> Result<Vec<FileNameWithInfo>, &'static str> {
        let mut files = self
            .files_for(requester.login)
            .await?
            .list(requester, dir)
            .await
            .ok_or("file or folder not found.")?;
        if dropbox_blocked(dir, None, access) {
            files.retain(|file| file.file_type == PARTIAL_TYPE);
        }
        Ok(files)
    }

    /// A file's size in bytes, for starting a [`myTran_DownloadFile`] (202)
    /// download. `Err` if it doesn't exist, is a folder, or is a drop box
    /// `access` can't view.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub async fn file_size(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<u64, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let size = files
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
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<Vec<FolderItem>, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let items = files
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
        let Ok(files) = self.files_for(requester.login).await else {
            return false;
        };
        files.exists(requester, dir, name).await
    }

    /// Whether `requester` may start (or resume) a [`myTran_UploadFile`] (203)
    /// upload of `name` under `dir`. `dir` must exist, and must be under an
    /// "Upload"/"Drop Box" folder unless `upload_anywhere`. A finished file
    /// already at `name` rejects the request, and so does a partial upload
    /// for a new upload. A new upload of `size` bytes
    /// needs `UPLOAD_SPACE_MARGIN` more bytes free. With `resume` true, an
    /// in-progress upload is also required, and its current size is returned
    /// as the resume offset (`0` otherwise).
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    #[allow(clippy::too_many_arguments)]
    pub async fn authorize_upload(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
        resume: bool,
        size: u64,
    ) -> Result<u64, Cow<'static, str>> {
        if !self.folder_exists(files, requester, dir).await {
            Err("file or folder not found.")?;
        }
        upload_destination_access(dir, access)?;
        if files.exists(requester, dir, name).await {
            Err("file already exists.")?;
        }
        let partial = files.partial_upload_size(requester, dir, name).await;
        if resume {
            Ok(partial.ok_or("file or folder not found.")?)
        } else if partial.is_some() {
            Err("file already exists.")?
        } else {
            self.require_free_space(files, requester, dir, "file", name, size)
                .await?;
            Ok(0)
        }
    }

    /// Refuse an upload of the `kind` `name`, `size` bytes, into `dir` when
    /// the disk has less than `size` plus `UPLOAD_SPACE_MARGIN` free. An
    /// unknown free space is enough.
    async fn require_free_space(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        kind: &str,
        name: &str,
        size: u64,
    ) -> Result<(), String> {
        match files.free_space(requester, dir).await {
            Some(free) if size.saturating_add(UPLOAD_SPACE_MARGIN) > free => Err(format!(
                "Cannot accept upload of the {kind} \"{name}\" because there is not enough free disk space."
            )),
            _ => Ok(()),
        }
    }

    /// Whether `dir` is a folder that `requester` can see. The root always is.
    async fn folder_exists(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> bool {
        let Some((name, parent)) = dir.components.split_last() else {
            return true;
        };
        files
            .info(requester, &HlPath::new(parent), name)
            .await
            .is_some_and(|info| info.is_folder())
    }

    /// Start (or resume) a [`myTran_UploadFldr`] (213) upload of folder
    /// `name` under `dir`, creating the folder if it's missing. `dir` must
    /// exist, with the same destination rule as [`Self::authorize_upload`].
    /// Resuming needs a folder already at `name`; otherwise nothing may be
    /// there yet. The folder's `size` bytes need `UPLOAD_SPACE_MARGIN`
    /// more bytes free.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    #[allow(clippy::too_many_arguments)]
    pub async fn start_folder_upload(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
        resume: bool,
        size: u64,
    ) -> Result<(), Cow<'static, str>> {
        if !self.folder_exists(files, requester, dir).await {
            Err("file or folder not found.")?;
        }
        upload_destination_access(dir, access)?;
        let existing = files.info(requester, dir, name).await;
        if resume {
            let info = existing.ok_or("file or folder not found.")?;
            if !info.is_folder() {
                Err("file already exists.")?;
            }
        } else if existing.is_some() {
            Err("file already exists.")?;
        }
        self.require_free_space(files, requester, dir, "folder", name, size)
            .await?;
        if resume {
            return Ok(());
        }
        files
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
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        is_folder: bool,
    ) -> Option<u64> {
        files.list(requester, dir).await?;
        if is_folder {
            if !files.exists(requester, dir, name).await {
                // A failed folder still moves on to the next item.
                if let Err(error) = files.mkdir(requester, dir, name).await {
                    tracing::debug!(%error, name, "folder upload item failed");
                }
            }
            return None;
        }
        if let Some(offset) = files.partial_upload_size(requester, dir, name).await {
            return Some(offset);
        }
        (!files.exists(requester, dir, name).await).then_some(0)
    }

    /// The byte length of the resource fork of `name`'s in-progress upload.
    /// 0 if there is no in-progress upload.
    pub async fn partial_resource_fork_size(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> u64 {
        if files
            .partial_upload_size(requester, dir, name)
            .await
            .is_none()
        {
            return 0;
        }
        files
            .resource_fork_size(requester, dir, &partial_upload_name(name))
            .await
    }

    /// One file or folder's listing metadata. `Err` if missing or
    /// drop-box-blocked.
    pub async fn file_info(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<FileNameWithInfo, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = files
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
        let files = self.files_for(requester.login).await?;
        let info = files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let is_dir = info.is_folder();
        let required = if is_dir {
            Access::DELETE_FOLDER
        } else {
            Access::DELETE_FILE
        };
        access.require(required)?;
        files
            .delete(requester, dir, name, is_dir)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Create a folder, after checking drop box visibility.
    pub async fn mkdir(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        files
            .mkdir(requester, dir, name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Rename a file or folder in place, after checking the matching
    /// privilege and drop box visibility.
    pub async fn rename_file(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        new_name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let is_folder = info.is_folder();
        access.require(if is_folder {
            Access::RENAME_FOLDER
        } else {
            Access::RENAME_FILE
        })?;
        files
            .rename(requester, dir, name, new_name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Move a file or folder into another directory, keeping its name,
    /// after checking the matching privilege and the policy's drop box
    /// check.
    pub async fn move_file(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        to_dir: &HlPath,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        let files = self.files_for(requester.login).await?;
        self.drop_box_source(dir, name, access)?;
        let info = files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let required = if info.is_folder() {
            Access::MOVE_FOLDER
        } else {
            Access::MOVE_FILE
        };
        access.require(required)?;
        files
            .r#move(requester, dir, name, to_dir)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Create an alias at `link_dir`/`link_name` pointing at
    /// `target_dir`/`target_name`, after the policy's drop box check.
    #[allow(clippy::too_many_arguments)]
    pub async fn symlink_file(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        target_dir: &HlPath,
        target_name: &str,
        link_dir: &HlPath,
        link_name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        self.drop_box_source(target_dir, target_name, access)?;
        files
            .symlink(requester, target_dir, target_name, link_dir, link_name)
            .await
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// A file or folder's stored comment, or `None` if it's never had
    /// one set. `Err` if missing or drop-box-blocked.
    pub async fn file_comment(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<Option<String>, Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        Ok(files.comment(requester, dir, name).await)
    }

    /// Set a file or folder's comment, after checking the matching
    /// privilege and drop box visibility. A failed write is an error only
    /// when the policy stops on it.
    pub async fn set_file_comment(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        dropbox_access(dir, Some(name), access)?;
        let info = files
            .info(requester, dir, name)
            .await
            .ok_or("file or folder not found.")?;
        let required = if info.is_folder() {
            Access::SET_FOLDER_COMMENT
        } else {
            Access::SET_FILE_COMMENT
        };
        access.require(required)?;
        let Err(error) = files.set_comment(requester, dir, name, comment).await else {
            return Ok(());
        };
        tracing::debug!(%error, name, "comment write failed");
        self.policy()
            .error(Silent::CommentWrite {
                name: name.to_owned(),
                error,
            })
            .map_or(Ok(()), Err)
    }

    /// The policy's error for a source in a drop box that `access` cannot
    /// view.
    fn drop_box_source(
        &self,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        let Err(reason) = dropbox_access(dir, Some(name), access) else {
            return Ok(());
        };
        self.policy()
            .error(Silent::DropBoxSource { reason })
            .map_or(Ok(()), Err)
    }

    /// Register a pending transfer, returning the reference number to
    /// hand the client. Retries on the vanishingly unlikely case the
    /// counter wraps into a still-pending ref. An upload expires after
    /// `TRANSFER_IDLE_TIMEOUT`.
    pub async fn register_transfer(self: &Arc<Self>, transfer: PendingTransfer) -> TransferRef {
        let is_upload = transfer.direction == TransferDirection::Put;
        let reference = {
            let mut transfers = self.transfers.write().await;
            loop {
                let reference = self.new_transfer_ref(is_upload);
                if let Entry::Vacant(entry) = transfers.entry(reference) {
                    entry.insert(transfer);
                    break reference;
                }
            }
        };
        if is_upload {
            self.spawn_upload_idle_timeout(reference);
        }
        reference
    }

    /// A new transfer reference number: a 16-bit counter that skips 0, over
    /// 16 random bits. Bit 0 is clear for an upload and set for a download.
    fn new_transfer_ref(&self, is_upload: bool) -> TransferRef {
        let counter = self.transfer_counter.next().get();
        let reference = u32::from(counter) << 16 | random_u32() & 0xFFFF;
        TransferRef(if is_upload {
            reference & !1
        } else {
            reference | 1
        })
    }

    /// Drop upload `reference` if no `HTXF` connection arrives within
    /// [`TRANSFER_IDLE_TIMEOUT`].
    fn spawn_upload_idle_timeout(self: &Arc<Self>, reference: TransferRef) {
        let state = Arc::clone(self);
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

    /// Like [`Self::take_transfer`], but only if an `HTXF` header of
    /// `transfer_type` finds the transfer. Otherwise the transfer stays.
    pub(crate) async fn take_htxf_transfer(
        &self,
        reference: TransferRef,
        transfer_type: HtxfType,
    ) -> Option<PendingTransfer> {
        let mut transfers = self.transfers.write().await;
        transfers
            .get(&reference)
            .filter(|transfer| transfer.accepts(transfer_type))?;
        transfers.remove(&reference)
    }

    /// Track `reference`'s progress until the guard drops. `None` for a banner.
    pub(crate) fn start_progress(
        &self,
        reference: TransferRef,
        transfer: &PendingTransfer,
    ) -> Option<ProgressGuard> {
        self.running.start(reference, transfer)
    }

    /// `user_id`'s pending, running, and waiting transfers, oldest first.
    pub async fn transfer_report(&self, user_id: u16) -> TransferReport {
        let mut transfers = self.running.for_user(user_id);
        for (&reference, transfer) in self.transfers.read().await.iter() {
            if transfer.user_id != user_id {
                continue;
            }
            if let Some(progress) = TransferProgress::from_pending(transfer) {
                transfers.push((reference, progress));
            }
        }
        let (waiting, active) = transfers
            .into_iter()
            .partition(|(reference, _)| self.download_queue.is_waiting(*reference));
        let oldest_first = |mut list: Vec<(TransferRef, TransferProgress)>| {
            list.sort_by_key(|(reference, _)| reference.0);
            list.into_iter().map(|(_, progress)| progress).collect()
        };
        TransferReport {
            active: oldest_first(active),
            waiting: oldest_first(waiting),
        }
    }

    /// Admit a registered download. Returns its [`myField_WaitingCount`] (116),
    /// `0` if it is active now.
    ///
    /// [`myField_WaitingCount`]: hotline_proto::FieldId::WaitingCount
    pub async fn admit_download(self: &Arc<Self>, reference: TransferRef, user_id: u16) -> u32 {
        let waiting_count = self.download_queue.admit(reference, user_id);
        if waiting_count == 0 {
            self.spawn_download_idle_timeout(reference);
        }
        waiting_count
    }

    /// Free `reference`'s slot if no `HTXF` connection arrives within
    /// [`TRANSFER_IDLE_TIMEOUT`]. Restarts on promotion.
    fn spawn_download_idle_timeout(self: &Arc<Self>, reference: TransferRef) {
        let state = Arc::clone(self);
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
    pub async fn release_download_slot(self: &Arc<Self>, reference: TransferRef) {
        let updates = self.download_queue.release(reference);
        self.push_download_updates(updates).await;
    }

    /// Cancel `user_id`'s download `reference`, queued or running, like
    /// openline's `KillDownloadByRefNum`. Another user's download is left
    /// alone.
    pub async fn kill_download(self: &Arc<Self>, reference: TransferRef, user_id: u16) {
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
    async fn release_downloads_for_user(self: &Arc<Self>, user_id: u16) {
        let updates = self.download_queue.remove_user(user_id);
        self.push_download_updates(updates).await;
    }

    /// Push one [`myTran_DownloadInfo`] (211) per update, starting a freshly
    /// promoted entry's idle timeout.
    ///
    /// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
    async fn push_download_updates(self: &Arc<Self>, updates: Vec<QueueUpdate>) {
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

    /// The `INFO` fork data of the file `dir`/`name`, for a download.
    pub async fn info_fork(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        encoding: &'static Encoding,
    ) -> Vec<u8> {
        let finder_info = files.finder_info(requester, dir, name).await;
        let FinderInfo {
            file_type,
            creator,
            flags,
        } = finder_info.unwrap_or_default();
        let times = files.times(requester, dir, name).await;
        let comment = files.comment(requester, dir, name).await;
        FlatFileInfo {
            file_type,
            creator,
            platform_flags: flags.into(),
            created: self.wire_date(times.created),
            modified: self.wire_date(times.modified),
            name: name.into(),
            comment: comment.unwrap_or_default(),
            ..FlatFileInfo::default()
        }
        .encode(encoding)
    }

    /// Store an uploaded `INFO` fork on the file `dir`/`name`: its type,
    /// creator, and flags without the inited and alias flags, and its
    /// comment when not empty.
    pub async fn apply_info_fork(
        &self,
        files: &dyn FileProvider,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        info: &FlatFileInfo,
    ) {
        let finder_info = FinderInfo {
            file_type: info.file_type,
            creator: info.creator,
            flags: info.platform_flags as u16 & !FinderInfo::INITED & !FinderInfo::ALIAS,
        };
        let stored = files
            .set_finder_info(requester, dir, name, finder_info)
            .await;
        if let Err(error) = stored {
            tracing::debug!(%error, name, "could not store an upload's Finder info");
        }
        if !info.comment.is_empty() {
            let stored = files.set_comment(requester, dir, name, &info.comment).await;
            if let Err(error) = stored {
                tracing::debug!(%error, name, "could not store an upload's comment");
            }
        }
    }

    /// The currently configured banner, for the login-time
    /// [`myTran_ServerBanner`] (122) push.
    ///
    /// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
    pub async fn banner_kind(&self) -> Option<BannerKind> {
        self.banner().kind().await
    }

    /// Start a banner-image download over `HTXF`, returning `(byte size,
    /// reference)` to reply with. `Err` if no image banner is configured
    /// (a URL banner has nothing to fetch this way).
    pub async fn start_banner_transfer(
        self: &Arc<Self>,
        client_ip: IpAddr,
        user_id: u16,
    ) -> Result<(u64, TransferRef), &'static str> {
        match self.banner().kind().await {
            Some(BannerKind::Image { .. }) => {}
            _ => return Err("no banner is configured."),
        }
        let size = self
            .banner()
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
        self.banner().open_read().await
    }

    /// The full classic news text.
    pub async fn news(&self) -> Arc<str> {
        self.flat_news().load().await
    }

    /// Prepend a new post to the classic news. Returns the formatted entry.
    ///
    /// Matches openline's `AddToMsgBoard` (`HotlineServ.cp`) byte for byte,
    /// including its missing space between month and day.
    pub async fn post_news(&self, poster: &str, text: &str) -> String {
        use chrono::{Datelike, Timelike};

        const FOOTER_DIVIDER: &str = "__________________________________________________________";

        let now = match self.policy().time_zone() {
            TimeZone::Local => chrono::Local::now().naive_local(),
            TimeZone::Utc => chrono::Utc::now().naive_utc(),
        };
        let month = now.format("%b");
        let entry = format!(
            "From {poster} ({month}{day:02} {hour:02}:{minute:02}):\r\r{text}\r{FOOTER_DIVIDER}\r",
            day = now.day(),
            hour = now.hour(),
            minute = now.minute(),
        );
        self.flat_news().post(&entry).await;
        entry
    }

    /// List a threaded news bundle's own contents at `path`. `Err` if it
    /// doesn't resolve to a bundle.
    pub async fn news_list_dir(
        &self,
        login: &str,
        path: &HlPath,
    ) -> Result<Vec<NewsCategoryListEntry>, &'static str> {
        let entries = self
            .threaded_news_for(login)
            .await?
            .list_dir(&path.components)
            .await
            .ok_or("news bundle not found")?;
        Ok(entries
            .into_iter()
            .map(|(name, kind)| match kind {
                NewsListKind::Bundle { count } => NewsCategoryListEntry::Bundle {
                    count,
                    name: name.into(),
                },
                NewsListKind::Category(summary) => NewsCategoryListEntry::Category {
                    count: summary.count,
                    guid: summary.guid,
                    add_sn: summary.add_sn,
                    del_sn: summary.del_sn,
                    name: name.into(),
                },
            })
            .collect())
    }

    /// List a threaded news category's articles at `path`. `Err` if it
    /// doesn't resolve to a category.
    pub async fn news_list_articles(
        &self,
        login: &str,
        path: &HlPath,
        encoding: &'static Encoding,
    ) -> Result<NewsArticleListData, &'static str> {
        let articles = self
            .threaded_news_for(login)
            .await?
            .list_articles(&path.components)
            .await
            .ok_or("news category not found")?;
        Ok(NewsArticleListData {
            articles: articles
                .into_iter()
                .map(|(id, article)| NewsArticleSummary {
                    id,
                    date: self.wire_article_date(article.date),
                    parent_id: article.parent_id,
                    flags: article.flags,
                    title: article.title,
                    poster: article.poster,
                    flavors: article
                        .data
                        .iter()
                        .map(|(mime_type, data)| NewsFlavor {
                            mime_type: (**mime_type).into(),
                            size: u16::try_from(body_to_wire(mime_type, data, encoding).len())
                                .unwrap_or(u16::MAX),
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    /// One article's full data at `path`. `Err` if the category or the
    /// article itself doesn't exist.
    pub async fn news_article(
        &self,
        login: &str,
        path: &HlPath,
        id: u32,
    ) -> Result<NewsArticleDetail, &'static str> {
        self.threaded_news_for(login)
            .await?
            .article(&path.components, id)
            .await
            .ok_or("news article not found")
    }

    /// Create a news bundle (folder) named `name` under `path`.
    pub async fn news_mkdir(
        &self,
        login: &str,
        path: &HlPath,
        name: &str,
    ) -> Result<(), &'static str> {
        self.threaded_news_for(login)
            .await?
            .mkdir(&path.components, name)
            .await
    }

    /// Create a news category named `name` under `path`.
    pub async fn news_mkcategory(
        &self,
        login: &str,
        path: &HlPath,
        name: &str,
    ) -> Result<(), &'static str> {
        self.threaded_news_for(login)
            .await?
            .mkcategory(&path.components, name)
            .await
    }

    /// Delete the news bundle or category named by `path` (its last
    /// component is the item's own name), after checking the matching
    /// bundle/category privilege. `Err` for the root or a path that
    /// doesn't resolve.
    pub async fn news_delete(
        &self,
        login: &str,
        path: &HlPath,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        let news = self.threaded_news_for(login).await?;
        if path.components.is_empty() {
            Err("news item not found")?;
        }
        let kind = news
            .item_kind(&path.components)
            .await
            .ok_or("news item not found")?;
        let required = match kind {
            NewsItemKind::Bundle => Access::NEWS_DELETE_FLDR,
            NewsItemKind::Category => Access::NEWS_DELETE_CAT,
        };
        access.require(required)?;
        news.delete(&path.components).await?;
        Ok(())
    }

    /// Post a new article into the news category at `path`.
    #[allow(clippy::too_many_arguments)]
    pub async fn news_post_article(
        &self,
        login: &str,
        path: &HlPath,
        parent_id: u32,
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
    ) -> Result<u32, PostError> {
        self.threaded_news_for(login)
            .await?
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
        login: &str,
        path: &HlPath,
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str> {
        self.threaded_news_for(login)
            .await?
            .delete_article(&path.components, id, recursive)
            .await
    }

    /// Create a new private chat room with `creator` as its only member,
    /// returning the new room's ID.
    pub async fn create_chat_room(&self, creator: u16) -> u32 {
        let room = ChatRoom {
            members: vec![creator],
            ..ChatRoom::default()
        };
        self.chat_rooms().create(room).await
    }

    /// Current members of a chat room, in join order, if it exists.
    pub async fn chat_room_members(&self, chat_id: u32) -> Option<Vec<u16>> {
        self.chat_room(chat_id).await.map(|room| room.members)
    }

    /// Snapshot of a chat room (members and subject), if it exists.
    pub async fn chat_room(&self, chat_id: u32) -> Option<ChatRoom> {
        self.chat_rooms().room(chat_id).await
    }

    /// Add `user_id` to a private chat room, as the policy's [`ChatRooms`]
    /// lets it. `true` when it is a new member.
    pub async fn join_chat_room(
        &self,
        chat_id: u32,
        user_id: u16,
        password: &str,
    ) -> Result<bool, &'static str> {
        let rooms = self.policy().chat_rooms();
        if rooms == ChatRooms::Open && chat_id == 0 {
            return Err(CLOSED_CHAT);
        }
        self.chat_rooms()
            .with_room(chat_id, |room| match rooms {
                ChatRooms::Open => Ok(room.add_member(user_id)),
                ChatRooms::Invited => room.join(user_id, password).map(|()| true),
            })
            .await
            .unwrap_or(Err(match rooms {
                ChatRooms::Open => CLOSED_CHAT,
                ChatRooms::Invited => "no such chat room",
            }))
    }

    /// Add `user_id` to a chat room directly, no invite or password
    /// check. For internal auto-membership (room 0 on login) and test
    /// setup, not the [`myTran_JoinChat`] (115) request path.
    ///
    /// [`myTran_JoinChat`]: hotline_proto::Opcode::JoinChat
    pub(crate) async fn add_chat_member(&self, chat_id: u32, user_id: u16) {
        self.chat_rooms()
            .with_room(chat_id, |room| room.add_member(user_id))
            .await;
    }

    /// Mark `user_id` as invited to a chat room. Returns whether this was
    /// a new invite (`false` if already invited, or if the room doesn't
    /// exist).
    pub async fn invite(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms()
            .with_room(chat_id, |room| room.invited.insert(user_id))
            .await
            .unwrap_or(false)
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
        self.chat_rooms()
            .with_room(chat_id, |room| room.invite_by(inviter, target_id))
            .await
            .unwrap_or(Err("no such chat room"))
    }

    /// Clear a pending invite without joining, e.g. on decline. Returns
    /// whether there was one to clear.
    pub async fn invite_declined(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms()
            .with_room(chat_id, |room| room.invited.remove(&user_id))
            .await
            .unwrap_or(false)
    }

    /// Whether `user_id` has a pending invite to a chat room.
    pub async fn is_invited(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_room(chat_id)
            .await
            .is_some_and(|room| room.invited.contains(&user_id))
    }

    /// Remove `user_id` from a chat room, deleting the room once its
    /// last member leaves. Chat ID `0` (the main room) is never deleted.
    /// Returns the remaining members, or `None` if `user_id` was not a
    /// member.
    pub async fn leave_chat_room(&self, chat_id: u32, user_id: u16) -> Option<Vec<u16>> {
        let mut remaining = None;
        let mut change = |room: &mut ChatRoom| {
            remaining = room.leave(user_id);
            remaining.is_none() || chat_id == 0 || !room.members.is_empty()
        };
        self.chat_rooms().update(chat_id, &mut change).await;
        remaining
    }

    /// Remove `user_id` from every chat room, deleting any private room
    /// that becomes empty. Returns each room's ID and remaining members.
    pub async fn leave_all_chat_rooms(&self, user_id: u16) -> Vec<(u32, Vec<u16>)> {
        let mut left = Vec::new();
        let mut change = |chat_id, room: &mut ChatRoom| {
            let Some(remaining) = room.leave(user_id) else {
                return true;
            };
            left.push((chat_id, remaining));
            chat_id == 0 || !room.members.is_empty()
        };
        self.chat_rooms().update_each(&mut change).await;
        left
    }

    /// Set a chat room's subject. `false` if the room doesn't exist.
    pub async fn set_chat_subject(&self, chat_id: u32, subject: &str) -> bool {
        self.chat_rooms()
            .with_room(chat_id, |room| room.subject = subject.into())
            .await
            .is_some()
    }

    /// Set a chat room's join password. `false` if the room doesn't exist.
    pub async fn set_chat_password(&self, chat_id: u32, password: &str) -> bool {
        self.chat_rooms()
            .with_room(chat_id, |room| room.password = password.into())
            .await
            .is_some()
    }

    /// Whether `user_id` may change a chat room's subject/password, as the
    /// policy's [`ChatRooms`] lets it.
    pub async fn authorize_chat_subject_change(
        &self,
        chat_id: u32,
        user_id: u16,
        is_global: bool,
        access_extra: AccessExtra,
    ) -> Result<(), Cow<'static, str>> {
        let members = self.chat_room_members(chat_id).await;
        if self.policy().chat_rooms() == ChatRooms::Open {
            return match members {
                Some(_) if !is_global => Ok(()),
                _ => Err("no such chat room")?,
            };
        }
        if is_global {
            access_extra.require(AccessExtra::SET_SUBJECT)?;
            return Ok(());
        }
        match members {
            Some(members) if members.contains(&user_id) => Ok(()),
            Some(_) => Err("you are not in that chat room")?,
            None => Err("no such chat room")?,
        }
    }

    /// Look up an account by login.
    pub async fn account(&self, login: &str) -> Option<Account> {
        self.accounts().get(login).await
    }

    /// The stored account for `login`, which `Err` names.
    pub async fn require_account(&self, login: &str) -> Result<Account, Cow<'static, str>> {
        let account = self.account(login).await;
        Ok(account.ok_or_else(|| format!("Cannot read account “{login}”."))?)
    }

    /// Create or overwrite an account.
    pub async fn put_account(
        &self,
        login: impl Into<Box<str>>,
        account: Account,
    ) -> Result<(), Cow<'static, str>> {
        let login = login.into();
        let _writes = self.account_writes.lock().await;
        self.accounts().put(login, account).await
    }

    /// Create the account `login`. `Err` if the login is empty or taken.
    /// An empty login gets the error for a taken login.
    pub async fn create_account(
        &self,
        login: impl Into<Box<str>>,
        account: Account,
    ) -> Result<(), Cow<'static, str>> {
        let login = login.into();
        let _writes = self.account_writes.lock().await;
        if login.is_empty() || self.accounts().get(&login).await.is_some() {
            Err(format!(
                "Cannot create account “{login}” because there is already an account with that login."
            ))?;
        }
        self.accounts().put(login, account).await
    }

    /// Save `edit` of the account at `old_login` under `login`. `Err` if
    /// `old_login` is gone, if `login` is empty, or if a rename's `login` is
    /// taken and the policy gives [`TakenLogin::Refuse`]. A rename goes
    /// through [`AccountProvider::rename`], and its error tells whether the
    /// account moved. With [`TakenLogin::Overwrite`], a rename onto a taken
    /// login writes `edit` of the `old_login` account over that account, and
    /// keeps `old_login`.
    pub(crate) async fn replace_account(
        &self,
        old_login: &str,
        login: impl Into<Box<str>>,
        edit: impl FnOnce(Account) -> Account,
    ) -> Result<ReplacedAccount, NotReplaced> {
        let login = login.into();
        let _writes = self.account_writes.lock().await;
        let Some(existing) = self.accounts().get(old_login).await else {
            Err(format!("No account with login “{old_login}”."))?
        };
        if login.is_empty() {
            Err("Error saving list of accounts.")?;
        }
        let renamed = old_login != &*login;
        let over_taken_login = renamed && self.accounts().get(&login).await.is_some();
        if over_taken_login && self.policy().taken_login() == TakenLogin::Refuse {
            Err("that login is taken")?;
        }
        let account = edit(existing);
        if renamed && !over_taken_login {
            if let Err(reason) = self
                .accounts()
                .rename(old_login, login.clone(), account.clone())
                .await
            {
                let moved = self.accounts().get(old_login).await.is_none()
                    && self.accounts().get(&login).await.is_some();
                return Err(NotReplaced { reason, moved });
            }
        } else {
            self.accounts().put(login, account.clone()).await?;
        }
        Ok(ReplacedAccount {
            account,
            over_taken_login,
        })
    }

    /// The active ban on `ip`, if any; see [`crate::BanProvider::is_banned`].
    pub async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord> {
        self.bans().is_banned(ip).await
    }

    /// `Err` with the reason a login fails if `ip` is banned.
    pub async fn require_not_banned(&self, ip: IpAddr) -> Result<(), &'static str> {
        match self.is_banned(ip).await {
            None => Ok(()),
            Some(BanRecord { until: None, .. }) => {
                Err("You are permanently banned on this server.")
            }
            Some(_) => Err("You are temporarily banned on this server."),
        }
    }

    /// Store `record`, which replaces any ban on its address.
    pub async fn ban(&self, record: BanRecord) -> Result<(), Cow<'static, str>> {
        self.bans().ban(record).await
    }

    /// Ban `ip` for [`TEMP_BAN_DURATION`], logging a failure. A ban on `ip`
    /// that lasts longer stays.
    pub(crate) async fn temp_ban(&self, ip: IpAddr) {
        let now = std::time::SystemTime::now();
        let until = now.checked_add(TEMP_BAN_DURATION).unwrap_or(now);
        let longer = self
            .is_banned(ip)
            .await
            .is_some_and(|ban| ban.until.is_none_or(|end| end >= until));
        if longer {
            return;
        }
        if let Err(error) = self.ban(BanRecord::new(ip, Some(until))).await {
            tracing::warn!(%ip, %error, "failed to persist ban");
        }
    }

    /// Lift `ip`'s ban. `Err` if it isn't banned.
    pub async fn unban(&self, ip: IpAddr) -> Result<(), Cow<'static, str>> {
        self.bans()
            .unban(ip)
            .await?
            .ok_or("address is not banned")?;
        Ok(())
    }

    /// Record one `event` from `user_id`; see [`crate::FloodPolicy::check`].
    pub async fn check_flood(&self, user_id: u16, event: FloodEvent<'_>) -> FloodDecision {
        self.flood().check(user_id, event).await
    }

    /// Record one new connection from `ip`; see
    /// [`crate::FloodPolicy::check_connection`].
    pub async fn check_connection_flood(&self, ip: IpAddr) -> FloodDecision {
        self.flood().check_connection(ip).await
    }

    /// Forget `user_id`'s accumulated flood state, called on disconnect.
    pub async fn reset_flood(&self, user_id: u16) {
        self.flood().reset(user_id).await;
    }

    /// Delete an account. The error names `login`, as openline's does.
    pub async fn delete_account(&self, login: &str) -> Result<(), Cow<'static, str>> {
        let _writes = self.account_writes.lock().await;
        match self.accounts().delete(login).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(format!("No account with login “{login}”."))?,
            Err(error) => Err(format!("Cannot delete account “{login}”:\r{error}"))?,
        }
    }

    /// Every account, login and details together, sorted by login for a
    /// deterministic listing order, [`myTran_GetUserList`] (348).
    ///
    /// [`myTran_GetUserList`]: hotline_proto::Opcode::GetUserList
    pub async fn list_accounts(&self) -> Vec<(Box<str>, Account)> {
        let mut accounts = self.accounts().list().await;
        accounts.sort_by(|(a, _), (b, _)| a.cmp(b));
        accounts
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::file::InMemoryFileProvider;
    use crate::news::InMemoryNewsProvider;

    #[tokio::test]
    async fn a_running_transfer_reports_its_bytes_until_its_guard_drops() {
        let state = Arc::<SharedState>::default();
        let transfer = PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "bob".into(),
                    store: state.files_for("bob").await.unwrap(),
                    dir: HlPath::default(),
                    name: "a.txt".into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
            }),
            direction: TransferDirection::Put,
            size: 1000,
            resume_offset: 0,
            client_ip: "192.0.2.1".parse().unwrap(),
            user_id: 2,
        };
        let reference = state.register_transfer(transfer).await;
        let pending = state.transfer_report(2).await;
        assert_eq!(pending.active.len(), 1);
        assert_eq!(pending.active[0].done, 0);

        let taken = state.take_transfer(reference).await.unwrap();
        let guard = state.start_progress(reference, &taken).unwrap();
        guard.counter().fetch_add(400, Ordering::Relaxed);
        let running = state.transfer_report(2).await;
        assert_eq!(running.active.len(), 1);
        assert_eq!(running.active[0].done, 400);
        assert!(state.transfer_report(3).await.active.is_empty());

        drop(guard);
        assert_eq!(state.transfer_report(2).await, TransferReport::default());
    }

    #[tokio::test]
    async fn a_connected_download_that_waits_for_a_slot_reports_as_waiting() {
        let state = Arc::<SharedState>::default();
        state.set_download_limits(1, 1);
        let transfer = PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "bob".into(),
                    store: state.files_for("bob").await.unwrap(),
                    dir: HlPath::default(),
                    name: "a.txt".into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
            }),
            direction: TransferDirection::Get,
            size: 1000,
            resume_offset: 0,
            client_ip: "192.0.2.1".parse().unwrap(),
            user_id: 2,
        };
        let first = state.register_transfer(transfer.clone()).await;
        assert_eq!(state.admit_download(first, 2).await, 0);
        let second = state.register_transfer(transfer).await;
        assert_eq!(state.admit_download(second, 2).await, 1);

        let taken = state.take_transfer(second).await.unwrap();
        let _guard = state.start_progress(second, &taken).unwrap();
        let report = state.transfer_report(2).await;
        assert_eq!(report.active.len(), 1);
        assert_eq!(report.waiting.len(), 1);
    }

    #[tokio::test]
    async fn connect_counts_up_and_skips_0_when_it_wraps() {
        let state = SharedState::default();
        let ids = |connection: Option<Connection>| connection.map(|c| c.user_id);
        assert_eq!(ids(state.connect().await), Some(1));
        state.remove_recipient(1).await;
        state.user_id_counter.0.store(u16::MAX, Ordering::Relaxed);
        assert_eq!(ids(state.connect().await), Some(u16::MAX));
        assert_eq!(ids(state.connect().await), Some(1));
    }

    #[tokio::test]
    async fn connect_skips_a_user_id_that_a_connection_holds() {
        let state = SharedState::default();
        let _held = state.register_recipient(2).await;
        let first = state.connect().await.unwrap();
        let second = state.connect().await.unwrap();
        assert_eq!((first.user_id, second.user_id), (1, 3));
    }

    #[tokio::test]
    async fn announce_quit_sends_a_disconnect_msg_to_each_session() {
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;

        state.announce_quit("Back soon.").await;

        let push = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .unwrap()
            .received(hotline_proto::text_encoding::DEFAULT);
        assert_eq!(
            push.r#type(),
            hotline_proto::opcode::TransactionType::Push(Opcode::DisconnectMsg)
        );
        assert_eq!(push.string(FieldId::Data).as_deref(), Some("Back soon."));
    }

    #[tokio::test]
    async fn connections_closed_resolves_once_each_inbox_drops() {
        let state = Arc::new(SharedState::default());
        let rx = state.register_recipient(1).await;
        let waiting = tokio::spawn(async move { state.connections_closed().await });
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        drop(rx);

        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn connection_count_includes_a_connection_before_login() {
        let state = Arc::<SharedState>::default();
        let _rx = state.register_recipient(1).await;
        assert!(state.users().await.is_empty());
        assert_eq!(state.connection_count().await, 1);

        state.remove_recipient(1).await;
        assert_eq!(state.connection_count().await, 0);
    }

    /// Two `SharedState`s sharing a news provider see each other's writes.
    #[tokio::test]
    async fn news_is_visible_across_shared_states_backed_by_the_same_provider() {
        let provider = Arc::new(InMemoryNewsProvider::default());
        let state = SharedState::new(Providers {
            flat_news: provider.clone(),
            threaded_news: provider.clone(),
            ..Providers::default()
        });

        state.post_news("alice", "hello").await;
        state
            .news_mkcategory("", &HlPath::default(), "General")
            .await
            .unwrap();
        state
            .news_post_article(
                "",
                &HlPath::new(["General"]),
                0,
                "Hi".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"hello".as_slice().into(),
            )
            .await
            .unwrap();

        let other = SharedState::new(Providers {
            flat_news: provider.clone(),
            threaded_news: provider,
            ..Providers::default()
        });

        assert!(other.news().await.contains("hello"));
        let articles = other
            .news_list_articles(
                "",
                &HlPath::new(["General"]),
                hotline_proto::text_encoding::DEFAULT,
            )
            .await
            .unwrap();
        assert_eq!(articles.articles.len(), 1);
        assert_eq!(&*articles.articles[0].title, "Hi");
    }

    /// Own files and threaded news for `alice` only.
    struct AliceStores {
        files: Arc<InMemoryFileProvider>,
        news: Arc<InMemoryNewsProvider>,
    }

    #[async_trait::async_trait]
    impl crate::AccountStores for AliceStores {
        async fn files(&self, login: &str) -> Result<Option<Arc<dyn FileProvider>>, &'static str> {
            Ok((login == "alice").then(|| self.files.clone() as Arc<dyn FileProvider>))
        }

        async fn threaded_news(
            &self,
            login: &str,
        ) -> Result<Option<Arc<dyn ThreadedNewsProvider>>, &'static str> {
            Ok((login == "alice").then(|| self.news.clone() as Arc<dyn ThreadedNewsProvider>))
        }
    }

    /// Own stores for `alice` that do not open.
    struct BrokenAliceStores;

    #[async_trait::async_trait]
    impl crate::AccountStores for BrokenAliceStores {
        async fn files(&self, login: &str) -> Result<Option<Arc<dyn FileProvider>>, &'static str> {
            match login {
                "alice" => Err("broken files"),
                _ => Ok(None),
            }
        }

        async fn threaded_news(
            &self,
            login: &str,
        ) -> Result<Option<Arc<dyn ThreadedNewsProvider>>, &'static str> {
            match login {
                "alice" => Err("broken news"),
                _ => Ok(None),
            }
        }
    }

    #[tokio::test]
    async fn an_account_whose_own_stores_do_not_open_gets_an_error() {
        let state = SharedState::new(Providers {
            account_stores: Arc::new(BrokenAliceStores),
            ..Providers::default()
        });
        let alice = FileRequester { login: "alice" };
        let root = HlPath::default();

        assert_eq!(
            state.list_files(alice, &root, Access::all()).await,
            Err("broken files")
        );
        assert_eq!(state.files_for("alice").await.err(), Some("broken files"));
        assert_eq!(
            state.news_mkcategory("alice", &root, "Own").await,
            Err("broken news")
        );

        let bob = FileRequester { login: "bob" };
        let bob_files = state.files_for("bob").await.unwrap();
        state
            .mkdir(&*bob_files, bob, &root, "New", Access::all())
            .await
            .unwrap();
        assert!(state.file_exists(bob, &root, "New").await);
        state.news_mkcategory("bob", &root, "Shared").await.unwrap();
        assert_eq!(state.news_list_dir("bob", &root).await.unwrap().len(), 1);
    }

    fn state_with_alice_stores() -> (
        Arc<SharedState>,
        Arc<InMemoryFileProvider>,
        Arc<InMemoryNewsProvider>,
    ) {
        let files = Arc::new(InMemoryFileProvider::default());
        let news = Arc::new(InMemoryNewsProvider::default());
        let state = SharedState::new(Providers {
            account_stores: Arc::new(AliceStores {
                files: files.clone(),
                news: news.clone(),
            }),
            ..Providers::default()
        });
        (state, files, news)
    }

    #[tokio::test]
    async fn an_account_with_its_own_files_lists_only_them() {
        let (state, files, _) = state_with_alice_stores();
        files.seed(&HlPath::default(), "own.txt", "x");
        let state = &state;
        let list = |login| async move {
            state
                .list_files(FileRequester { login }, &HlPath::default(), Access::all())
                .await
                .unwrap()
                .into_iter()
                .map(|file| file.name.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(list("alice").await, ["own.txt"]);
        assert!(list("bob").await.is_empty());
    }

    #[tokio::test]
    async fn an_account_with_its_own_threaded_news_writes_only_there() {
        let (state, _, news) = state_with_alice_stores();

        state
            .news_mkcategory("alice", &HlPath::default(), "Own")
            .await
            .unwrap();

        let shared = state
            .news_list_dir("bob", &HlPath::default())
            .await
            .unwrap();
        assert!(shared.is_empty());
        let own = news.list_dir(&[]).await.unwrap();
        assert_eq!(own.len(), 1);
    }

    #[tokio::test]
    async fn temp_ban_keeps_a_permanent_ban() {
        let state = Arc::<SharedState>::default();
        let ip = IpAddr::from([10, 0, 0, 1]);
        state.ban(BanRecord::new(ip, None)).await.unwrap();

        state.temp_ban(ip).await;

        assert_eq!(state.is_banned(ip).await.unwrap().until, None);
    }

    /// Matches openline's `AddToMsgBoard` header, body, and footer shape,
    /// with no `<` byte.
    #[tokio::test]
    async fn post_news_matches_openlines_header_and_footer_shape() {
        let state = Arc::<SharedState>::default();

        let entry = state.post_news("alice", "hello").await;

        let footer = "_".repeat(58);
        assert!(entry.starts_with("From alice ("), "{entry:?}");
        assert!(entry.contains("):\r\rhello\r"), "{entry:?}");
        assert!(entry.ends_with(&format!("\r{footer}\r")), "{entry:?}");
        assert!(!entry.contains('<'), "{entry:?}");
    }

    /// `users()` comes in user ID order, not in insertion order.
    #[tokio::test]
    async fn users_are_ordered_by_user_id_not_insertion_order() {
        let state = Arc::<SharedState>::default();
        for id in [3, 1, 2] {
            state
                .insert_user(UserEntry {
                    user_id: id,
                    login: Box::default(),
                    account_name: Box::default(),
                    name: format!("user{id}").into(),
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
        let state = Arc::<SharedState>::default();
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
    async fn leave_all_chat_rooms_keeps_the_main_room() {
        let state = Arc::<SharedState>::default();
        state.add_chat_member(0, 1).await;

        let left = state.leave_all_chat_rooms(1).await;

        assert_eq!(left, [(0, vec![])]);
        state.add_chat_member(0, 2).await;
        assert_eq!(state.chat_room_members(0).await, Some(vec![2]));
    }

    #[tokio::test]
    async fn leave_chat_room_by_a_non_member_keeps_an_empty_room() {
        let state = Arc::<SharedState>::default();
        let chat_id = state.chat_rooms().create(ChatRoom::default()).await;

        assert_eq!(state.leave_chat_room(chat_id, 1).await, None);
        assert!(state.chat_room(chat_id).await.is_some());
    }

    #[tokio::test]
    async fn global_room_members_are_tracked_in_join_order() {
        let state = Arc::<SharedState>::default();
        for id in [3, 1, 2] {
            state
                .insert_user(UserEntry {
                    user_id: id,
                    login: Box::default(),
                    account_name: Box::default(),
                    name: format!("user{id}").into(),
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
        let state = Arc::<SharedState>::default();
        state.leave_chat_room(0, 1).await;
        assert!(state.chat_room(0).await.is_some());
    }

    #[tokio::test]
    async fn list_files_lists_the_root_directory() {
        let files = Arc::new(InMemoryFileProvider::default());
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["sub"]), "inner.txt", *b"x");
        let state = SharedState::new(Providers {
            files,
            ..Providers::default()
        });

        let requester = FileRequester { login: "alice" };
        let mut entries = state
            .list_files(requester, &HlPath::default(), Access::all())
            .await
            .unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(&*entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
        assert_eq!(&*entries[1].name, "sub");
        assert_eq!(&entries[1].file_type, b"fldr");
    }

    fn state_with_files(files: InMemoryFileProvider) -> Arc<SharedState> {
        SharedState::new(Providers {
            files: Arc::new(files),
            ..Providers::default()
        })
    }

    #[tokio::test]
    async fn create_account_refuses_a_taken_login() {
        let state = Arc::<SharedState>::default();
        state.create_account("bob", Account::guest()).await.unwrap();
        assert!(state.create_account("bob", Account::guest()).await.is_err());
    }

    #[tokio::test]
    async fn replace_account_refuses_a_deleted_login_and_writes_a_taken_rename_over_it() {
        let state = Arc::<SharedState>::default();
        assert!(
            state
                .replace_account("bob", "bob", |_| Account::guest())
                .await
                .is_err()
        );
        assert!(state.account("bob").await.is_none());

        state
            .put_account("bob", Account::admin("bob's"))
            .await
            .unwrap();
        state.put_account("amy", Account::guest()).await.unwrap();
        let replaced = state
            .replace_account("bob", "amy", |bob| Account {
                name: "Edited".into(),
                ..bob
            })
            .await
            .unwrap();
        assert!(replaced.over_taken_login);
        assert!(state.account("bob").await.is_some());
        assert_eq!(
            state.account("amy").await.unwrap(),
            Account {
                name: "Edited".into(),
                ..Account::admin("bob's")
            }
        );

        let replaced = state
            .replace_account("bob", "bobby", |_| Account::guest())
            .await
            .unwrap();
        assert!(!replaced.over_taken_login);
        assert!(state.account("bob").await.is_none());
        assert!(state.account("bobby").await.is_some());
    }

    #[tokio::test]
    async fn walk_folder_skips_a_drop_box_and_its_contents_without_view_drop_boxes() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"a");
        files.seed(&HlPath::new(["Top", "My Drop Box"]), "secret.txt", *b"s");
        let state = state_with_files(files);
        let requester = FileRequester { login: "alice" };
        let store = state.files_for("alice").await.unwrap();
        let root = HlPath::default();
        let walk = |access| state.walk_folder(&*store, requester, &root, "Top", access);

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
        let store = state.files_for("alice").await.unwrap();
        assert!(
            state
                .walk_folder(
                    &*store,
                    requester,
                    &HlPath::default(),
                    "Drop Box",
                    Access::empty()
                )
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn an_in_progress_upload_is_listed_under_its_real_hpf_name() {
        let files = Arc::new(InMemoryFileProvider::default());
        let state = SharedState::new(Providers {
            files,
            ..Providers::default()
        });
        let requester = FileRequester { login: "alice" };
        let store = state.files_for("alice").await.unwrap();

        {
            let mut writer = store
                .create_write(requester, &HlPath::default(), "a.txt")
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
        assert_eq!(&*entries[0].name, "a.txt.hpf");
        assert_eq!(entries[0].file_type, crate::file::PARTIAL_TYPE);
        assert_eq!(entries[0].creator, crate::file::PARTIAL_CREATOR);

        let info = state
            .file_info(
                &*store,
                requester,
                &HlPath::default(),
                "a.txt.hpf",
                Access::all(),
            )
            .await
            .unwrap();
        assert_eq!(info.file_type, crate::file::PARTIAL_TYPE);
        assert_eq!(info.creator, crate::file::PARTIAL_CREATOR);

        store
            .finish_upload(requester, &HlPath::default(), "a.txt")
            .await
            .unwrap();
        let info = state
            .file_info(
                &*store,
                requester,
                &HlPath::default(),
                "a.txt",
                Access::all(),
            )
            .await
            .unwrap();
        assert_ne!(info.file_type, crate::file::PARTIAL_TYPE);
    }

    #[test]
    fn new_transfer_ref_counts_in_the_high_bits_and_marks_the_direction() {
        let state = SharedState::default();
        let download = state.new_transfer_ref(false).0;
        let upload = state.new_transfer_ref(true).0;
        assert_eq!((download >> 16, download & 1), (1, 1));
        assert_eq!((upload >> 16, upload & 1), (2, 0));

        state.transfer_counter.0.store(u16::MAX, Ordering::Relaxed);
        assert_eq!(state.new_transfer_ref(false).0 >> 16, u32::from(u16::MAX));
        assert_eq!(state.new_transfer_ref(false).0 >> 16, 1);
    }

    #[tokio::test]
    async fn register_transfer_allocates_distinct_refs_and_take_consumes_them() {
        let state = Arc::<SharedState>::default();
        let store = state.files_for("alice").await.unwrap();
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".into(),
                    store: store.clone(),
                    dir: HlPath::default(),
                    name: "x".into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
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
        let state = Arc::<SharedState>::default();
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(crate::state::FileTransfer {
                    target: crate::state::TransferTarget {
                        login: "alice".into(),
                        store: state.files_for("alice").await.unwrap(),
                        dir: HlPath::default(),
                        name: "x".into(),
                    },
                    raw: false,
                    encoding: hotline_proto::text_encoding::DEFAULT,
                    info: Vec::new(),
                    resource_size: 0,
                    resource_offset: 0,
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
        let state = Arc::<SharedState>::default();
        state.set_download_limits(1, 10);
        let store = state.files_for("alice").await.unwrap();
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".into(),
                    store: store.clone(),
                    dir: HlPath::default(),
                    name: "x".into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
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
        let state = Arc::<SharedState>::default();
        state.set_download_limits(1, 10);
        let store = state.files_for("alice").await.unwrap();
        let make = || PendingTransfer {
            kind: TransferKind::File(crate::state::FileTransfer {
                target: crate::state::TransferTarget {
                    login: "alice".into(),
                    store: store.clone(),
                    dir: HlPath::default(),
                    name: "x".into(),
                },
                raw: false,
                encoding: hotline_proto::text_encoding::DEFAULT,
                info: Vec::new(),
                resource_size: 0,
                resource_offset: 0,
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
            login: "bob".into(),
            account_name: "Bob".into(),
            name: "Bob".into(),
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
        let state = Arc::<SharedState>::default();
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
        let state = Arc::<SharedState>::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_access_fields(2, "broadcast=1").await;

        assert_eq!(result, Err("unknown argument \"broadcast\"".into()));
    }

    /// mhxd matches console keys with `strcmp`.
    #[tokio::test]
    async fn apply_access_fields_console_key_is_case_sensitive() {
        let state = Arc::<SharedState>::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_access_fields(2, "DELETE_FILES=1").await;

        assert_eq!(result, Err("unknown argument \"DELETE_FILES\"".into()));
    }

    /// mhxd stops at a word with no `=` and still applies what came before.
    #[tokio::test]
    async fn apply_access_fields_stops_at_a_word_without_equals() {
        let state = Arc::<SharedState>::default();
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
        let state = Arc::<SharedState>::default();
        let non_volatile = AccessExtra::default() - AccessExtra::ACCESS_VOLATILE;
        state.insert_user(test_user_entry(2, non_volatile)).await;

        let result = state.apply_access_fields(2, "broadcast=1").await;

        assert_eq!(result, Err("user cannot be modified".into()));
    }

    #[tokio::test]
    async fn apply_0wn_fields_sets_icon_and_name() {
        let state = Arc::<SharedState>::default();
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
        let state = Arc::<SharedState>::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::empty()))
            .await;

        let result = state.apply_0wn_fields(2, "icon=5").await;

        assert_eq!(result, Err("User cannot be 0wned".into()));
    }

    /// mhxd's `user_0wn` ignores a key it doesn't know.
    #[tokio::test]
    async fn apply_0wn_fields_ignores_an_unknown_key() {
        let state = Arc::<SharedState>::default();
        state
            .insert_user(test_user_entry(2, AccessExtra::default()))
            .await;

        let result = state.apply_0wn_fields(2, "color=1").await;

        assert_eq!(result, Ok(None));
    }

    /// mhxd reads the icon with `strtoul`, so a non-number is `0`.
    #[tokio::test]
    async fn apply_0wn_fields_reads_the_icon_like_strtoul() {
        let state = Arc::<SharedState>::default();
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
        let state = Arc::<SharedState>::default();
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
        assert_eq!(update.name.as_deref(), long.get(..31));
    }
}
