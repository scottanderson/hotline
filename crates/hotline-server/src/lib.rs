//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and shared state (connected users);
//! each accepted connection becomes a [`Session`] that performs the
//! `TRTP` handshake and then loops on incoming transactions, dispatching
//! by transaction type to the handlers registered on [`Server`].

mod accounts;
mod banner;
mod bans;
mod bitflags_serde;
mod error;
mod files;
mod flood;
mod news;
mod server;
mod session;
mod state;

pub use accounts::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
pub use banner::{BannerKind, BannerProvider, NoBanner, UrlBannerProvider};
pub use bans::{BanProvider, BanRecord, InMemoryBanProvider};
pub use error::{Result, ServerError};
pub use files::{
    type_and_creator_for_name, BoxedReader, BoxedWriter, FileProvider, FileRequester,
    InMemoryFileProvider,
};
pub use flood::{FloodConfig, FloodDecision, FloodPolicy, InMemoryFloodPolicy, NoFloodPolicy};
pub use news::{InMemoryNewsProvider, NewsBundle, NewsProvider};
pub use server::{Server, ServerConfig};
pub use session::Session;
pub use state::{
    NewsArticleDetail, PendingTransfer, SharedState, TransferDirection, TransferKind, UserEntry,
};
