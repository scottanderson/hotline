//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and shared state (connected users);
//! each accepted connection becomes a [`Session`] that performs the
//! `TRTP` handshake and then loops on incoming transactions, dispatching
//! by transaction type to the handlers registered on [`Server`].

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
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, FileProvider,
    FileRequester, FolderItem, InMemoryFileProvider, PARTIAL_CREATOR, PARTIAL_SUFFIX, PARTIAL_TYPE,
};
pub use flood::{FloodDecision, FloodEvent, FloodPolicy, InMemoryFloodPolicy, NoFloodPolicy};
pub use news::{
    FlatNewsProvider, InMemoryNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle,
    NewsItemKind, NewsListKind, ThreadedNewsProvider,
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
