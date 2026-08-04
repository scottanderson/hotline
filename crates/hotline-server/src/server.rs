use std::sync::Arc;

use tokio::net::TcpListener;

use crate::accounts::AccountProvider;
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
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:5500".to_string(),
            agreement: "hotline-rs".to_string(),
            server_name: "hotline-rs".to_string(),
        }
    }
}

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: SharedState,
}

impl Server {
    /// Create a server with the given configuration, backed by `accounts`
    /// (see [`AccountProvider`]; the embedding application supplies it).
    pub fn new(config: ServerConfig, accounts: Arc<dyn AccountProvider>) -> Self {
        let state = SharedState::new(accounts);
        Self { config, state }
    }

    /// Bind and accept connections forever, spawning one task per client.
    /// Never returns unless the listener errors.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        tracing::info!(addr = %self.config.bind_addr, "hotline-server listening");

        loop {
            let (stream, peer) = listener.accept().await?;
            // Avoids Nagle delaying small replies behind the peer's ACKs.
            if let Err(e) = stream.set_nodelay(true) {
                tracing::warn!(%peer, error = %e, "failed to set TCP_NODELAY");
            }
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
