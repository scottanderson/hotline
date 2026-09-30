/// One thing a session did that [`FloodPolicy`](crate::FloodPolicy) counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodEvent<'a> {
    /// Any transaction, before it is handled.
    Transaction,
    /// A private message, a broadcast, or a chat invite.
    Message,
    /// The text of a chat line, private message, broadcast, or news post.
    Text(&'a [u8]),
    /// A chat line, before its access check.
    Chat,
    /// Adds a relayed chat line of this many bytes. Never bans by itself.
    ChatBytes(usize),
    /// A name or icon change.
    NameChange,
}
