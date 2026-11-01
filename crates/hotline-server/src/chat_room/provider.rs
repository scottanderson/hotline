use async_trait::async_trait;

use crate::chat_room::ChatRoom;

/// The storage of the chat rooms, keyed by chat ID.
///
/// Chat ID `0` is the main chat room. A new provider holds it with no
/// members. Each change runs with no other change to its room between
/// its read and its write.
#[async_trait]
pub trait ChatRoomProvider: Send + Sync {
    /// Store `room` under a new chat ID other than `0`, and return the ID.
    async fn create(&self, room: ChatRoom) -> u32;
    /// A copy of the room `chat_id`.
    async fn room(&self, chat_id: u32) -> Option<ChatRoom>;
    /// Run `change` on the room `chat_id`. The room is deleted when
    /// `change` returns `false`. Returns `false` if the room does not
    /// exist.
    async fn update(
        &self,
        chat_id: u32,
        change: &mut (dyn for<'r> FnMut(&'r mut ChatRoom) -> bool + Send),
    ) -> bool;
    /// Run `change` on each room with its chat ID. The room is deleted
    /// when `change` returns `false`.
    async fn update_each(
        &self,
        change: &mut (dyn for<'r> FnMut(u32, &'r mut ChatRoom) -> bool + Send),
    );
}
