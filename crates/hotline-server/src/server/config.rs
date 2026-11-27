use std::num::NonZeroUsize;
use std::time::Duration;

use hotline_proto::text_encoding::{self, Encoding};

/// `max_downloads`'s default.
pub const DEFAULT_MAX_DOWNLOADS: usize = 100;

/// `max_downloads_per_client`'s default.
pub const DEFAULT_MAX_DOWNLOADS_PER_CLIENT: usize = 10;

/// `max_connections`'s default.
pub const DEFAULT_MAX_CONNECTIONS: usize = 500;

/// `max_connections_per_address`'s default.
pub const DEFAULT_MAX_CONNECTIONS_PER_ADDRESS: usize = 5;

/// `login_timeout`'s default.
pub const DEFAULT_LOGIN_TIMEOUT: Duration = Duration::from_secs(60);

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind, e.g. `"0.0.0.0:5500"`. The file transfer data
    /// connection listens one port above this.
    pub bind_addr: Box<str>,
    /// Agreement text shown to clients on login (empty = no agreement).
    pub agreement: Box<str>,
    /// Name reported in the [`myTran_Login`] (107) reply (protocol version >= 151).
    ///
    /// [`myTran_Login`]: hotline_proto::Opcode::Login
    pub server_name: Box<str>,
    /// Chat prefix for console commands; `None` relays all chat.
    pub console_prefix: Option<char>,
    /// Concurrent downloads before further ones queue. Uploads are not capped.
    pub max_downloads: usize,
    /// Concurrent downloads for one user before further ones queue.
    pub max_downloads_per_client: usize,
    /// Open connections before further ones are refused.
    pub max_connections: Option<NonZeroUsize>,
    /// Open connections from one address before further ones are refused.
    pub max_connections_per_address: Option<NonZeroUsize>,
    /// Time from accept to a completed login before the connection closes.
    pub login_timeout: Duration,
    /// Text encoding of every connection.
    pub encoding: &'static Encoding,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".into(),
            agreement: "Hotline".into(),
            server_name: "Hotline".into(),
            console_prefix: None,
            max_downloads: DEFAULT_MAX_DOWNLOADS,
            max_downloads_per_client: DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
            max_connections: NonZeroUsize::new(DEFAULT_MAX_CONNECTIONS),
            max_connections_per_address: NonZeroUsize::new(DEFAULT_MAX_CONNECTIONS_PER_ADDRESS),
            login_timeout: DEFAULT_LOGIN_TIMEOUT,
            encoding: text_encoding::DEFAULT,
        }
    }
}
