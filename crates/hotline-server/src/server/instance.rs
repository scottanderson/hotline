use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::error::Result;
use crate::flood::FloodDecision;
use crate::providers::Providers;
use crate::server::ServerConfig;
use crate::server::connections::ConnectionLimits;
use crate::server::htxf::run_htxf_listener;
use crate::session::Session;
use crate::state::SharedState;

/// A running (or not-yet-started) Hotline server.
pub struct Server {
    config: ServerConfig,
    state: Arc<SharedState>,
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
    pub const fn state(&self) -> &Arc<SharedState> {
        &self.state
    }

    /// Bind `bind_addr` and the next port for file transfers, then
    /// [`Server::run_on`] them.
    pub async fn run(self) -> Result<()> {
        let listener = TcpListener::bind(&*self.config.bind_addr).await?;
        let local_addr = listener.local_addr()?;
        let htxf_listener = TcpListener::bind(port_above(local_addr, 1)?).await?;
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
            if self.state.is_quitting() {
                tracing::debug!(%peer, "rejected connection while quitting");
                continue;
            }
            let Some(permit) = self.connections.try_acquire(peer.ip()) else {
                tracing::debug!(%peer, "rejected connection over the connection limit");
                continue;
            };
            if self.state.check_connection_flood(peer.ip()).await == FloodDecision::Ban {
                if self.state.is_banned(peer.ip()).await.is_none() {
                    tracing::warn!(%peer, "banned for connection flooding");
                }
                self.state.temp_ban(peer.ip()).await;
                continue;
            }
            // Avoids Nagle delaying small replies behind the peer's ACKs.
            if let Err(e) = stream.set_nodelay(true) {
                tracing::debug!(%peer, error = %e, "failed to set TCP_NODELAY");
            }
            let state = self.state.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                let _permit = permit;
                tracing::debug!(%peer, "connection accepted");
                if let Err(e) = Session::run(stream, state, config).await {
                    tracing::debug!(%peer, error = %e, "session ended with error");
                }
            });
        }
    }
}

/// The address `n` ports above `addr`. `Err` past port 65535.
fn port_above(addr: SocketAddr, n: u16) -> Result<SocketAddr> {
    let port = addr.port().checked_add(n).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the port {n} above {addr} is larger than 65535"),
        )
    })?;
    Ok(SocketAddr::new(addr.ip(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_on_closes_a_connection_while_quitting() {
        use tokio::io::AsyncReadExt;

        let server = Server::new(ServerConfig::default(), Providers::default());
        server.state().start_quit();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let htxf_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(server.run_on(listener, htxf_listener));

        let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
        let mut buf = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut buf),
        )
        .await
        .expect("the server should close the socket");
        assert_eq!(read.unwrap(), 0);
    }

    #[test]
    fn port_above_adds_to_the_port() {
        let addr: SocketAddr = "127.0.0.1:5500".parse().unwrap();
        assert_eq!(port_above(addr, 2).unwrap().port(), 5502);
    }

    #[test]
    fn port_above_65535_is_an_error() {
        let addr: SocketAddr = "127.0.0.1:65534".parse().unwrap();
        assert_eq!(port_above(addr, 1).unwrap().port(), 65535);
        assert!(port_above(addr, 2).is_err());
    }
}
