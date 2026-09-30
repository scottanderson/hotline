//! Flood/spam limits, checked per transaction, per kind of message, and
//! per new connection.
//!
//! [`FloodPolicy`] is the decision interface; `hotline-server` only defines
//! it and provides [`NoFloodPolicy`] and [`InMemoryFloodPolicy`] as
//! defaults. The embedding application supplies the real policy to
//! [`crate::Server::new`], the same way it supplies a [`crate::BanProvider`].

mod decision;
mod event;
mod memory;
mod none;
mod policy;

pub use decision::FloodDecision;
pub use event::FloodEvent;
pub use memory::InMemoryFloodPolicy;
pub use none::NoFloodPolicy;
pub use policy::FloodPolicy;

/// Most of one chat line that counts, openline's relay cap.
pub(crate) const MAX_CHAT_LINE: usize = 8192;
