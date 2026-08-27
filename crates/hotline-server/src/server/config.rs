/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind, e.g. `"0.0.0.0:5500"`. The file transfer data
    /// connection listens one port above this.
    pub bind_addr: String,
    /// Agreement text shown to clients on login (empty = no agreement).
    pub agreement: String,
    /// Name reported in the `Login` reply (protocol version >= 151).
    pub server_name: String,
    /// Chat prefix for console commands; `None` relays all chat.
    pub console_prefix: Option<char>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".to_string(),
            agreement: "Hotline".to_string(),
            server_name: "Hotline".to_string(),
            console_prefix: None,
        }
    }
}
