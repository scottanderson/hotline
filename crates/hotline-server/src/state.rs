//! Shared, cross-session server state: the connected-user table, chat
//! rooms, classic news, and the two delivery channels sessions use to
//! reach each other (broadcast to everyone, targeted to one user).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU16, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, RwLock};

use hotline_proto::consts::AccessPrivileges;
use hotline_proto::types::{FileNameWithInfo, UserFlags};
use hotline_proto::Transaction;

/// How long without any request before a user is considered idle.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// One connected user, as tracked by the server.
#[derive(Debug, Clone)]
pub struct UserEntry {
    /// Socket/user ID assigned at connect time.
    pub user_id: u16,
    /// Display name (post `SET_CLIENT_USER_INFO`).
    pub name: String,
    /// Icon ID.
    pub icon: u16,
    /// Privileges granted at login. Also kept in
    /// [`crate::session::Session`], which is what handlers check against.
    pub access: AccessPrivileges,
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

/// A transaction addressed to exactly one connected user (private
/// message, chat-room traffic, a kick notice) rather than everyone -
/// see [`SharedState::send_to`].
#[derive(Debug, Clone)]
pub struct Targeted {
    /// Recipient's user ID. Every session subscribes to the same
    /// channel and forwards only what's addressed to itself.
    pub target_user_id: u16,
    /// The transaction to forward.
    pub tx: Transaction,
}

/// A transaction broadcast to everyone *except* one user, currently only
/// the login join notification. Contrast [`Targeted`] (exactly one user)
/// and [`SharedState::publish`] (everyone, including the sender).
#[derive(Debug, Clone)]
pub struct Announcement {
    /// User ID to skip when fanning this out.
    pub exclude_user_id: u16,
    /// The transaction to forward to everyone else.
    pub tx: Transaction,
}

/// State shared by every [`crate::Session`], behind cheap `Arc` clones.
#[derive(Clone)]
pub struct SharedState {
    users: Arc<RwLock<HashMap<u16, UserEntry>>>,
    next_user_id: Arc<AtomicU16>,
    /// Server-initiated transactions meant for everyone (public chat,
    /// user list changes) get published here; each session subscribes
    /// and forwards them to its own client.
    broadcast: broadcast::Sender<Transaction>,
    /// Transactions meant for exactly one user (private messages, chat
    /// rooms, kicks). See [`Targeted`].
    targeted: broadcast::Sender<Targeted>,
    /// Transactions meant for everyone except one user. See
    /// [`Announcement`].
    announcements: broadcast::Sender<Announcement>,
    /// Root-folder file listing. In-memory placeholder for a real
    /// filesystem-backed implementation.
    files: Arc<RwLock<Vec<FileNameWithInfo>>>,
    /// Classic (pre-threaded) news: one flat, newest-first text blob.
    news: Arc<RwLock<String>>,
    /// Chat ID `0` is always present: the main room every connected
    /// user is implicitly in. Other entries are private rooms.
    chat_rooms: Arc<RwLock<HashMap<u32, ChatRoom>>>,
    next_chat_id: Arc<AtomicU32>,
}

impl Default for SharedState {
    fn default() -> Self {
        let (broadcast, _rx) = broadcast::channel(256);
        let (targeted, _rx) = broadcast::channel(256);
        let (announcements, _rx) = broadcast::channel(256);
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
            broadcast,
            targeted,
            announcements,
            files: Arc::new(RwLock::new(files)),
            news: Arc::new(RwLock::new(String::new())),
            chat_rooms: Arc::new(RwLock::new(HashMap::from([(0, ChatRoom::default())]))),
            next_chat_id: Arc::new(AtomicU32::new(1)),
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

    /// Publish a transaction to every session (e.g. a chat message or a
    /// user list change notification). Sessions that are slow/gone just
    /// miss it; delivery is best-effort like the real protocol's chat.
    pub fn publish(&self, tx: Transaction) {
        let _ = self.broadcast.send(tx);
    }

    /// Subscribe to the broadcast (everyone) stream. Called once per
    /// session.
    pub fn subscribe(&self) -> broadcast::Receiver<Transaction> {
        self.broadcast.subscribe()
    }

    /// Send a transaction to exactly one connected user (private
    /// message, chat-room traffic, a kick notice). A no-op if that user
    /// isn't connected, same best-effort delivery as [`Self::publish`].
    pub fn send_to(&self, target_user_id: u16, tx: Transaction) {
        let _ = self.targeted.send(Targeted { target_user_id, tx });
    }

    /// Subscribe to the targeted-delivery stream. Called once per
    /// session; each session filters for its own `user_id`.
    pub fn subscribe_targeted(&self) -> broadcast::Receiver<Targeted> {
        self.targeted.subscribe()
    }

    /// Broadcast a transaction to every session except `exclude_user_id`
    /// (the login join notification, see [`Announcement`]).
    pub fn announce(&self, exclude_user_id: u16, tx: Transaction) {
        let _ = self.announcements.send(Announcement {
            exclude_user_id,
            tx,
        });
    }

    /// Subscribe to the announcement stream. Called once per session;
    /// each session skips announcements that exclude its own `user_id`.
    pub fn subscribe_announcements(&self) -> broadcast::Receiver<Announcement> {
        self.announcements.subscribe()
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
                    name: format!("user{id}"),
                    icon: 0,
                    access: AccessPrivileges::empty(),
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
                    name: format!("user{id}"),
                    icon: 0,
                    access: AccessPrivileges::empty(),
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
