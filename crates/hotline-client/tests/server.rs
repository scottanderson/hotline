//! Requests against a real in-process `hotline-server`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hotline_client::{
    AccountChange, Ban, ClientError, ClientUserInfo, HotlineClient, Password, UserAccount,
};
use hotline_proto::htxf::FolderItemHeader;
use hotline_proto::opcode::TransactionType;
use hotline_proto::types::{HlPath, UserChangeOptions};
use hotline_proto::{Access, FieldId, Opcode, Received, Transaction};
use hotline_server::{InMemoryFileProvider, Providers, Server, ServerConfig};
use tokio::net::TcpListener;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

/// A listener on a free port, and one on the next port for HTXF.
async fn listeners() -> (TcpListener, TcpListener) {
    loop {
        let base = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = base.local_addr().unwrap().port();
        if port == u16::MAX {
            continue;
        }
        if let Ok(htxf) = TcpListener::bind(("127.0.0.1", port + 1)).await {
            return (base, htxf);
        }
    }
}

/// Start a server with the in-memory backends and return its address.
async fn start_server() -> SocketAddr {
    start_server_with(Providers::default()).await
}

/// Start a server with `providers` and return its address.
async fn start_server_with(providers: Providers) -> SocketAddr {
    start_server_config(ServerConfig::default(), providers).await
}

/// Start a server with `config` on a free port and `providers`, and return its address.
async fn start_server_config(config: ServerConfig, providers: Providers) -> SocketAddr {
    let (listener, htxf_listener) = listeners().await;
    let addr = listener.local_addr().unwrap();
    tokio::spawn(Server::new(config, providers).run_on(listener, htxf_listener));
    addr
}

/// The next push with `opcode`, skipping every other push.
async fn next_push(
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

/// Log in as `login` under `nick` and accept the agreement.
async fn join_as(
    addr: SocketAddr,
    login: &str,
    password: &str,
    nick: &str,
) -> (HotlineClient, UnboundedReceiver<Transaction<Received>>) {
    let (client, mut events) = HotlineClient::connect(addr).await.unwrap();
    client.login(login, password).await.unwrap();
    next_push(&mut events, Opcode::ShowAgreement).await;
    client.agree(nick, 0).await.unwrap();
    (client, events)
}

/// Log in as `guest` under `nick` and accept the agreement.
async fn join(
    addr: SocketAddr,
    nick: &str,
) -> (HotlineClient, UnboundedReceiver<Transaction<Received>>) {
    join_as(addr, "guest", "", nick).await
}

async fn user_id(client: &HotlineClient, nick: &str) -> u16 {
    let users = client.get_user_name_list().await.unwrap();
    users.iter().find(|u| &*u.name == nick).unwrap().user_id
}

#[tokio::test]
async fn a_server_in_utf8_counts_a_nick_in_characters() {
    let utf8 = hotline_proto::text_encoding::from_label("utf-8").unwrap();
    let config = ServerConfig {
        encoding: utf8,
        ..ServerConfig::default()
    };
    let addr = start_server_config(config, Providers::default()).await;
    let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}";
    let mut clients = Vec::new();
    for nick in [konnichiwa, "bob"] {
        let (client, mut events) = HotlineClient::connect(addr).await.unwrap();
        client.encoding().set(utf8);
        client.login("guest", "").await.unwrap();
        next_push(&mut events, Opcode::ShowAgreement).await;
        client.agree(nick, 0).await.unwrap();
        clients.push((client, events));
    }

    clients[0].0.send_chat("hi").await.unwrap();

    let msg = next_push(&mut clients[1].1, Opcode::ChatMsg).await;
    assert_eq!(
        msg.string(FieldId::Data),
        Some(format!("\r        {konnichiwa}:  hi"))
    );
}

#[tokio::test]
async fn send_instant_msg_reaches_its_target() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let (_bob, mut bob_events) = join(addr, "bob").await;

    let bob_id = user_id(&alice, "bob").await;
    alice.send_instant_msg(bob_id, "hi bob").await.unwrap();

    let msg = next_push(&mut bob_events, Opcode::ServerMsg).await;
    assert_eq!(msg.string(FieldId::Data).as_deref(), Some("hi bob"));
    assert_eq!(msg.string(FieldId::UserName).as_deref(), Some("alice"));
}

#[tokio::test]
async fn disconnect_user_with_a_ban_tells_the_user_why() {
    let addr = start_server().await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let (_bob, mut bob_events) = join(addr, "bob").await;

    let bob_id = user_id(&admin, "bob").await;
    admin
        .disconnect_user(bob_id, "bob", Ban::Temporary)
        .await
        .unwrap();

    let msg = next_push(&mut bob_events, Opcode::DisconnectMsg).await;
    assert_eq!(
        msg.string(FieldId::Data).as_deref(),
        Some("You are temporarily banned on this server.")
    );
}

#[tokio::test]
async fn disconnect_user_without_the_privilege_is_a_server_error() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let (_bob, _bob_events) = join(addr, "bob").await;

    let bob_id = user_id(&alice, "bob").await;
    let result = alice.disconnect_user(bob_id, "bob", Ban::None).await;
    assert!(matches!(result, Err(ClientError::Server(_))), "{result:?}");
}

#[tokio::test]
async fn keep_connection_alive_is_traffic() {
    let addr = start_server().await;
    let (alice, _alice_events) = join(addr, "alice").await;

    let before = alice.last_traffic();
    alice.keep_connection_alive().await.unwrap();
    assert!(alice.last_traffic() > before);
}

#[tokio::test]
async fn user_broadcast_reaches_every_user() {
    let addr = start_server().await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let (_bob, mut bob_events) = join(addr, "bob").await;

    admin.user_broadcast("server going down").await.unwrap();

    let msg = next_push(&mut bob_events, Opcode::UserBroadcast).await;
    assert_eq!(
        msg.string(FieldId::Data).as_deref(),
        Some("server going down")
    );
}

#[tokio::test]
async fn set_client_user_info_renames_and_sets_the_automatic_response() {
    let addr = start_server().await;
    let (alice, mut alice_events) = join(addr, "alice").await;
    let (bob, _bob_events) = join(addr, "bob").await;
    next_push(&mut alice_events, Opcode::NotifyChangeUser).await;

    let info = ClientUserInfo {
        name: "robert".into(),
        icon: 7,
        options: UserChangeOptions::AUTO_RESPONSE,
        automatic_response: "away".into(),
    };
    bob.set_client_user_info(&info).await.unwrap();
    let change = next_push(&mut alice_events, Opcode::NotifyChangeUser).await;
    let user = change.user_entry().unwrap();
    assert_eq!((&*user.name, user.icon), ("robert", 7));

    alice.send_instant_msg(user.user_id, "hi").await.unwrap();
    let reply = next_push(&mut alice_events, Opcode::ServerMsg).await;
    assert_eq!(reply.string(FieldId::Data).as_deref(), Some("away"));
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
    let entry = post.string(FieldId::Data).unwrap();
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

/// The names in the folder at `path`, sorted.
async fn names(client: &HotlineClient, path: &HlPath) -> Vec<String> {
    let list = client.get_file_name_list(path).await.unwrap();
    let mut names: Vec<_> = list.iter().map(|f| f.name.to_string()).collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn file_operations_create_describe_move_alias_and_delete() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::default(), "readme.txt", *b"hi");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let addr = start_server_with(providers).await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let (root, games) = (HlPath::default(), HlPath::new(["Games"]));

    admin.new_folder(&root, "Games").await.unwrap();
    admin
        .set_file_info(&root, "readme.txt", "read.me", Some("a note"))
        .await
        .unwrap();
    let info = admin.get_file_info(&root, "read.me").await.unwrap();
    assert_eq!(
        (&*info.name, info.size, &*info.comment),
        ("read.me", 2, "a note")
    );
    admin
        .set_file_info(&root, "read.me", "read.me", Some(""))
        .await
        .unwrap();
    let info = admin.get_file_info(&root, "read.me").await.unwrap();
    assert_eq!(&*info.comment, "");

    admin.move_file(&root, "read.me", &games).await.unwrap();
    assert_eq!(names(&admin, &games).await, ["read.me"]);
    admin
        .make_file_alias(&games, "read.me", &root)
        .await
        .unwrap();
    assert_eq!(names(&admin, &root).await, ["Games", "read.me"]);

    admin.delete_file(&root, "Games").await.unwrap();
    assert_eq!(names(&admin, &root).await, ["read.me"]);
}

#[tokio::test]
async fn kill_download_takes_a_download_out_of_the_queue() {
    let files = InMemoryFileProvider::default();
    files.seed(&HlPath::default(), "a.txt", *b"a");
    let providers = Providers {
        files: Arc::new(files),
        ..Providers::default()
    };
    let config = ServerConfig {
        max_downloads_per_client: 1,
        ..ServerConfig::default()
    };
    let addr = start_server_config(config, providers).await;
    let (alice, _alice_events) = join(addr, "alice").await;
    let root = HlPath::default();

    let first = alice.download_file(&root, "a.txt", 0).await.unwrap();
    assert_eq!(first.waiting_count, 0);
    let second = alice.download_file(&root, "a.txt", 0).await.unwrap();
    assert_eq!(second.waiting_count, 1);
    alice.kill_download(second.reference()).await.unwrap();
    let third = alice.download_file(&root, "a.txt", 0).await.unwrap();
    assert_eq!(third.waiting_count, 1);
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
async fn accounts_are_created_read_changed_and_deleted_with_each_opcode_set() {
    let addr = start_server().await;
    let (admin, _admin_events) = join_as(addr, "admin", "admin", "root").await;
    let carol = UserAccount {
        name: "Carol".into(),
        login: "carol".into(),
        password: Password::New("pw".into()),
        access: Access::READ_CHAT,
    };

    admin.new_user(&carol).await.unwrap();
    let read = admin.get_user("carol").await.unwrap();
    assert_eq!(
        read,
        UserAccount {
            password: Password::Unchanged,
            ..carol.clone()
        }
    );
    let changed = UserAccount {
        access: Access::READ_CHAT | Access::SEND_CHAT,
        ..read
    };
    admin.set_user(&changed).await.unwrap();
    assert_eq!(admin.get_user("carol").await.unwrap(), changed);

    let renamed = UserAccount {
        login: "visitor".into(),
        password: Password::Empty,
        ..changed
    };
    admin
        .set_user_list(&[AccountChange::Save {
            old_login: Some("carol".into()),
            account: renamed.clone(),
        }])
        .await
        .unwrap();
    let accounts = admin.get_user_list().await.unwrap();
    assert!(accounts.contains(&renamed));
    assert!(!accounts.iter().any(|a| &*a.login == "carol"));

    admin
        .set_user_list(&[AccountChange::Delete("visitor".into())])
        .await
        .unwrap();
    admin.new_user(&carol).await.unwrap();
    admin.delete_user("carol").await.unwrap();
    let logins: Vec<_> = admin
        .get_user_list()
        .await
        .unwrap()
        .into_iter()
        .map(|a| a.login)
        .collect();
    assert_eq!(logins, ["admin".into(), "guest".into()]);
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
    let text = msg.string(FieldId::Data).unwrap();
    assert!(text.contains("hi room"), "{text:?}");

    alice.set_chat_subject(chat_id, "plans").await.unwrap();
    let subject = next_push(&mut bob_events, Opcode::NotifyChatSubject).await;
    assert_eq!(
        subject.string(FieldId::ChatSubject).as_deref(),
        Some("plans")
    );

    bob.leave_chat(chat_id).await.unwrap();
    let left = next_push(&mut alice_events, Opcode::NotifyChatDeleteUser).await;
    assert_eq!(left.field(FieldId::UserID).unwrap().as_u16(), Some(bob_id));
}
