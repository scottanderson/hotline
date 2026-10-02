//! Requests against a real in-process `hotline-server`.

use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use std::time::Duration;

use hotline_client::{ClientError, HotlineClient};
use hotline_proto::htxf::FolderItemHeader;
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
        .download_file(&HlPath::new(["Games"]), "doom.wad", 0)
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
async fn download_file_from_an_offset_receives_the_rest() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::default(), "doom.wad", *b"IWAD data");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let download = alice
        .download_file(&HlPath::default(), "doom.wad", 5)
        .await
        .unwrap();
    let mut data = Vec::new();
    timeout(WAIT, download.receive(&mut data))
        .await
        .expect("download finishes")
        .unwrap();

    assert_eq!(data, b"data");
}

#[tokio::test]
async fn upload_file_resumes_a_partial_upload() {
    let files = Arc::new(InMemoryFileProvider::default());
    files.seed(&HlPath::new(["Uploads"]), "new.txt.hpf", *b"hel");
    let providers = Providers {
        files: files.clone(),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let uploads = HlPath::new(["Uploads"]);

    let upload = alice
        .upload_file(&uploads, "new.txt", 5, true)
        .await
        .unwrap();
    assert_eq!(upload.offset(), 3);
    timeout(WAIT, upload.send(&mut &b"lo"[..]))
        .await
        .expect("upload finishes")
        .unwrap();

    let download = alice.download_file(&uploads, "new.txt", 0).await.unwrap();
    let mut data = Vec::new();
    download.receive(&mut data).await.unwrap();
    assert_eq!(data, b"hello");
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

    let upload = alice
        .upload_file(&uploads, "new.txt", 5, false)
        .await
        .unwrap();
    let sent = timeout(WAIT, upload.send(&mut &b"hello"[..]))
        .await
        .expect("upload finishes")
        .unwrap();
    assert_eq!(sent, 5);

    let download = alice.download_file(&uploads, "new.txt", 0).await.unwrap();
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
async fn download_folder_resumes_a_file_from_an_offset() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::new(["Games"]), "doom.wad", *b"IWAD");
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
    let mut receiver = timeout(WAIT, download.open()).await.unwrap().unwrap();
    let item = receiver.next_item().await.unwrap().unwrap();
    assert_eq!(item.path.components, ["doom.wad"]);
    let mut data = Vec::new();
    timeout(WAIT, receiver.resume_file(&mut data, 2))
        .await
        .unwrap()
        .unwrap();

    assert_eq!(data, b"AD");
    assert!(receiver.next_item().await.unwrap().is_none());
}

#[tokio::test]
async fn upload_folder_stores_every_item() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::new(["Uploads"]), "keep.txt", *b"");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let uploads = HlPath::new(["Uploads"]);
    let items: [(&[&str], Option<&[u8]>); 3] = [
        (&["a.txt"], Some(b"alpha")),
        (&["Sub"], None),
        (&["Sub", "b.txt"], Some(b"beta")),
    ];

    let upload = admin
        .upload_folder(&uploads, "Pack", 3, 9, false)
        .await
        .unwrap();
    let mut sender = upload.open().await.unwrap().expect("a transfer");
    for (path, data) in items {
        let header = FolderItemHeader {
            is_folder: data.is_none(),
            path: HlPath::new(path.iter().copied()),
        };
        let wanted = timeout(WAIT, sender.send_item(&header))
            .await
            .unwrap()
            .unwrap();
        if let (Some(offset), Some(data)) = (wanted, data) {
            assert_eq!(offset, 0);
            sender
                .send_file(&mut &data[..], data.len() as u64)
                .await
                .unwrap();
        }
    }
    timeout(WAIT, sender.finish()).await.unwrap().unwrap();

    let download = admin.download_folder(&uploads, "Pack").await.unwrap();
    let mut receiver = download.open().await.unwrap();
    let mut received = Vec::new();
    while let Some(item) = receiver.next_item().await.unwrap() {
        let mut data = Vec::new();
        if !item.is_folder {
            receiver.receive_file(&mut data).await.unwrap();
        }
        received.push((item.path.components.join("/"), data));
    }
    received.sort_unstable();
    assert_eq!(
        received,
        [
            ("Sub".to_string(), vec![]),
            ("Sub/b.txt".to_string(), b"beta".to_vec()),
            ("a.txt".to_string(), b"alpha".to_vec()),
        ]
    );
}

#[tokio::test]
async fn download_file_of_a_missing_file_is_a_server_error() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let result = alice.download_file(&HlPath::default(), "nothing", 0).await;
    assert!(matches!(result, Err(ClientError::Server(_))), "{result:?}");
}

#[tokio::test]
async fn threaded_news_lists_posts_reads_and_deletes() {
    let addr = start_server().await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let root = HlPath::default();
    let general = HlPath::new(["General"]);

    admin.new_news_fldr(&root, "Archive").await.unwrap();
    admin.new_news_cat(&root, "General").await.unwrap();
    let mut names: Vec<_> = admin
        .get_news_cat_name_list(&root)
        .await
        .unwrap()
        .iter()
        .map(|entry| entry.name().to_string())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["Archive", "General"]);

    admin
        .post_news_art(&general, 0, "Hello", "first post")
        .await
        .unwrap();
    admin
        .post_news_art(&general, 1, "Re: Hello", "a reply")
        .await
        .unwrap();
    let articles = admin.get_news_art_name_list(&general).await.unwrap();
    let thread: Vec<_> = articles
        .iter()
        .map(|a| (a.id, a.parent_id, &*a.title))
        .collect();
    assert_eq!(thread, [(1, 0, "Hello"), (2, 1, "Re: Hello")]);

    let article = admin
        .get_news_art_data(&general, 1, hotline_client::TEXT_PLAIN)
        .await
        .unwrap();
    assert_eq!(&*article.title, "Hello");
    assert_eq!(&*article.poster, "root");
    assert_eq!(article.first_child_id, 2);
    assert_eq!(article.data, b"first post");

    admin.del_news_art(&general, 1, true).await.unwrap();
    assert!(admin
        .get_news_art_name_list(&general)
        .await
        .unwrap()
        .is_empty());
    admin.del_news_item(&general).await.unwrap();
    assert_eq!(admin.get_news_cat_name_list(&root).await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_private_chat_room_carries_chat_subject_and_leave() {
    let addr = start_server().await;
    let (alice, mut alice_events) = join_as(addr, "admin", "admin", "alice").await;
    let (bob, mut bob_events) = join_as(addr, "admin", "admin", "bob").await;
    let encoding = hotline_proto::text_encoding::DEFAULT;

    let bob_id = user_id(&alice, "bob").await;
    let room = alice.invite_new_chat(bob_id).await.unwrap();
    assert_eq!(room.users.len(), 1);
    let invite = next_push(&mut bob_events, Opcode::InviteToChat).await;
    let chat_id = invite.field(FieldId::ChatID).unwrap().as_u32().unwrap();
    assert_eq!(chat_id, room.id);

    let joined = bob.join_chat(chat_id).await.unwrap();
    let mut names: Vec<_> = joined.users.iter().map(|u| &*u.name).collect();
    names.sort_unstable();
    assert_eq!(names, ["alice", "bob"]);
    next_push(&mut alice_events, Opcode::NotifyChatChangeUser).await;

    alice.send_room_chat(chat_id, "hi room").await.unwrap();
    let msg = next_push(&mut bob_events, Opcode::ChatMsg).await;
    assert_eq!(msg.field(FieldId::ChatID).unwrap().as_u32(), Some(chat_id));
    let text = msg.string(FieldId::Data, encoding).unwrap();
    assert!(text.contains("hi room"), "{text:?}");

    alice.set_chat_subject(chat_id, "plans").await.unwrap();
    let subject = next_push(&mut bob_events, Opcode::NotifyChatSubject).await;
    assert_eq!(
        subject.string(FieldId::ChatSubject, encoding).as_deref(),
        Some("plans")
    );

    bob.leave_chat(chat_id).await.unwrap();
    let left = next_push(&mut alice_events, Opcode::NotifyChatDeleteUser).await;
    assert_eq!(left.field(FieldId::UserID).unwrap().as_u16(), Some(bob_id));
}
