/// `max_downloads`'s default, openline's `mOptions.nMaxDownloads`.
pub const DEFAULT_MAX_DOWNLOADS: usize = 100;

/// `max_downloads_per_client`'s default, openline's `mOptions.nMaxDownloadsClient`.
pub const DEFAULT_MAX_DOWNLOADS_PER_CLIENT: usize = 10;

/// `max_connections`'s default.
pub const DEFAULT_MAX_CONNECTIONS: usize = 500;

/// `max_connections_per_address`'s default, openline's
/// `mOptions.nMaxConnectionsIP`.
pub const DEFAULT_MAX_CONNECTIONS_PER_ADDRESS: usize = 5;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind, e.g. `"0.0.0.0:5500"`. The file transfer data
    /// connection listens one port above this.
    pub bind_addr: String,
    /// Agreement text shown to clients on login (empty = no agreement).
    pub agreement: String,
    /// Name reported in the [`myTran_Login`] (107) reply (protocol version >= 151).
    ///
    /// [`myTran_Login`]: hotline_proto::Opcode::Login
    pub server_name: String,
    /// Chat prefix for console commands; `None` relays all chat.
    pub console_prefix: Option<char>,
    /// Concurrent downloads before further ones queue. Uploads are not capped.
    pub max_downloads: usize,
    /// Concurrent downloads for one user before further ones queue.
    pub max_downloads_per_client: usize,
    /// Open connections before further ones are refused; 0 means no limit.
    pub max_connections: usize,
    /// Open connections from one address before further ones are refused;
    /// 0 means no limit.
    pub max_connections_per_address: usize,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".to_string(),
            agreement: "Hotline".to_string(),
            server_name: "Hotline".to_string(),
            console_prefix: None,
            max_downloads: DEFAULT_MAX_DOWNLOADS,
            max_downloads_per_client: DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
            max_connections: DEFAULT_MAX_CONNECTIONS,
            max_connections_per_address: DEFAULT_MAX_CONNECTIONS_PER_ADDRESS,
        }
    }
}
