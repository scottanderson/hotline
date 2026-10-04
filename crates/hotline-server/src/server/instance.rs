use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::error::Result;
use crate::flood::FloodDecision;
use crate::providers::Providers;
use crate::server::connections::ConnectionLimits;
use crate::server::htxf::run_htxf_listener;
use crate::server::ServerConfig;
use crate::session::Session;
use crate::state::SharedState;

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: SharedState,
    connections: Arc<ConnectionLimits>,
}

impl Server {
    /// Create a server with `config`, backed by `providers`.
    pub fn new(config: ServerConfig, providers: Providers) -> Self {
        let state = SharedState::new(providers);
        state.set_download_limits(config.max_downloads, config.max_downloads_per_client);
        let connections =
            ConnectionLimits::new(config.max_connections, config.max_connections_per_address);
        Self {
            config,
            state,
            connections,
        }
    }

    /// This server's cross-session state, for a caller that runs its own
    /// task alongside [`Server::run`].
    pub const fn state(&self) -> &SharedState {
        &self.state
    }

    /// Bind `bind_addr` and the next port for file transfers, then
    /// [`Server::run_on`] them.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&*self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let htxf_addr = SocketAddr::new(local_addr.ip(), local_addr.port() + 1);
        let htxf_listener = TcpListener::bind(htxf_addr).await?;
        self.run_on(listener, htxf_listener).await
    }

    /// Accept connections on `listener` and file transfers on
    /// `htxf_listener` forever, spawning one task per client. A client
    /// finds `htxf_listener` on the port after `listener`'s. Never returns
    /// unless the listener errors.
    pub async fn run_on(self, listener: TcpListener, htxf_listener: TcpListener) -> Result<()> {
        tracing::info!(addr = %listener.local_addr()?, "hotline-server listening");
        tracing::info!(
            addr = %htxf_listener.local_addr()?,
            "hotline-server listening for file transfers"
        );
        let htxf_state = self.state.clone();
        tokio::spawn(async move {
            run_htxf_listener(htxf_listener, htxf_state).await;
        });

        loop {
            let (stream, peer) = listener.accept().await?;
            let Some(permit) = self.connections.try_acquire(peer.ip()) else {
                tracing::debug!(%peer, "rejected connection over the connection limit");
                continue;
            };
            if self.state.check_connection_flood(peer.ip()).await == FloodDecision::Ban {
                tracing::info!(%peer, "banned for connection flooding");
                self.state.temp_ban(peer.ip()).await;
                continue;
            }
            // Avoids Nagle delaying small replies behind the peer's ACKs.
            if let Err(e) = stream.set_nodelay(true) {
                tracing::warn!(%peer, error = %e, "failed to set TCP_NODELAY");
            }
            let state = self.state.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                let _permit = permit;
                tracing::debug!(%peer, "connection accepted");
                if let Err(e) = Session::run(stream, state, config).await {
                    tracing::warn!(%peer, error = %e, "session ended with error");
                }
            });
        }
    }
}
