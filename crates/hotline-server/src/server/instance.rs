use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::account::AccountProvider;
use crate::ban::BanProvider;
use crate::banner::BannerProvider;
use crate::error::Result;
use crate::file::FileProvider;
use crate::flood::FloodPolicy;
use crate::news::NewsProvider;
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
    /// Create a server with the given configuration, backed by `accounts`
    /// (see [`AccountProvider`]), `files` (see [`FileProvider`]), `news`
    /// (see [`NewsProvider`]), `bans` (see [`BanProvider`]), `flood` (see
    /// [`FloodPolicy`]), and `banner` (see [`BannerProvider`]) - the
    /// embedding application supplies all six concrete backends.
    pub fn new(
        config: ServerConfig,
        accounts: Arc<dyn AccountProvider>,
        files: Arc<dyn FileProvider>,
        news: Arc<dyn NewsProvider>,
        bans: Arc<dyn BanProvider>,
        flood: Arc<dyn FloodPolicy>,
        banner: Arc<dyn BannerProvider>,
    ) -> Self {
        let state = SharedState::new(accounts, files, news, bans, flood, banner);
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
            // Reject a banned address before it can consume a handshake or
            // any other protocol resource, matching how the reference
            // server checks its ban list immediately on accept.
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
