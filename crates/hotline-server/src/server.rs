use tokio::net::TcpListener;

use hotline_proto::consts::AccessPrivileges;

use crate::error::Result;
use crate::session::Session;
use crate::state::SharedState;

/// Server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Address to bind, e.g. `"0.0.0.0:5500"`.
    pub bind_addr: String,
    /// Agreement text shown to clients on login (empty = no agreement).
    pub agreement: String,
    /// Name reported in the `Login` reply (protocol version >= 151).
    pub server_name: String,
    /// Privileges every user gets on login (no account database yet).
    pub default_access: AccessPrivileges,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".to_string(),
            agreement: String::new(),
            server_name: "hotline-rs".to_string(),
            default_access: AccessPrivileges::guest_default(),
        }
    }
}

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: SharedState,
}

impl Server {
    /// Create a server with the given configuration and fresh shared state.
    pub fn new(config: ServerConfig) -> Self {
        Self {
            config,
            state: SharedState::default(),
        }
    }

    /// Bind and accept connections forever, spawning one task per client.
    /// Never returns unless the listener errors.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        tracing::info!(addr = %self.config.bind_addr, "hotline-server listening");

        loop {
            let (stream, peer) = listener.accept().await?;
            let state = self.state.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                tracing::debug!(%peer, "connection accepted");
                if let Err(e) = Session::run(stream, state, config).await {
                    tracing::warn!(%peer, error = %e, "session ended with error");
                }
            });
        }
    }
}
