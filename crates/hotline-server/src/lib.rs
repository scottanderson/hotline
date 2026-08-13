//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and shared state (connected users);
//! each accepted connection becomes a [`Session`] that performs the
//! `TRTP` handshake and then loops on incoming transactions, dispatching
//! by transaction type to the handlers registered on [`Server`].

mod accounts;
mod bitflags_serde;
mod error;
mod files;
mod news;
mod server;
mod session;
mod state;

pub use accounts::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
pub use error::{Result, ServerError};
pub use files::{BoxedReader, BoxedWriter, FileProvider, FileRequester, InMemoryFileProvider};
pub use server::{Server, ServerConfig};
pub use session::Session;
pub use state::{NewsArticleDetail, PendingTransfer, SharedState, TransferDirection, UserEntry};
