//! The users that are online.
//!
//! [`UserListProvider`] is the storage interface, and
//! [`InMemoryUserListProvider`] is the default. The embedding application
//! can supply its own to [`crate::Server::new`] in [`crate::Providers`].

mod memory;
mod provider;

pub use memory::InMemoryUserListProvider;
pub use provider::UserListProvider;
