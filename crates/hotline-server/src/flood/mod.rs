//! Flood/spam rate limiting, checked once per inbound client transaction.
//!
//! [`FloodPolicy`] is the decision interface; `hotline-server` only defines
//! it and provides [`NoFloodPolicy`] and [`InMemoryFloodPolicy`] as
//! defaults. The embedding application supplies the real policy to
//! [`crate::Server::new`], the same way it supplies a [`crate::BanProvider`].

mod config;
mod decision;
mod memory;
mod none;
mod policy;
mod state;

pub use config::FloodConfig;
pub use decision::FloodDecision;
pub use memory::InMemoryFloodPolicy;
pub use none::NoFloodPolicy;
pub use policy::FloodPolicy;
