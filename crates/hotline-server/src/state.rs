//! Shared, cross-session server state: the connected-user table, chat
//! rooms, classic news, and the one per-recipient channel sessions use to
//! reach each other.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, Mutex, RwLock};

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::types::{
    FileNameWithInfo, HlDate, HlPath, NewsArticleListData, NewsArticleSummary,
    NewsCategoryListEntry, UserFlags,
};
use hotline_proto::Transaction;

use crate::accounts::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
use crate::bans::{BanProvider, BanRecord, InMemoryBanProvider};
use crate::files::{BoxedReader, BoxedWriter, FileProvider, FileRequester, InMemoryFileProvider};
use crate::news::{InMemoryNewsProvider, NewsItemKind, NewsListKind, NewsProvider};

/// How long without any request before a user is considered idle.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// One connected user, as tracked by the server.
#[derive(Debug, Clone)]
pub struct UserEntry {
    /// Socket/user ID assigned at connect time.
    pub user_id: u16,
    /// Account login this session authenticated as. Also kept in
    /// [`crate::session::Session`].
    pub login: String,
    /// The account's own registered display name, which can differ from
    /// `name` if `USE_ANY_NAME` lets this session pick its own.
    pub account_name: String,
    /// Display name (post `SET_CLIENT_USER_INFO`).
    pub name: String,
    /// Icon ID.
    pub icon: u16,
    /// Client-reported protocol version (`HTLC_DATA_CLIENTVERSION`, field
    /// 160), `None` if the login request never sent one.
    pub client_version: Option<u16>,
    /// Remote socket address this connection was accepted from.
    pub remote_addr: SocketAddr,
    /// When this connection was accepted.
    pub connected_at: Instant,
    /// Privileges granted at login. Also kept in
    /// [`crate::session::Session`], which is what handlers check against.
    pub access: Access,
    /// Server-side-only account settings granted at login, also kept in
    /// [`crate::session::Session`].
    pub access_extra: AccessExtra,
    /// Last time this user sent any request. Compared against
    /// [`IDLE_THRESHOLD`] by [`UserEntry::is_idle`].
    pub last_active: Instant,
    /// Refuses incoming private messages. Also kept in
    /// [`crate::session::Session`].
    pub refuses_pm: bool,
    /// Refuses incoming private chat invites. Also kept in
    /// [`crate::session::Session`].
    pub refuses_chat: bool,
    /// Custom automatic-response text sent to private-message senders,
    /// `None` when the feature is off. Also kept in
    /// [`crate::session::Session`].
    pub automatic_response: Option<String>,
}

impl UserEntry {
    /// `true` if this user hasn't sent a request in over
    /// [`IDLE_THRESHOLD`].
    pub fn is_idle(&self) -> bool {
        self.last_active.elapsed() > IDLE_THRESHOLD
    }

    /// Current `UserFlags` bits: admin (from `DISCONNECT_USERS`), idle, and
    /// whether this user holds every/no currently-defined privilege.
    pub fn flags(&self) -> u16 {
        let mut bits = if self.access.contains(Access::DISCON_USER) {
            UserFlags::ADMIN.bits()
        } else {
            0
        };
        if self.is_idle() {
            bits |= UserFlags::AWAY.bits();
        }
        if self.access == Access::all() - Access::NO_AGREEMENT {
            bits |= UserFlags::ALL_PRIVS.bits();
        }
        if self.access.is_empty() {
            bits |= UserFlags::NO_PRIVS.bits();
        }
        if self.refuses_pm {
            bits |= UserFlags::REFUSES_PM.bits();
        }
        if self.refuses_chat {
            bits |= UserFlags::REFUSES_CHAT.bits();
        }
        bits
    }
}

/// `true` if any component of `dir` names an "Upload" or "Drop Box"
/// folder (case-insensitive), the one place an account without
/// `UPLOAD_ANYWHERE` may still upload to.
fn is_dropbox_path(dir: &HlPath) -> bool {
    dir.components.iter().any(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("upload") || c.contains("drop box")
    })
}

/// `true` if `access` lacks `VIEW_DROP_BOXES` and `dir`/`name`'s path
/// contains "drop box" (case-insensitive). `name` is `None` for a bare
/// directory listing.
fn dropbox_blocked(dir: &HlPath, name: Option<&str>, access: Access) -> bool {
    if access.contains(Access::VIEW_DROP_BOXES) {
        return false;
    }
    dir.components
        .iter()
        .map(String::as_str)
        .chain(name)
        .any(|c| c.to_ascii_lowercase().contains("drop box"))
}

/// `Err` if `dropbox_blocked` for `dir`/`name`, worded via
/// [`RequireFlags::require`] so it names `VIEW_DROP_BOXES` like any other
/// missing-privilege error.
fn dropbox_access(
    dir: &HlPath,
    name: Option<&str>,
    access: Access,
) -> Result<(), Cow<'static, str>> {
    if dropbox_blocked(dir, name, access) {
        access.require(Access::VIEW_DROP_BOXES)?;
    }
    Ok(())
}

/// A private chat room: its member user IDs, in join order, plus a
/// subject line.
#[derive(Debug, Clone, Default)]
pub struct ChatRoom {
    /// Currently joined members, in the order they joined.
    pub members: Vec<u16>,
    /// Room subject, settable by any member.
    pub subject: String,
    /// Users invited but not yet joined.
    pub invited: HashSet<u16>,
    /// Join password. Empty means the room is unprotected.
    pub password: String,
}

/// Which way a pending `HTXF` transfer moves file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Client downloading a file from the server (`FILE_GET`).
    Get,
    /// Client uploading a file to the server (`FILE_PUT`).
    Put,
}

/// A transfer awaiting its data connection: allocated when `FILE_GET`/
/// `FILE_PUT` replies with a reference number, consumed once the client
/// opens the second connection and sends that reference back. Identifies
/// the file by [`FileRequester`] login plus directory/name, not a raw
/// path, so completing the transfer still goes through the
/// [`FileProvider`] rather than assuming filesystem storage.
#[derive(Debug, Clone)]
pub struct PendingTransfer {
    /// Whether this is a download or upload.
    pub direction: TransferDirection,
    /// The requesting session's account login.
    pub login: String,
    /// Directory the file is in.
    pub dir: HlPath,
    /// File name.
    pub name: String,
    /// Download: the file's size in bytes. Upload: the size the client
    /// declared it will send.
    pub size: u64,
    /// Download only: bytes into the file to start sending from, when the
    /// client already has a leading portion from an earlier attempt.
    /// Always `0` for an upload or a non-resumed download.
    pub resume_offset: u64,
    /// The control connection's remote address; the data connection must
    /// arrive from the same IP or the transfer is refused.
    pub client_ip: IpAddr,
}

/// One threaded news article's full data, for a `HTLC_HDR_NEWS_GETTHREAD`
/// (400) reply.
#[derive(Debug, Clone)]
pub struct NewsArticleDetail {
    pub title: String,
    pub poster: String,
    pub date: HlDate,
    pub prev_id: u32,
    pub next_id: u32,
    pub parent_id: u32,
    pub first_child_id: u32,
    pub flavor: String,
    pub data: Vec<u8>,
}

/// A live privilege change for every session logged in under `login`.
#[derive(Debug, Clone)]
pub struct AccessUpdate {
    /// Login the modified account is stored under.
    pub login: String,
    /// The account's new privileges.
    pub access: Access,
    /// The account's new display name, if non-empty.
    pub name: Option<String>,
}

/// Everything server-initiated that can arrive on a recipient's channel.
#[derive(Debug, Clone)]
pub enum RecipientMessage {
    /// A wire transaction to forward as-is.
    Push(Transaction),
    /// A live privilege change for this session's login to apply.
    AccessUpdate(AccessUpdate),
    /// Close this connection with no wire-visible notice.
    Disconnect,
}

impl RecipientMessage {
    #[cfg(test)]
    pub(crate) fn into_push(self) -> Option<Transaction> {
        match self {
            Self::Push(transaction) => Some(transaction),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_access_update(self) -> Option<AccessUpdate> {
        match self {
            Self::AccessUpdate(access_update) => Some(access_update),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_disconnect(&self) -> bool {
        matches!(self, Self::Disconnect)
    }
}

/// State shared by every [`crate::Session`], behind cheap `Arc` clones.
#[derive(Clone)]
pub struct SharedState {
    users: Arc<RwLock<HashMap<u16, UserEntry>>>,
    next_user_id: Arc<AtomicU16>,
    /// One inbound channel per currently connected recipient.
    recipients: Arc<RwLock<HashMap<u16, mpsc::Sender<RecipientMessage>>>>,
    /// File storage backing the file listing and `FILE_GET`/`FILE_PUT`;
    /// see [`crate::FileProvider`].
    files: Arc<dyn FileProvider>,
    /// News storage, see [`crate::NewsProvider`]. No local cache: read and
    /// written directly on every call.
    news_provider: Arc<dyn NewsProvider>,
    /// Serializes a flat-news load-then-save round trip through `news_provider`.
    news_flat_lock: Arc<Mutex<()>>,
    /// Same purpose as `news_flat_lock`, for the threaded tree.
    news_tree_lock: Arc<Mutex<()>>,
    /// Chat ID `0` is always present: the main room every connected
    /// user is implicitly in. Other entries are private rooms.
    chat_rooms: Arc<RwLock<HashMap<u32, ChatRoom>>>,
    next_chat_id: Arc<AtomicU32>,
    /// Account storage; see [`crate::AccountProvider`].
    accounts: Arc<dyn AccountProvider>,
    /// Ban storage; see [`crate::BanProvider`].
    bans: Arc<dyn BanProvider>,
    /// Transfers awaiting their `HTXF` data connection, keyed by the
    /// reference number handed to the client.
    transfers: Arc<RwLock<HashMap<u32, PendingTransfer>>>,
    next_transfer_ref: Arc<AtomicU32>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new(
            Arc::new(InMemoryAccountProvider::default()),
            Arc::new(InMemoryFileProvider::default()),
            Arc::new(InMemoryNewsProvider::default()),
            Arc::new(InMemoryBanProvider::default()),
        )
    }
}

impl SharedState {
    /// Build fresh shared state backed by `accounts`, `files`, `news`, and `bans`.
    pub fn new(
        accounts: Arc<dyn AccountProvider>,
        files: Arc<dyn FileProvider>,
        news: Arc<dyn NewsProvider>,
        bans: Arc<dyn BanProvider>,
    ) -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            next_user_id: Arc::new(AtomicU16::new(1)),
            recipients: Arc::new(RwLock::new(HashMap::new())),
            files,
            news_provider: news,
            news_flat_lock: Arc::new(Mutex::new(())),
            news_tree_lock: Arc::new(Mutex::new(())),
            chat_rooms: Arc::new(RwLock::new(HashMap::from([(0, ChatRoom::default())]))),
            next_chat_id: Arc::new(AtomicU32::new(1)),
            accounts,
            bans,
            transfers: Arc::new(RwLock::new(HashMap::new())),
            next_transfer_ref: Arc::new(AtomicU32::new(1)),
        }
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
    }

    /// Snapshot of all currently connected users, in connection order
    /// (`user_id` is assigned monotonically, so sorting by it avoids the
    /// underlying map's arbitrary iteration order).
    pub async fn users(&self) -> Vec<UserEntry> {
        let mut users: Vec<UserEntry> = self.users.read().await.values().cloned().collect();
        users.sort_by_key(|u| u.user_id);
        users
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
            entry.last_active = Instant::now();
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

    /// Publish a transaction to every session (e.g. a chat message or a
    /// user list change notification). Sessions that are slow/gone just
    /// miss it; delivery is best-effort like the real protocol's chat.
    /// Not privilege-gated here - see [`crate::session::Session::may_receive`],
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

    /// A file's size in bytes, for starting a `FILE_GET` download. `Err`
    /// if it doesn't exist, is a folder, or is a drop box `access` can't view.
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

    /// Whether a file or folder already exists at this path.
    pub async fn file_exists(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> bool {
        self.files.exists(requester, dir, name).await
    }

    /// Whether `requester` may start a `FILE_PUT` upload of `name` under
    /// `dir`: the destination must be under an "Upload"/"Drop Box" folder
    /// unless `upload_anywhere`, and nothing may already exist there (no
    /// resume support yet).
    pub async fn authorize_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        access: Access,
    ) -> Result<(), Cow<'static, str>> {
        if !is_dropbox_path(dir) {
            access.require(Access::UPLOAD_ANYWHERE)?;
        }
        if self.files.exists(requester, dir, name).await {
            Err("file already exists.")?;
        }
        Ok(())
    }

    /// Open a file for reading, for a `FILE_GET` download.
    pub async fn open_file_for_read(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> std::io::Result<BoxedReader> {
        self.files.open_read(requester, dir, name).await
    }

    /// Create a file for writing, for a `FILE_PUT` upload.
    pub async fn create_file_for_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> std::io::Result<BoxedWriter> {
        self.files.create_write(requester, dir, name).await
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
    /// counter wraps into a still-pending ref.
    pub async fn register_transfer(&self, transfer: PendingTransfer) -> u32 {
        let mut transfers = self.transfers.write().await;
        loop {
            let reference = self.next_transfer_ref.fetch_add(1, Ordering::Relaxed);
            if reference != 0 && !transfers.contains_key(&reference) {
                transfers.insert(reference, transfer);
                return reference;
            }
        }
    }

    /// Remove and return a pending transfer by reference number, if one
    /// is still waiting. Consumes it: a reference is only good once.
    pub async fn take_transfer(&self, reference: u32) -> Option<PendingTransfer> {
        self.transfers.write().await.remove(&reference)
    }

    /// The full classic news text.
    pub async fn news(&self) -> String {
        self.news_provider.load_flat().await.to_string()
    }

    /// Prepend a new post to the classic news. Returns the formatted entry.
    pub async fn post_news(&self, poster: &str, text: &str) -> String {
        const DIVIDER: &str = "_________________________________________________________";
        let entry = format!("{DIVIDER}\rFrom {poster}\r\r{text}\r\r");
        let _guard = self.news_flat_lock.lock().await;
        let flat = self.news_provider.load_flat().await;
        let mut new_flat = String::with_capacity(entry.len() + flat.len());
        new_flat.push_str(&entry);
        new_flat.push_str(&flat);
        self.news_provider
            .save_flat(Arc::new(new_flat.into_boxed_str()))
            .await;
        entry
    }

    /// List a threaded news bundle's own contents at `path`. `Err` if it
    /// doesn't resolve to a bundle.
    pub async fn news_list_dir(
        &self,
        path: &HlPath,
    ) -> Result<Vec<NewsCategoryListEntry>, &'static str> {
        let tree = self.news_provider.load_tree().await;
        let entries = tree
            .list_dir(&path.components)
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
        let tree = self.news_provider.load_tree().await;
        let articles = tree
            .list_articles(&path.components)
            .ok_or("news category not found")?;
        Ok(NewsArticleListData {
            articles: articles
                .into_iter()
                .map(|(id, article)| NewsArticleSummary {
                    id,
                    date: article.date,
                    parent_id: article.parent_id,
                    flags: article.flags,
                    title: article.title.clone(),
                    poster: article.poster.clone(),
                    flavor: article.flavor.clone(),
                    flavor_size: article.data.len() as u16,
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
        let tree = self.news_provider.load_tree().await;
        let view = tree
            .article(&path.components, id)
            .ok_or("news article not found")?;
        Ok(NewsArticleDetail {
            title: view.article.title.clone(),
            poster: view.article.poster.clone(),
            date: view.article.date,
            prev_id: view.prev_id,
            next_id: view.next_id,
            parent_id: view.article.parent_id,
            first_child_id: view.article.children.first().copied().unwrap_or(0),
            flavor: view.article.flavor.clone(),
            data: view.article.data.clone(),
        })
    }

    /// Create a news bundle (folder) named `name` under `path`.
    pub async fn news_mkdir(&self, path: &HlPath, name: &str) -> Result<(), &'static str> {
        let _guard = self.news_tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.news_provider.load_tree().await);
        tree.mkdir(&path.components, name)?;
        self.news_provider.save_tree(Arc::new(tree)).await;
        Ok(())
    }

    /// Create a news category named `name` under `path`.
    pub async fn news_mkcategory(&self, path: &HlPath, name: &str) -> Result<(), &'static str> {
        let _guard = self.news_tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.news_provider.load_tree().await);
        tree.mkcategory(&path.components, name)?;
        self.news_provider.save_tree(Arc::new(tree)).await;
        Ok(())
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
        let _guard = self.news_tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.news_provider.load_tree().await);
        let kind = tree
            .item_kind(&path.components)
            .ok_or("news item not found")?;
        let required = match kind {
            NewsItemKind::Bundle => Access::NEWS_DELETE_FLDR,
            NewsItemKind::Category => Access::NEWS_DELETE_CAT,
        };
        access.require(required)?;
        tree.delete(&path.components).map_err(|e| e.to_string())?;
        self.news_provider.save_tree(Arc::new(tree)).await;
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
        flavor: String,
        data: Vec<u8>,
    ) -> Result<u32, &'static str> {
        let _guard = self.news_tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.news_provider.load_tree().await);
        let id = tree.post_article(
            &path.components,
            parent_id,
            title,
            poster,
            flags,
            flavor,
            data,
        )?;
        self.news_provider.save_tree(Arc::new(tree)).await;
        Ok(id)
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
        let _guard = self.news_tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.news_provider.load_tree().await);
        tree.delete_article(&path.components, id, recursive)?;
        self.news_provider.save_tree(Arc::new(tree)).await;
        Ok(())
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
    /// setup, not the `CHAT_JOIN` request path.
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
    pub async fn put_account(&self, login: String, account: Account) -> Result<(), String> {
        self.accounts.put(login, account).await
    }

    /// The active ban on `ip`, if any; see [`crate::BanProvider::is_banned`].
    pub async fn is_banned(&self, ip: std::net::IpAddr) -> Option<BanRecord> {
        self.bans.is_banned(ip).await
    }

    /// Ban `ip` until `until`, or permanently if `None`.
    pub async fn ban(
        &self,
        ip: std::net::IpAddr,
        until: Option<std::time::SystemTime>,
    ) -> Result<(), String> {
        self.bans.ban(ip, until).await
    }

    /// Delete an account. `Err("no such account")` if it didn't exist.
    pub async fn delete_account(&self, login: &str) -> Result<(), String> {
        match self.accounts.delete(login).await {
            Ok(true) => Ok(()),
            Ok(false) => Err("no such account".to_string()),
            Err(reason) => Err(reason),
        }
    }

    /// Every account, login and details together, sorted by login for a
    /// deterministic listing order (`HTLC_HDR_GET_USER_LIST`, 348).
    pub async fn list_accounts(&self) -> Vec<(String, Account)> {
        let mut accounts = self.accounts.list().await;
        accounts.sort_by(|(a, _), (b, _)| a.cmp(b));
        accounts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two `SharedState`s sharing a [`NewsProvider`] see each other's writes.
    #[tokio::test]
    async fn news_is_visible_across_shared_states_backed_by_the_same_provider() {
        let provider: Arc<dyn NewsProvider> = Arc::new(InMemoryNewsProvider::default());
        let state = SharedState::new(
            Arc::new(InMemoryAccountProvider::default()),
            Arc::new(InMemoryFileProvider::default()),
            provider.clone(),
            Arc::new(InMemoryBanProvider::default()),
        );

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

        let other = SharedState::new(
            Arc::new(InMemoryAccountProvider::default()),
            Arc::new(InMemoryFileProvider::default()),
            provider,
            Arc::new(InMemoryBanProvider::default()),
        );

        assert!(other.news().await.contains("hello"));
        let articles = other
            .news_list_articles(&HlPath::new(["General"]))
            .await
            .unwrap();
        assert_eq!(articles.articles.len(), 1);
        assert_eq!(articles.articles[0].title, "Hi");
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
        let state = SharedState::new(
            Arc::new(InMemoryAccountProvider::default()),
            files,
            Arc::new(crate::news::InMemoryNewsProvider::default()),
            Arc::new(InMemoryBanProvider::default()),
        );

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

    #[tokio::test]
    async fn register_transfer_allocates_distinct_refs_and_take_consumes_them() {
        let state = SharedState::default();
        let make = || PendingTransfer {
            direction: TransferDirection::Get,
            login: "alice".to_string(),
            dir: HlPath::default(),
            name: "x".to_string(),
            size: 0,
            resume_offset: 0,
            client_ip: "127.0.0.1".parse().unwrap(),
        };

        let ref_a = state.register_transfer(make()).await;
        let ref_b = state.register_transfer(make()).await;
        assert_ne!(ref_a, ref_b);

        assert!(state.take_transfer(ref_a).await.is_some());
        assert!(state.take_transfer(ref_a).await.is_none());
        assert!(state.take_transfer(ref_b).await.is_some());
    }
}
