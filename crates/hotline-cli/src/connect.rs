//! A connection to a server, and the login on it.

use hotline_client::{HotlineClient, LoginResult};
use hotline_proto::text_encoding::Encoding;
use hotline_proto::{Received, Transaction};
use tokio::sync::mpsc;

/// The port of a server whose address names none.
pub const DEFAULT_PORT: u16 = 5500;

/// A server, and the account that logs in to it.
#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub login: String,
    pub password: String,
    /// The text encoding of the connection.
    pub encoding: &'static Encoding,
}

impl Target {
    /// The address of the server, `host:port`.
    pub fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

/// `host`, or `host:port` when `port` is not [`DEFAULT_PORT`].
pub fn short_address(host: &str, port: u16) -> String {
    if port == DEFAULT_PORT {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

/// A connection that has logged in.
pub struct Connection {
    pub client: HotlineClient,
    /// The pushes of the server.
    pub events: mpsc::UnboundedReceiver<Transaction<Received>>,
    pub login: LoginResult,
}

/// Connect to `target` and log in.
pub async fn connect(target: Target) -> anyhow::Result<Connection> {
    let (client, events) = HotlineClient::connect(target.address()).await?;
    client.encoding().set(target.encoding);
    let login = client.login(&target.login, &target.password).await?;
    Ok(Connection {
        client,
        events,
        login,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_address_leaves_out_only_the_default_port() {
        assert_eq!(short_address("example.com", DEFAULT_PORT), "example.com");
        assert_eq!(short_address("[::1]", 6000), "[::1]:6000");
    }
}
