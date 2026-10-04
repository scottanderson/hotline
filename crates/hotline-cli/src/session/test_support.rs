//! A real in-process server for session tests.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use hotline_client::HotlineClient;
use hotline_proto::opcode::TransactionType;
use hotline_proto::{text_encoding, Field, Opcode, Received, Transaction};
use hotline_server::{Providers, Server, ServerConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(5);

/// A free port whose next port, for HTXF, is also free.
fn free_port() -> u16 {
    loop {
        let base = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = base.local_addr().unwrap().port();
        if port < u16::MAX && TcpListener::bind(("127.0.0.1", port + 1)).is_ok() {
            return port;
        }
    }
}

/// Start a server with `providers` and return its address.
pub async fn start_server(providers: Providers) -> SocketAddr {
    start_server_config(ServerConfig::default(), providers).await
}

/// Start a server with `config` on a free port and `providers`, and return its address.
pub async fn start_server_config(config: ServerConfig, providers: Providers) -> SocketAddr {
    let addr = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let config = ServerConfig {
        bind_addr: addr.to_string().into(),
        ..config
    };
    tokio::spawn(Server::new(config, providers).run());
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return addr;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not start");
}

/// A push as the client reads it in the default encoding.
pub fn push(opcode: Opcode, fields: Vec<Field>) -> Transaction<Received> {
    Transaction::push(opcode, fields).received(text_encoding::DEFAULT)
}

/// The next push with `opcode`, skipping every other push.
pub async fn next_push(
    events: &mut UnboundedReceiver<Transaction<Received>>,
    opcode: Opcode,
) -> Transaction<Received> {
    timeout(WAIT, async {
        loop {
            let tx = events.recv().await.expect("connection open");
            if tx.r#type() == TransactionType::Push(opcode) {
                return tx;
            }
        }
    })
    .await
    .expect("push arrives")
}

/// Log in as `admin` under `nick` and accept the agreement.
pub async fn join_admin(
    addr: SocketAddr,
    nick: &str,
) -> (HotlineClient, UnboundedReceiver<Transaction<Received>>) {
    let (client, mut events) = HotlineClient::connect(addr).await.unwrap();
    client.login("admin", "admin").await.unwrap();
    next_push(&mut events, Opcode::ShowAgreement).await;
    client.agree(nick, 0).await.unwrap();
    (client, events)
}
