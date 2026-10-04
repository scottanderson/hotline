//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and the [`SharedState`] of all
//! connections. Each accepted connection becomes a [`Session`]. The
//! session does the `TRTP` handshake and then handles each transaction
//! by its opcode. [`Providers`] holds the storage for accounts, files,
//! news, and bans.
//!
//! ```no_run
//! use hotline_server::{Providers, Server, ServerConfig};
//!
//! # async fn run() -> hotline_server::Result<()> {
//! let config = ServerConfig {
//!     bind_addr: "0.0.0.0:5500".into(),
//!     ..ServerConfig::default()
//! };
//! Server::new(config, Providers::default()).run().await
//! # }
//! ```

mod account;
mod activity;
mod ban;
mod banner;
mod bitflags_serde;
mod console;
mod error;
mod file;
mod flood;
mod news;
mod providers;
mod server;
mod session;
mod state;

pub use account::{AccessExtra, Account, AccountProvider, InMemoryAccountProvider};
pub use activity::{AccountAction, Activity, ActivityEntry, ActivityLog, NoActivityLog};
pub use ban::{BanProvider, BanRecord, InMemoryBanProvider};
pub use banner::{BannerKind, BannerProvider, NoBanner, UrlBannerProvider};
pub use console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply, DefaultChatCommands};
pub use error::{Result, ServerError};
pub use file::{
    partial_upload_name, type_and_creator_for_name, validate_file_name, BoxedReader, BoxedWriter,
    FileProvider, FileRequester, FolderItem, InMemoryFileProvider, PARTIAL_CREATOR, PARTIAL_SUFFIX,
    PARTIAL_TYPE,
};
pub use flood::{FloodDecision, FloodEvent, FloodPolicy, InMemoryFloodPolicy, NoFloodPolicy};
pub use news::{
    FlatNewsProvider, InMemoryNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle,
    NewsCategory, NewsItemKind, NewsListKind, ThreadedNewsProvider, HLNZ_MAGIC,
};
pub use providers::Providers;
pub use server::{
    Server, ServerConfig, DEFAULT_LOGIN_TIMEOUT, DEFAULT_MAX_CONNECTIONS,
    DEFAULT_MAX_CONNECTIONS_PER_ADDRESS, DEFAULT_MAX_DOWNLOADS, DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
};
pub use session::Session;
pub use state::{
    FileTransfer, FolderTransfer, PendingTransfer, SharedState, TransferDirection, TransferKind,
    TransferProgress, TransferReport, TransferSection, UserEntry,
};
