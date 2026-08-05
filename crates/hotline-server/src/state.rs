//! Shared, cross-session server state: the connected-user table, chat
//! rooms, classic news, and the one per-recipient channel sessions use to
//! reach each other.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, RwLock};

use hotline_proto::consts::AccessPrivileges;
use hotline_proto::opcode::{Opcode, ServerOpcode};
use hotline_proto::types::{FileNameWithInfo, UserFlags};
use hotline_proto::Transaction;

use crate::accounts::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};

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
}

/// A live privilege change, see [`SharedState::push_access_update`].
#[derive(Debug, Clone)]
pub struct AccessUpdate {
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
    /// Root-folder file listing. In-memory placeholder for a real
    /// filesystem-backed implementation.
    files: Arc<RwLock<Vec<FileNameWithInfo>>>,
    /// Classic (pre-threaded) news: one flat, newest-first text blob.
    news: Arc<RwLock<String>>,
    /// Chat ID `0` is always present: the main room every connected
    /// user is implicitly in. Other entries are private rooms.
    chat_rooms: Arc<RwLock<HashMap<u32, ChatRoom>>>,
    next_chat_id: Arc<AtomicU32>,
    /// Account storage; see [`crate::AccountProvider`].
    accounts: Arc<dyn AccountProvider>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self::new(Arc::new(InMemoryAccountProvider::default()))
    }
}

impl SharedState {
    /// Build fresh shared state backed by `accounts`.
    pub fn new(accounts: Arc<dyn AccountProvider>) -> Self {
        let files = vec![
            FileNameWithInfo {
                file_type: *b"fldr",
                creator: *b"\0\0\0\0",
                size: 0,
                name: "Uploads".to_string(),
            },
            FileNameWithInfo {
                file_type: *b"TEXT",
                creator: *b"ttxt",
                size: 0,
                name: "readme.txt".to_string(),
            },
        ];
        Self {
            users: Arc::new(RwLock::new(HashMap::new())),
            next_user_id: Arc::new(AtomicU16::new(1)),
            recipients: Arc::new(RwLock::new(HashMap::new())),
            files: Arc::new(RwLock::new(files)),
            news: Arc::new(RwLock::new(String::new())),
            chat_rooms: Arc::new(RwLock::new(HashMap::from([(0, ChatRoom::default())]))),
            next_chat_id: Arc::new(AtomicU32::new(1)),
            accounts,
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
        self.join_chat_room(0, user_id).await;
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

    /// Whether `user_id` may receive `opcode`, based on its connected
    /// user entry's privileges. Pushes not listed here are unfiltered.
    async fn recipient_may_receive(&self, user_id: u16, opcode: Opcode) -> bool {
        let (access, access_extra) = match self.user(user_id).await {
            Some(entry) => (entry.access, entry.access_extra),
            None => (AccessPrivileges::empty(), AccessExtra::empty()),
        };
        match opcode {
            Opcode::Push(ServerOpcode::UserChange) | Opcode::Push(ServerOpcode::UserPart) => {
                access_extra.contains(AccessExtra::USER_GETLIST)
            }
            Opcode::Push(ServerOpcode::Chat) => access.contains(AccessPrivileges::READ_CHAT),
            Opcode::Push(ServerOpcode::NewsfilePost) => {
                access.contains(AccessPrivileges::READ_NEWS)
            }
            _ => true,
        }
    }

    /// Publish a transaction to every privileged session. Sessions that
    /// are slow/gone, or not privileged to see it, just miss it.
    pub async fn publish(&self, tx: Transaction) {
        let opcode = tx.opcode();
        for (&user_id, sender) in self.recipients.read().await.iter() {
            if self.recipient_may_receive(user_id, opcode).await {
                let _ = sender.try_send(RecipientMessage::Push(tx.clone()));
            }
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
        let opcode = tx.opcode();
        for (&user_id, sender) in self.recipients.read().await.iter() {
            if user_id != exclude_user_id && self.recipient_may_receive(user_id, opcode).await {
                let _ = sender.try_send(RecipientMessage::Push(tx.clone()));
            }
        }
    }

    /// Push a live privilege change to every recipient logged in under `login`.
    pub async fn push_access_update(
        &self,
        login: &str,
        access: AccessPrivileges,
        name: Option<String>,
    ) {
        let update = AccessUpdate { access, name };
        for (&user_id, sender) in self.recipients.read().await.iter() {
            if self.user(user_id).await.is_some_and(|u| u.login == login) {
                let _ = sender.try_send(RecipientMessage::AccessUpdate(update.clone()));
            }
        }
    }

    /// Snapshot of the (currently root-only) file listing. `path` is
    /// accepted for API shape but ignored until real subfolder support
    /// lands.
    pub async fn root_files(&self) -> Vec<FileNameWithInfo> {
        self.files.read().await.clone()
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

    /// Add `user_id` to a chat room, at the end of the join order, and
    /// clear any pending invite for them. `false` if the room doesn't
    /// exist.
    pub async fn join_chat_room(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .map(|room| {
                if !room.members.contains(&user_id) {
                    room.members.push(user_id);
                }
                room.invited.remove(&user_id);
            })
            .is_some()
    }

    /// Mark `user_id` as invited to a chat room. `false` if the room
    /// doesn't exist.
    pub async fn invited(&self, chat_id: u32, user_id: u16) -> bool {
        self.chat_rooms
            .write()
            .await
            .get_mut(&chat_id)
            .map(|room| {
                room.invited.insert(user_id);
            })
            .is_some()
    }

    /// Clear a pending invite without joining, e.g. on decline.
    pub async fn invite_declined(&self, chat_id: u32, user_id: u16) {
        if let Some(room) = self.chat_rooms.write().await.get_mut(&chat_id) {
            room.invited.remove(&user_id);
        }
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

    /// Look up an account by login.
    pub async fn account(&self, login: &str) -> Option<Account> {
        self.accounts.get(login).await
    }

    /// Create or overwrite an account.
    pub async fn put_account(&self, login: String, account: Account) -> Result<(), String> {
        self.accounts.put(login, account).await
    }

    /// Delete an account. `false` if it didn't exist.
    pub async fn delete_account(&self, login: &str) -> Result<bool, String> {
        self.accounts.delete(login).await
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
        state.join_chat_room(shared, 1).await;

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
}
