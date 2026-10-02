//! Requests against a real in-process `hotline-server`.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use hotline_client::{ClientError, HotlineClient};
use hotline_proto::opcode::TransactionType;
use hotline_proto::types::HlPath;
use hotline_proto::{FieldId, Opcode, Transaction};
use hotline_server::{InMemoryFileProvider, Providers, Server, ServerConfig};
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
    start_server_with(Providers::default()).await
}

/// Start a server with `providers` and return its address.
async fn start_server_with(providers: Providers) -> SocketAddr {
    let addr = SocketAddr::from(([127, 0, 0, 1], free_port()));
    let config = ServerConfig {
        bind_addr: addr.to_string().into(),
        ..ServerConfig::default()
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

#[tokio::test]
async fn get_file_name_list_lists_the_root_and_a_folder() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::default(), "readme.txt", *b"hi");
    files.seed(&HlPath::new(["Games"]), "doom.wad", *b"wad");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let root = alice.get_file_name_list(&HlPath::default()).await.unwrap();
    let mut names: Vec<_> = root.iter().map(|f| (&*f.name, f.is_folder())).collect();
    names.sort_unstable();
    assert_eq!(names, [("Games", true), ("readme.txt", false)]);

    let games = alice
        .get_file_name_list(&HlPath::new(["Games"]))
        .await
        .unwrap();
    assert_eq!(games.len(), 1);
    assert_eq!(&*games[0].name, "doom.wad");
    assert_eq!(games[0].size, 3);
}

#[tokio::test]
async fn download_file_receives_the_data_fork() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::new(["Games"]), "doom.wad", *b"IWAD data");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let download = alice
        .download_file(&HlPath::new(["Games"]), "doom.wad")
        .await
        .unwrap();
    assert_eq!(download.file_size, 9);
    let mut data = Vec::new();
    let written = timeout(WAIT, download.receive(&mut data))
        .await
        .expect("download finishes")
        .unwrap();

    assert_eq!(written, 9);
    assert_eq!(data, b"IWAD data");
}

#[tokio::test]
async fn upload_file_stores_the_data_in_the_upload_folder() {
    let files = Arc::new(InMemoryFileProvider::default());
    files.seed(&HlPath::new(["Uploads"]), "keep.txt", *b"");
    let providers = Providers {
        files: files.clone(),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let uploads = HlPath::new(["Uploads"]);

    let upload = alice.upload_file(&uploads, "new.txt", 5).await.unwrap();
    let sent = timeout(WAIT, upload.send(&mut &b"hello"[..]))
        .await
        .expect("upload finishes")
        .unwrap();
    assert_eq!(sent, 5);

    let download = alice.download_file(&uploads, "new.txt").await.unwrap();
    let mut data = Vec::new();
    download.receive(&mut data).await.unwrap();
    assert_eq!(data, b"hello");
}

#[tokio::test]
async fn download_folder_steps_through_every_item() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::new(["Games"]), "doom.wad", *b"IWAD");
    files.seed(&HlPath::new(["Games", "Maps"]), "e1m1.map", *b"map");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;

    let download = admin
        .download_folder(&HlPath::default(), "Games")
        .await
        .unwrap();
    assert_eq!(download.item_count, 3);
    let mut receiver = timeout(WAIT, download.open()).await.unwrap().unwrap();
    let mut items = Vec::new();
    while let Some(item) = timeout(WAIT, receiver.next_item()).await.unwrap().unwrap() {
        let path = item.path.components.join("/");
        let mut data = Vec::new();
        if path == "doom.wad" {
            receiver.receive_file(&mut data).await.unwrap();
        }
        items.push((path, item.is_folder, data));
    }

    items.sort_unstable();
    assert_eq!(
        items,
        [
            ("Maps".to_string(), true, vec![]),
            ("Maps/e1m1.map".to_string(), false, vec![]),
            ("doom.wad".to_string(), false, b"IWAD".to_vec()),
        ]
    );
}

#[tokio::test]
async fn download_file_of_a_missing_file_is_a_server_error() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let result = alice.download_file(&HlPath::default(), "nothing").await;
    assert!(matches!(result, Err(ClientError::Server(_))), "{result:?}");
}
