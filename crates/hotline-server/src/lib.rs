//! Async Hotline server framework.
//!
//! [`Server`] owns a `TcpListener` and the [`SharedState`] of all
//! connections. Each accepted connection becomes a [`Session`]. The
//! session does the `TRTP` handshake and then handles each transaction
//! by its opcode. [`Providers`] holds the storage for accounts, files,
//! news, bans, the online users, and the chat rooms.
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

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod account;
mod activity;
mod ban;
mod banner;
mod bitflags_serde;
mod chat_room;
mod console;
mod error;
mod file;
mod flood;
mod news;
mod policy;
mod providers;
mod random;
mod server;
mod session;
mod state;
mod sync;
mod time;
mod user_list;

pub use account::{
    AccessExtra, Account, AccountProvider, AccountStores, InMemoryAccountProvider, NoAccountStores,
};
pub use activity::{AccountAction, Activity, ActivityEntry, ActivityLog, NoActivityLog};
pub use ban::{BanProvider, BanRecord, InMemoryBanProvider};
pub use banner::{BannerKind, BannerProvider, NoBanner, UrlBannerProvider};
pub use chat_room::{ChatRoom, ChatRoomProvider, InMemoryChatRoomProvider};
pub use console::{ChatCommandActor, ChatCommandDispatcher, ConsoleReply, DefaultChatCommands};
pub use error::{Result, ServerError};
pub use file::{
    BoxedReader, BoxedWriter, FileProvider, FileRequester, FinderInfo, FolderItem,
    InMemoryFileProvider, ItemTimes, PARTIAL_CREATOR, PARTIAL_SUFFIX, PARTIAL_TYPE,
    partial_upload_name, type_and_creator_for_name, validate_file_name,
};
pub use flood::{FloodDecision, FloodEvent, FloodPolicy, InMemoryFloodPolicy, NoFloodPolicy};
pub use news::{
    FlatNewsProvider, HLNZ_MAGIC, InMemoryNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle,
    NewsCategory, NewsItemKind, NewsListKind, PostError, ThreadedNewsProvider,
};
pub use policy::{ChatRooms, OpenlinePolicy, ServerPolicy, Silent, TakenLogin, TimeZone};
pub use providers::Providers;
pub use server::{
    DEFAULT_LOGIN_TIMEOUT, DEFAULT_MAX_CONNECTIONS, DEFAULT_MAX_CONNECTIONS_PER_ADDRESS,
    DEFAULT_MAX_DOWNLOADS, DEFAULT_MAX_DOWNLOADS_PER_CLIENT, Server, ServerConfig,
};
pub use session::Session;
pub use state::{
    FileTransfer, FolderTransfer, PendingTransfer, SharedState, TransferDirection, TransferKind,
    TransferProgress, TransferReport, TransferSection, UserEntry,
};
pub use user_list::{InMemoryUserListProvider, UserListProvider};

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
