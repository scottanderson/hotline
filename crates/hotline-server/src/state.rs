//! Shared, cross-session server state: the connected-user table, chat
//! rooms, classic news, and the one per-recipient channel sessions use to
//! reach each other.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, RwLock};

use hotline_proto::consts::AccessPrivileges;
use hotline_proto::types::{FileNameWithInfo, HlPath, UserFlags};
use hotline_proto::Transaction;

use crate::accounts::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
use crate::files::{BoxedReader, BoxedWriter, FileProvider, FileRequester, InMemoryFileProvider};

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
    /// Remote socket address this connection was accepted from.
    pub remote_addr: SocketAddr,
    /// When this connection was accepted.
    pub connected_at: Instant,
    /// Privileges granted at login. Also kept in
    /// [`crate::session::Session`], which is what handlers check against.
    pub access: AccessPrivileges,
    /// Server-side-only account settings granted at login, also kept in
    /// [`crate::session::Session`].
    pub access_extra: AccessExtra,
    /// Last time this user sent any request. Compared against
    /// [`IDLE_THRESHOLD`] by [`UserEntry::is_idle`].
    pub last_active: Instant,
}

impl UserEntry {
    /// `true` if this user hasn't sent a request in over
    /// [`IDLE_THRESHOLD`].
    pub fn is_idle(&self) -> bool {
        self.last_active.elapsed() > IDLE_THRESHOLD
    }

    /// Current `UserFlags` bits: admin (from `DISCONNECT_USERS`) plus idle.
    pub fn flags(&self) -> u16 {
        let mut bits = if self.access.contains(AccessPrivileges::DISCONNECT_USERS) {
            UserFlags::ADMIN.bits()
        } else {
            0
        };
        if self.is_idle() {
            bits |= UserFlags::AWAY.bits();
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
    /// The control connection's remote address; the data connection must
    /// arrive from the same IP or the transfer is refused.
    pub client_ip: IpAddr,
}

/// A live privilege change for every session logged in under `login`.
#[derive(Debug, Clone)]
pub struct AccessUpdate {
    /// Login the modified account is stored under.
    pub login: String,
    /// The account's new privileges.
    pub access: AccessPrivileges,
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
    /// Classic (pre-threaded) news: one flat, newest-first text blob.
    news: Arc<RwLock<String>>,
    /// Chat ID `0` is always present: the main room every connected
    /// user is implicitly in. Other entries are private rooms.
    chat_rooms: Arc<RwLock<HashMap<u32, ChatRoom>>>,
    next_chat_id: Arc<AtomicU32>,
    /// Account storage; see [`crate::AccountProvider`].
    accounts: Arc<dyn AccountProvider>,
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
        )
    }
}

impl SharedState {
    /// Build fresh shared state backed by `accounts` and `files`.
    pub fn new(accounts: Arc<dyn AccountProvider>, files: Arc<dyn FileProvider>) -> Self {
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            next_user_id: Arc::new(AtomicU16::new(1)),
            recipients: Arc::new(RwLock::new(HashMap::new())),
            files,
            news: Arc::new(RwLock::new(String::new())),
            chat_rooms: Arc::new(RwLock::new(HashMap::from([(0, ChatRoom::default())]))),
            next_chat_id: Arc::new(AtomicU32::new(1)),
            accounts,
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
            .is_some_and(|u| u.access.contains(AccessPrivileges::CANT_BE_DISCONNECTED))
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

    /// Like [`Self::publish`], except `exclude_user_id` never receives it.
    pub async fn announce(&self, exclude_user_id: u16, tx: Transaction) {
        for (&user_id, sender) in self.recipients.read().await.iter() {
            if user_id != exclude_user_id {
                let _ = sender.try_send(RecipientMessage::Push(tx.clone()));
            }
        }
    }

    /// Push a live privilege change for every session logged in under `login`.
    pub async fn push_access_update(
        &self,
        login: String,
        access: AccessPrivileges,
        name: Option<String>,
    ) {
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
    /// root folder. `None` if it doesn't exist (or isn't visible to
    /// `requester`); see [`crate::FileProvider::list`].
    pub async fn list_files(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        self.files.list(requester, dir).await
    }

    /// A file's size in bytes, or `None` if it doesn't exist, is a
    /// folder, or isn't visible to `requester`.
    pub async fn file_size(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<u64> {
        self.files.size(requester, dir, name).await
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
        upload_anywhere: bool,
    ) -> Result<(), &'static str> {
        if !upload_anywhere && !is_dropbox_path(dir) {
            return Err("You are not allowed to do that.");
        }
        if self.files.exists(requester, dir, name).await {
            return Err("file already exists.");
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
        self.news.read().await.clone()
    }

    /// Prepend a new post to the classic news. Returns the formatted entry.
    pub async fn post_news(&self, poster: &str, text: &str) -> String {
        const DIVIDER: &str = "_________________________________________________________";
        let entry = format!("{DIVIDER}\rFrom {poster}\r\r{text}\r\r");
        self.news.write().await.insert_str(0, &entry);
        entry
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
    /// global room (`chat_id == 0`) needs `has_set_subject`; any other
    /// room just needs `user_id` to already be a member.
    pub async fn authorize_chat_subject_change(
        &self,
        chat_id: u32,
        user_id: u16,
        is_global: bool,
        has_set_subject: bool,
    ) -> Result<(), &'static str> {
        if is_global {
            return has_set_subject
                .then_some(())
                .ok_or("You are not allowed to do that.");
        }
        match self.chat_room_members(chat_id).await {
            Some(members) if members.contains(&user_id) => Ok(()),
            Some(_) => Err("you are not in that chat room"),
            None => Err("no such chat room"),
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

    /// Delete an account. `Err("no such account")` if it didn't exist.
    pub async fn delete_account(&self, login: &str) -> Result<(), String> {
        match self.accounts.delete(login).await {
            Ok(true) => Ok(()),
            Ok(false) => Err("no such account".to_string()),
            Err(reason) => Err(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
                    access: AccessPrivileges::empty(),
                    access_extra: AccessExtra::default(),
                    remote_addr: "127.0.0.1:0".parse().unwrap(),
                    connected_at: Instant::now(),
                    last_active: Instant::now(),
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
                    access: AccessPrivileges::empty(),
                    access_extra: AccessExtra::default(),
                    remote_addr: "127.0.0.1:0".parse().unwrap(),
                    connected_at: Instant::now(),
                    last_active: Instant::now(),
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
        let state = SharedState::new(Arc::new(InMemoryAccountProvider::default()), files);

        let requester = FileRequester { login: "alice" };
        let mut entries = state
            .list_files(requester, &HlPath::default())
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
