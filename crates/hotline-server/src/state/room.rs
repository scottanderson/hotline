use std::collections::HashSet;

/// A private chat room: its member user IDs, in join order, plus a
/// subject line.
#[derive(Debug, Clone, Default)]
pub struct ChatRoom {
    /// Currently joined members, in the order they joined.
    pub members: Vec<u16>,
    /// Room subject, settable by any member.
    pub subject: Box<str>,
    /// Users invited but not yet joined.
    pub invited: HashSet<u16>,
    /// Join password. Empty means the room is unprotected.
    pub password: Box<str>,
}
