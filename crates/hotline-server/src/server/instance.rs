use std::net::SocketAddr;

use tokio::net::TcpListener;

use crate::error::Result;
use crate::providers::Providers;
use crate::server::htxf::run_htxf_listener;
use crate::server::ServerConfig;
use crate::session::Session;
use crate::state::SharedState;

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: SharedState,
}

impl Server {
    /// Create a server with `config`, backed by `providers`.
    pub fn new(config: ServerConfig, providers: Providers) -> Self {
        let state = SharedState::new(providers);
        Self { config, state }
    }

    /// Bind and accept connections forever, spawning one task per client.
    /// Never returns unless the listener errors.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        tracing::info!(addr = %self.config.bind_addr, "hotline-server listening");

        let htxf_addr = SocketAddr::new(local_addr.ip(), local_addr.port() + 1);
        let htxf_listener = TcpListener::bind(htxf_addr).await?;
        tracing::info!(addr = %htxf_addr, "hotline-server listening for file transfers");
        let htxf_state = self.state.clone();
        tokio::spawn(async move {
            run_htxf_listener(htxf_listener, htxf_state).await;
        });

        loop {
            let (stream, peer) = listener.accept().await?;
            // Reject a banned address before the handshake.
            if self.state.is_banned(peer.ip()).await.is_some() {
                tracing::debug!(%peer, "rejected connection from banned address");
                continue;
            }
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
