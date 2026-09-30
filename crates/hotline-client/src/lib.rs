//! Async Hotline client session.
//!
//! `HotlineClient` owns the TCP connection, performs the `TRTP` handshake,
//! and runs a background read loop that demultiplexes incoming
//! transactions: replies are routed back to the matching `request()` call
//! by transaction ID, and server-initiated transactions (chat, user
//! join/leave, news posts, ...) are delivered through the
//! `mpsc::UnboundedReceiver<Transaction>` [`HotlineClient::connect`] returns
//! alongside the client.

mod client;
mod client_info_text;
mod error;
mod login_result;

pub use client::HotlineClient;
pub use client_info_text::ClientInfoText;
pub use error::{ClientError, Result};
pub use login_result::LoginResult;
