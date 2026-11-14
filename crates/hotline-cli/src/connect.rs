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
    /// The name of the bookmark or of the tracked server, if any.
    pub name: Option<Box<str>>,
    pub host: String,
    pub port: u16,
    pub login: String,
    pub password: String,
    /// The text encoding of the connection.
    pub encoding: &'static Encoding,
}

/// The host and the port of `address`, `host` or `host:port`, with
/// [`DEFAULT_PORT`] when it names no port. `None` when the host is blank or
/// the port is not a number. An IPv6 host with a port is in brackets, as in
/// `[::1]:5500`.
pub fn parse_address(address: &str) -> Option<(String, u16)> {
    let text = address.trim();
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) if !host.contains(':') || host.ends_with(']') => {
            (host, port.parse().ok()?)
        }
        _ => (text, DEFAULT_PORT),
    };
    (!host.is_empty()).then(|| host.to_string()).zip(Some(port))
}

impl Target {
    /// The name of the server as the status shows it: [`Target::name`], or
    /// else the host.
    pub fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.host)
    }

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
    fn display_name_is_the_name_or_else_the_host() {
        let mut target = Target {
            name: None,
            host: "example.com".into(),
            port: 6000,
            login: String::new(),
            password: String::new(),
            encoding: hotline_proto::text_encoding::DEFAULT,
        };
        assert_eq!(target.display_name(), "example.com");
        target.name = Some("Home".into());
        assert_eq!(target.display_name(), "Home");
    }

    #[test]
    fn short_address_leaves_out_only_the_default_port() {
        assert_eq!(short_address("example.com", DEFAULT_PORT), "example.com");
        assert_eq!(short_address("[::1]", 6000), "[::1]:6000");
    }

    #[test]
    fn parse_address_takes_the_port_after_the_last_colon() {
        assert_eq!(
            parse_address("example.com:6000"),
            Some(("example.com".into(), 6000))
        );
        assert_eq!(
            parse_address(" example.com "),
            Some(("example.com".into(), 5500))
        );
        assert_eq!(parse_address("[::1]:6000"), Some(("[::1]".into(), 6000)));
        assert_eq!(parse_address("[::1]"), Some(("[::1]".into(), 5500)));
    }

    #[test]
    fn parse_address_refuses_a_blank_host_or_a_bad_port() {
        assert_eq!(parse_address(""), None);
        assert_eq!(parse_address(":6000"), None);
        assert_eq!(parse_address("example.com:x"), None);
        assert_eq!(parse_address("example.com:70000"), None);
    }
}
