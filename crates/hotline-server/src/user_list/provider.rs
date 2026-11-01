use async_trait::async_trait;

use crate::state::UserEntry;

/// The storage of the users that are online, keyed by user ID.
///
/// A [`UserEntry`] holds [`Instant`] times, so a provider keeps the
/// entries in the server's process.
///
/// [`Instant`]: std::time::Instant
#[async_trait]
pub trait UserListProvider: Send + Sync {
    /// Store `entry`, which replaces the entry with its user ID.
    async fn insert(&self, entry: UserEntry);
    /// Remove the user `user_id`.
    async fn remove(&self, user_id: u16);
    /// The user `user_id`.
    async fn user(&self, user_id: u16) -> Option<UserEntry>;
    /// Every user, in user ID order.
    async fn users(&self) -> Vec<UserEntry>;
    /// Record that the user `user_id` sent a request now.
    async fn touch(&self, user_id: u16);
}
