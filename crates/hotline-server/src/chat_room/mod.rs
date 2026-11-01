//! The private chat rooms, and the main chat room.
//!
//! [`ChatRoomProvider`] is the storage interface, and
//! [`InMemoryChatRoomProvider`] is the default. The embedding application
//! can supply its own to [`crate::Server::new`] in [`crate::Providers`].

mod memory;
mod provider;
mod room;

pub use memory::InMemoryChatRoomProvider;
pub use provider::ChatRoomProvider;
pub use room::ChatRoom;
