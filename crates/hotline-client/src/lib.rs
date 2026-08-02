//! Async Hotline client session.
//!
//! `HotlineClient` owns the TCP connection, performs the `TRTP` handshake,
//! and runs a background read loop that demultiplexes incoming
//! transactions: replies are routed back to the matching `request()` call
//! by transaction ID, and server-initiated transactions (chat, user
//! join/leave, news posts, ...) are delivered through [`HotlineClient::events`].

mod client;
mod error;

pub use client::{HotlineClient, LoginResult};
pub use error::{ClientError, Result};
