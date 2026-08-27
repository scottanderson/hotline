//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and shared state (connected users);
//! each accepted connection becomes a [`Session`] that performs the
//! `TRTP` handshake and then loops on incoming transactions, dispatching
//! by transaction type to the handlers registered on [`Server`].

mod account;
mod ban;
mod banner;
mod bitflags_serde;
mod console;
mod error;
mod file;
mod flood;
mod news;
mod server;
mod session;
mod state;

pub use account::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
pub use ban::{BanProvider, BanRecord, InMemoryBanProvider};
pub use banner::{BannerKind, BannerProvider, NoBanner, UrlBannerProvider};
pub use console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply, DefaultChatCommands};
pub use error::{Result, ServerError};
pub use file::{
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, FileProvider,
    FileRequester, InMemoryFileProvider, PARTIAL_CREATOR, PARTIAL_SUFFIX, PARTIAL_TYPE,
};
pub use flood::{FloodConfig, FloodDecision, FloodPolicy, InMemoryFloodPolicy, NoFloodPolicy};
pub use news::{InMemoryNewsProvider, NewsArticleDetail, NewsBundle, NewsProvider};
pub use server::{Server, ServerConfig};
pub use session::Session;
pub use state::{PendingTransfer, SharedState, TransferDirection, TransferKind, UserEntry};
