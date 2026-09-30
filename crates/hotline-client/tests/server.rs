//! Requests against a real in-process `hotline-server`.

use std::net::{SocketAddr, TcpListener};
use std::time::Duration;

use hotline_client::HotlineClient;
use hotline_proto::opcode::TransactionType;
use hotline_proto::{FieldId, Opcode, Transaction};
use hotline_server::{Providers, Server, ServerConfig};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

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

/// Start a server with the in-memory backends and return its address.
async fn start_server() -> SocketAddr {
    let addr = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let config = ServerConfig {
        bind_addr: addr.to_string().into(),
        ..ServerConfig::default()
    };
    tokio::spawn(Server::new(config, Providers::default()).run());
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return addr;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("server did not start");
}

/// The next push with `opcode`, skipping every other push.
async fn next_push(events: &mut UnboundedReceiver<Transaction>, opcode: Opcode) -> Transaction {
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

/// Log in as `login` under `nick` and accept the agreement.
async fn join_as(
    addr: SocketAddr,
    login: &str,
    password: &str,
    nick: &str,
) -> (HotlineClient, UnboundedReceiver<Transaction>) {
    let (client, mut events) = HotlineClient::connect(addr).await.unwrap();
    client.login(login, password).await.unwrap();
    next_push(&mut events, Opcode::ShowAgreement).await;
    client.agree(nick, 0).await.unwrap();
    (client, events)
}

/// Log in as `guest` under `nick` and accept the agreement.
async fn join(addr: SocketAddr, nick: &str) -> (HotlineClient, UnboundedReceiver<Transaction>) {
    join_as(addr, "guest", "", nick).await
}

async fn user_id(client: &HotlineClient, nick: &str) -> u16 {
    let users = client.get_user_name_list().await.unwrap();
    users.iter().find(|u| &*u.name == nick).unwrap().user_id
}

#[tokio::test]
async fn send_instant_msg_reaches_its_target() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let (_bob, mut bob_events) = join(addr, "bob").await;

    let bob_id = user_id(&alice, "bob").await;
    alice.send_instant_msg(bob_id, "hi bob").await.unwrap();

    let msg = next_push(&mut bob_events, Opcode::ServerMsg).await;
    let encoding = hotline_proto::text_encoding::DEFAULT;
    assert_eq!(
        msg.string(FieldId::Data, encoding).as_deref(),
        Some("hi bob")
    );
    assert_eq!(
        msg.string(FieldId::UserName, encoding).as_deref(),
        Some("alice")
    );
}

#[tokio::test]
async fn get_client_info_text_names_the_user() {
    let addr = start_server().await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let (_bob, _bob_events) = join(addr, "bob").await;

    let bob_id = user_id(&admin, "bob").await;
    let info = admin.get_client_info_text(bob_id).await.unwrap();
    assert_eq!(&*info.name, "bob");
    assert!(
        info.text.starts_with("Nickname:   bob\r"),
        "{:?}",
        info.text
    );
}

#[tokio::test]
async fn old_post_news_reaches_readers_and_get_msgs() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let (_bob, mut bob_events) = join(addr, "bob").await;

    alice.old_post_news("hello news").await.unwrap();

    let post = next_push(&mut bob_events, Opcode::NewMsg).await;
    let encoding = hotline_proto::text_encoding::DEFAULT;
    let entry = post.string(FieldId::Data, encoding).unwrap();
    assert!(entry.contains("hello news"), "{entry:?}");
    assert!(alice.get_msgs().await.unwrap().contains("hello news"));
}
