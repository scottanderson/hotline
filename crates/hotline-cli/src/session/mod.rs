//! [`ClientSession`] state: what the client knows about the connection,
//! decoupled from the terminal. Data plus pure update logic.

mod accounts;
mod chat;
mod chat_room;
mod event_outcome;
mod file_ops;
mod keep_alive;
mod mode;
mod request;
mod running;
#[cfg(test)]
pub(crate) mod test_support;
mod threaded_news;
mod transfers;

use std::collections::VecDeque;
use std::path::PathBuf;

use hotline_client::{Ban, ClientInfoText, ClientUserInfo, HotlineClient, UserAccount};
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::{to_wire_lines, EncodingHandle};
use hotline_proto::types::{FileNameWithInfo, HlPath, UserChangeOptions, UserNameWithInfo};
use hotline_proto::FieldId;
use hotline_proto::{Access, Field, Received, Transaction};

use crate::line::{Line, LineKind, Scrollback};

pub use chat_room::{ChatInvite, ChatRoom};
pub use event_outcome::{Disconnect, EventOutcome};
pub use keep_alive::KEEP_ALIVE;
pub use mode::Mode;
pub use request::{Next, Request};
use running::RunningTransfer;
pub use threaded_news::{ArticleText, NewsFolder, NewsFolderNode, NewsTarget, NewsThread};
pub use transfers::TransferLine;

pub struct ClientSession {
    pub name: Box<str>,
    pub icon: u16,
    pub options: UserChangeOptions,
    /// The reply to a private message while [`UserChangeOptions::AUTO_RESPONSE`] is on.
    pub automatic_response: Box<str>,
    pub mode: Mode,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Scrollback,
    /// The classic news, `None` until it is fetched.
    pub news: Option<String>,
    /// The number of changes to [`Self::news`] and of news posts pushed.
    pub news_revision: u64,
    /// The listing of the folder at `file_path`, `None` until it is fetched.
    pub files: Option<Vec<FileNameWithInfo>>,
    pub file_path: HlPath,
    /// The folder that downloads are saved in.
    pub download_dir: PathBuf,
    pub access: Access,
    /// The server's version from the login reply.
    pub server_version: Option<u16>,
    /// The accounts on the Admin tab, `None` until they are fetched.
    pub accounts: Option<Vec<UserAccount>>,
    /// The threaded news bundles and categories, as the tree shows them.
    /// `None` until they are fetched.
    pub news_folders: Option<Vec<NewsFolderNode>>,
    /// The category whose threads show.
    pub news_category: Option<HlPath>,
    /// The articles of `news_category`, in thread order.
    pub news_threads: Vec<NewsThread>,
    /// The article that the preview shows.
    pub news_preview: Option<ArticleText>,
    /// The private chat rooms this client is in, oldest first.
    pub rooms: Vec<ChatRoom>,
    /// Invitations that wait for an answer, oldest first.
    pub invites: VecDeque<ChatInvite>,
    /// The transfers that run, oldest first.
    transfers: Vec<RunningTransfer>,
    /// The ID of the last transfer kept in `transfers`.
    next_transfer_id: u64,
    /// The error lines shown in the chat.
    error_count: u64,
    /// This connection's active text encoding, shared with the client's codec.
    encoding: EncodingHandle,
}

impl ClientSession {
    pub fn new(name: impl Into<Box<str>>, icon: u16, encoding: EncodingHandle) -> Self {
        Self {
            name: name.into(),
            icon,
            options: UserChangeOptions::empty(),
            automatic_response: Box::default(),
            mode: Mode::Joining,
            users: Vec::new(),
            lines: Scrollback::from(Line::system(
                "connected - type a message and press Enter, Esc to quit",
            )),
            news: None,
            news_revision: 0,
            files: None,
            file_path: HlPath::default(),
            download_dir: PathBuf::from("."),
            access: Access::empty(),
            server_version: None,
            accounts: None,
            news_folders: None,
            news_category: None,
            news_threads: Vec::new(),
            news_preview: None,
            rooms: Vec::new(),
            invites: VecDeque::new(),
            transfers: Vec::new(),
            next_transfer_id: 0,
            error_count: 0,
            encoding,
        }
    }

    /// Show the server's agreement text and wait for the user to accept it.
    pub fn show_agreement(&mut self, text: String) {
        self.mode = Mode::Agreement(text);
    }

    /// Accept the agreement, [`myTran_Agreed`] (121), and switch to chat.
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
    pub fn accept_agreement(&mut self, client: &HotlineClient) -> Request<()> {
        self.mode = Mode::Chat;
        let client = client.clone();
        let info = self.user_info();
        Request::new(async move {
            let agreed = client.agree(&info).await;
            let users = client.get_user_name_list().await;
            move |session: &mut Self| {
                if let Err(e) = agreed {
                    session.push(format!("failed to complete login: {e}"), LineKind::Error);
                }
                match users {
                    Ok(users) => session.users = users,
                    Err(e) => {
                        session.push(format!("failed to fetch user list: {e}"), LineKind::Error)
                    }
                }
            }
        })
    }

    fn push(&mut self, text: impl Into<Box<str>>, kind: LineKind) {
        if kind == LineKind::Error {
            self.error_count = self.error_count.wrapping_add(1);
        }
        self.lines.push(Line::new(text, kind));
    }

    /// The error lines shown in the chat so far.
    pub const fn error_count(&self) -> u64 {
        self.error_count
    }

    /// `true` if the server's version is `version` or later.
    fn server_at_least(&self, version: u16) -> bool {
        self.server_version.is_some_and(|v| v >= version)
    }

    /// Show the error text of a reply that no request waits for.
    fn show_unmatched_error(&mut self, tx: &Transaction<Received>) {
        if tx.is_error() {
            let text = tx
                .string(FieldId::ErrorText)
                .unwrap_or_else(|| "unknown error".to_string());
            self.push(text, LineKind::Error);
        }
    }

    /// Apply one server-initiated transaction (chat relay, user list
    /// change, permission denial, ...) to the session state.
    pub fn apply_server_event(&mut self, tx: &Transaction<Received>) -> EventOutcome {
        let TransactionType::Push(opcode) = tx.r#type() else {
            self.show_unmatched_error(tx);
            return EventOutcome::Applied;
        };
        match opcode {
            /* 102 */ Opcode::NewMsg => self.apply_new_msg(tx),
            /* 104 */ Opcode::ServerMsg => self.apply_server_msg(tx),
            /* 106 */ Opcode::ChatMsg => self.apply_chat_msg(tx),
            /* 109 */ Opcode::ShowAgreement => return self.apply_show_agreement(tx),
            /* 111 */ Opcode::DisconnectMsg => return self.apply_disconnect_msg(tx),
            /* 113 */ Opcode::InviteToChat => self.apply_invite_to_chat(tx),
            /* 117 */ Opcode::NotifyChatChangeUser => self.apply_notify_chat_change_user(tx),
            /* 118 */ Opcode::NotifyChatDeleteUser => self.apply_notify_chat_delete_user(tx),
            /* 119 */ Opcode::NotifyChatSubject => self.apply_notify_chat_subject(tx),
            /* 122 */ Opcode::ServerBanner => self.apply_server_banner(tx),
            /* 211 */ Opcode::DownloadInfo => self.apply_download_info(tx),
            /* 301 */ Opcode::NotifyChangeUser => self.apply_notify_change_user(tx),
            /* 302 */ Opcode::NotifyDeleteUser => self.apply_notify_delete_user(tx),
            /* 354 */ Opcode::UserAccess => self.apply_user_access(tx),
            /* 355 */ Opcode::UserBroadcast => self.apply_user_broadcast(tx),
            _ => {
                for line in format!("Unrecognized opcode for transaction: {tx:#?}").lines() {
                    self.push(line, LineKind::Error);
                }
            }
        }
        EventOutcome::Applied
    }

    /// [`myTran_ShowAgreement`] (109): show the text and wait for acceptance.
    /// With no text, the caller accepts right away.
    ///
    /// [`myTran_ShowAgreement`]: hotline_proto::Opcode::ShowAgreement
    fn apply_show_agreement(&mut self, tx: &Transaction<Received>) -> EventOutcome {
        match tx.string(FieldId::Data) {
            Some(text) => {
                self.show_agreement(text);
                EventOutcome::Applied
            }
            None => EventOutcome::NeedsAgreementAccept,
        }
    }

    /// [`myTran_ServerMsg`] (104).
    ///
    /// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
    fn apply_server_msg(&mut self, tx: &Transaction<Received>) {
        let text = tx.string(FieldId::Data);
        let from = tx.string(FieldId::UserName);
        match (from, text) {
            // A private message carries the sender's name; a
            // permission-denied or admin broadcast notice
            // doesn't.
            (Some(from), Some(text)) => {
                self.push(
                    format!("[PM] {from}: {}", display_text(&text)),
                    LineKind::Chat,
                );
            }
            (None, Some(text)) => {
                self.push(display_text(&text), LineKind::Error);
            }
            _ => {}
        }
    }

    /// [`myTran_NewMsg`] (102): a new classic news post.
    ///
    /// [`myTran_NewMsg`]: hotline_proto::Opcode::NewMsg
    fn apply_new_msg(&mut self, tx: &Transaction<Received>) {
        if let Some(entry) = tx.string(FieldId::Data) {
            if let Some(news) = &mut self.news {
                news.insert_str(0, &entry);
            }
            self.news_revision = self.news_revision.wrapping_add(1);
            self.push("new news post", LineKind::System);
        }
    }

    /// [`myTran_ChatMsg`] (106).
    ///
    /// [`myTran_ChatMsg`]: hotline_proto::Opcode::ChatMsg
    fn apply_chat_msg(&mut self, tx: &Transaction<Received>) {
        if self.room_chat_msg(tx) {
            return;
        }
        if let Some(text) = tx.string(FieldId::Data) {
            self.push(display_text(&text), LineKind::Chat);
        }
    }

    /// [`myTran_DisconnectMsg`] (111): the server's reason to end the
    /// connection. A message without text changes nothing.
    ///
    /// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
    fn apply_disconnect_msg(&mut self, tx: &Transaction<Received>) -> EventOutcome {
        match tx.string(FieldId::Data).filter(|text| !text.is_empty()) {
            Some(text) => {
                let reason = Disconnect::Message(display_text(&text).into());
                EventOutcome::Disconnected(self.disconnect(reason))
            }
            None => EventOutcome::Applied,
        }
    }

    /// End the session with `reason`: show it in the chat, and leave the
    /// agreement. Returns `reason`.
    pub fn disconnect(&mut self, reason: Disconnect) -> Disconnect {
        self.push(reason.line(), LineKind::Error);
        if matches!(self.mode, Mode::Agreement(_)) {
            self.mode = Mode::Chat;
        }
        reason
    }

    /// [`myTran_ServerBanner`] (122): show the banner's link, if it has one.
    /// A banner image is not shown.
    ///
    /// [`myTran_ServerBanner`]: hotline_proto::Opcode::ServerBanner
    fn apply_server_banner(&mut self, tx: &Transaction<Received>) {
        if let Some(url) = tx.string(FieldId::ServerBannerUrl) {
            self.push(format!("server banner: {url}"), LineKind::System);
        }
    }

    /// [`myTran_DownloadInfo`] (211): a queued download's place in the queue.
    ///
    /// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
    fn apply_download_info(&mut self, tx: &Transaction<Received>) {
        let waiting = tx
            .field(FieldId::WaitingCount)
            .map(Field::as_u32)
            .unwrap_or_default();
        let name = tx
            .field(FieldId::RefNum)
            .map(Field::as_u32)
            .and_then(|reference| self.set_queue_place(reference, waiting));
        let name = name.as_deref().unwrap_or("a download");
        self.push(
            format!("{name} waits, {waiting} ahead in the queue"),
            LineKind::System,
        );
    }

    /// [`myTran_NotifyChangeUser`] (301).
    ///
    /// [`myTran_NotifyChangeUser`]: hotline_proto::Opcode::NotifyChangeUser
    fn apply_notify_change_user(&mut self, tx: &Transaction<Received>) {
        let Some(user) = tx.user_entry() else {
            return;
        };
        match self.users.iter_mut().find(|u| u.user_id == user.user_id) {
            Some(old) => {
                let (old_name, name) = (&old.name, &user.name);
                let renamed = (!old_name.is_empty() && !name.is_empty() && old_name != name)
                    .then(|| format!(" <<< {old_name} is now known as {name} >>>"));
                *old = user;
                if let Some(line) = renamed {
                    self.push(line, LineKind::System);
                }
            }
            None => {
                self.push(format!("{} joined", user.name), LineKind::System);
                self.users.push(user);
            }
        }
    }

    /// [`myTran_NotifyDeleteUser`] (302).
    ///
    /// [`myTran_NotifyDeleteUser`]: hotline_proto::Opcode::NotifyDeleteUser
    fn apply_notify_delete_user(&mut self, tx: &Transaction<Received>) {
        let Some(user_id) = tx.field(FieldId::UserID).map(Field::as_u16) else {
            return;
        };
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
        for room in &mut self.rooms {
            room.users.retain(|u| u.user_id != user_id);
        }
    }

    /// [`myTran_UserAccess`] (354).
    ///
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
    fn apply_user_access(&mut self, tx: &Transaction<Received>) {
        self.access = tx.access().unwrap_or(self.access);

        // This session's own uid/icon/name.
        if let Some(result) = tx.user_list_entry() {
            match result {
                Ok(entry) => match self.users.iter_mut().find(|u| u.user_id == entry.user_id) {
                    Some(existing) => *existing = entry,
                    None => self.users.push(entry),
                },
                Err(e) => self.push(
                    format!("failed to parse own user info: {e}"),
                    LineKind::Error,
                ),
            }
        }
    }

    /// [`myTran_UserBroadcast`] (355): a message from an administrator to every user.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    fn apply_user_broadcast(&mut self, tx: &Transaction<Received>) {
        let Some(text) = tx.string(FieldId::Data) else {
            return;
        };
        let from = match tx.string(FieldId::UserName) {
            Some(name) => format!(" from {name}"),
            None => String::new(),
        };
        self.push(
            format!("[Administrator Message{from}] {}", display_text(&text)),
            LineKind::Chat,
        );
    }

    /// The privileges that the client applies. Before server version 182,
    /// a private chat and a private message need no privilege, and a folder
    /// download or upload needs the privilege for a file.
    pub fn granted(&self) -> Access {
        if self.server_at_least(182) {
            return self.access;
        }
        let mut access = self.access | Access::CREATE_CHAT | Access::SEND_MESSAGE;
        access.set(
            Access::DOWNLOAD_FOLDER,
            self.access.contains(Access::DOWNLOAD_FILE),
        );
        access.set(
            Access::UPLOAD_FOLDER,
            self.access.contains(Access::UPLOAD_FILE),
        );
        access
    }

    /// [`myTran_UserBroadcast`] (355), showing an error line if it fails.
    ///
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    pub fn user_broadcast(client: &HotlineClient, text: &str) -> Request<()> {
        let client = client.clone();
        let text = to_wire_lines(text);
        Request::report(
            async move { client.user_broadcast(&text).await },
            "failed to broadcast",
            |_, ()| {},
        )
    }

    /// Send `line` as chat, reporting a failed send as a local error line
    /// (a transport error may never come back as a server push).
    pub async fn send_chat(&mut self, client: &HotlineClient, line: &str) {
        if let Err(e) = client.send_chat(&to_wire_lines(line)).await {
            self.push(format!("failed to send: {e}"), LineKind::Error);
        }
    }

    /// [`myTran_GetClientInfoText`] (303), showing an error line if it fails.
    ///
    /// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
    pub fn get_client_info_text(
        client: &HotlineClient,
        user_id: u16,
    ) -> Request<Option<ClientInfoText>> {
        let client = client.clone();
        Request::report(
            async move { client.get_client_info_text(user_id).await },
            "failed to get info",
            |_, info| Some(info),
        )
    }

    pub fn get_msgs(client: &HotlineClient) -> Request<()> {
        let client = client.clone();
        Request::report(
            async move { client.get_msgs().await },
            "failed to get news",
            |session: &mut Self, news| {
                if session.news.as_ref() != Some(&news) {
                    session.news_revision = session.news_revision.wrapping_add(1);
                }
                session.news = Some(news);
            },
        )
    }

    /// [`myTran_GetFileNameList`] (200) for `path`. On success, `path` becomes
    /// the current folder and this returns `true`.
    ///
    /// [`myTran_GetFileNameList`]: hotline_proto::Opcode::GetFileNameList
    pub fn get_file_name_list(client: &HotlineClient, path: HlPath) -> Request<bool> {
        let client = client.clone();
        Request::report(
            async move {
                client
                    .get_file_name_list(&path)
                    .await
                    .map(|files| (files, path))
            },
            "failed to list files",
            |session: &mut Self, (files, path)| {
                session.files = Some(files);
                session.file_path = path;
                true
            },
        )
    }

    /// The path of the folder `name` in the current folder.
    pub fn child_folder(&self, name: &str) -> HlPath {
        let mut path = self.file_path.clone();
        path.components.push(name.to_string());
        path
    }

    /// The path of the current folder's parent, `None` at the root.
    pub fn parent_folder(&self) -> Option<HlPath> {
        let mut path = self.file_path.clone();
        path.components.pop()?;
        Some(path)
    }

    pub fn old_post_news(client: &HotlineClient, text: &str) -> Request<()> {
        let client = client.clone();
        let text = to_wire_lines(text);
        Request::report(
            async move { client.old_post_news(&text).await },
            "failed to post news",
            |_, ()| {},
        )
    }

    /// This user's name, icon, and options.
    pub fn user_info(&self) -> ClientUserInfo {
        ClientUserInfo {
            name: self.name.clone(),
            icon: self.icon,
            options: self.options,
            automatic_response: self.automatic_response.clone(),
        }
    }

    /// Change this user's name, icon, and options,
    /// [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub async fn set_client_user_info(&mut self, client: &HotlineClient, info: ClientUserInfo) {
        if let Err(e) = client.set_client_user_info(&info).await {
            return self.push(format!("failed to change options: {e}"), LineKind::Error);
        }
        self.name = info.name;
        self.icon = info.icon;
        self.options = info.options;
        self.automatic_response = info.automatic_response;
    }

    /// [`myTran_DisconnectUser`] (110), showing an error line if it fails.
    ///
    /// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
    pub fn disconnect_user(&self, client: &HotlineClient, user_id: u16, ban: Ban) -> Request<()> {
        let client = client.clone();
        let name = self.user_name(user_id);
        let failed = format!("failed to disconnect {name}");
        Request::report(
            async move { client.disconnect_user(user_id, &name, ban).await },
            failed,
            |_, ()| {},
        )
    }

    /// The name of the user `user_id`, or `#<user_id>` for a user not in the list.
    pub fn user_name(&self, user_id: u16) -> String {
        match self.users.iter().find(|u| u.user_id == user_id) {
            Some(user) => user.name.to_string(),
            None => format!("#{user_id}"),
        }
    }

    pub fn send_instant_msg(client: &HotlineClient, user_id: u16, text: &str) -> Request<()> {
        let client = client.clone();
        let text = to_wire_lines(text);
        Request::report(
            async move { client.send_instant_msg(user_id, &text).await.map(|()| text) },
            "failed to send",
            move |session: &mut Self, text| {
                let to = session.user_name(user_id);
                session.push(format!("[PM to {to}] {text}"), LineKind::Chat);
            },
        )
    }
}

/// Strip a leading `\r`/`\n` some servers prepend to chat/status text;
/// each `Line` here is already its own row.
fn display_text(s: &str) -> String {
    s.trim_start_matches(['\r', '\n']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::types::UserFlags;

    #[test]
    fn granted_adds_the_old_server_privileges() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        session.access = Access::DOWNLOAD_FILE | Access::UPLOAD_FOLDER;
        assert_eq!(
            session.granted(),
            Access::DOWNLOAD_FILE
                | Access::DOWNLOAD_FOLDER
                | Access::CREATE_CHAT
                | Access::SEND_MESSAGE
        );
        session.server_version = Some(182);
        assert_eq!(
            session.granted(),
            Access::DOWNLOAD_FILE | Access::UPLOAD_FOLDER
        );
    }

    #[test]
    fn new_msg_adds_the_post_to_the_top_of_the_news() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        session.news = Some("From bob\rold".to_string());
        let post = test_support::push(
            Opcode::NewMsg,
            vec![Field::string(FieldId::Data, "From alice\rhello")],
        );
        let _ = session.apply_server_event(&post);
        assert_eq!(
            session.news.as_deref(),
            Some("From alice\rhelloFrom bob\rold")
        );
        assert_eq!(&*session.lines.last().unwrap().text, "new news post");
        assert_eq!(session.news_revision, 1);
    }

    #[test]
    fn disconnect_msg_ends_the_session_with_the_reason() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        session.show_agreement("terms".into());
        let tx = test_support::push(
            Opcode::DisconnectMsg,
            vec![Field::string(FieldId::Data, "\rback soon")],
        );
        let outcome = session.apply_server_event(&tx);
        assert!(
            matches!(outcome, EventOutcome::Disconnected(Disconnect::Message(text)) if &*text == "back soon")
        );
        assert_eq!(&*session.lines.last().unwrap().text, "back soon");
        assert!(matches!(session.mode, Mode::Chat));
    }

    #[test]
    fn disconnect_msg_without_text_changes_nothing() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let tx = test_support::push(Opcode::DisconnectMsg, vec![]);
        let outcome = session.apply_server_event(&tx);
        assert!(matches!(outcome, EventOutcome::Applied));
        assert_eq!(session.lines.len(), 1);
    }

    #[tokio::test]
    async fn get_msgs_counts_only_a_change() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        for _ in 0..2 {
            let apply = ClientSession::get_msgs(&client).reply().await;
            let _ = apply(&mut session);
        }
        assert_eq!(session.news_revision, 1);
    }

    #[test]
    fn new_msg_counts_a_post_before_the_news_is_fetched() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let post = test_support::push(Opcode::NewMsg, vec![Field::string(FieldId::Data, "hi")]);
        let _ = session.apply_server_event(&post);
        assert_eq!((session.news.as_deref(), session.news_revision), (None, 1));
    }

    #[test]
    fn child_and_parent_folder_walk_the_path() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        assert_eq!(session.parent_folder(), None);
        session.file_path = session.child_folder("Games");
        assert_eq!(session.child_folder("Doom"), HlPath::new(["Games", "Doom"]));
        assert_eq!(session.parent_folder(), Some(HlPath::default()));
    }

    #[test]
    fn server_banner_shows_its_url() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let banner = test_support::push(
            Opcode::ServerBanner,
            vec![
                Field::bytes(FieldId::ServerBannerType, b"URL ".to_vec()),
                Field::string(FieldId::ServerBannerUrl, "http://example.com"),
            ],
        );
        let _ = session.apply_server_event(&banner);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "server banner: http://example.com"
        );
    }

    #[test]
    fn show_unmatched_error_shows_the_error_text() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let invite = Transaction::request(Opcode::InviteToChat, 7, vec![]).received(encoding);
        let error = invite.reply_error("no such user");
        let _ = session.apply_server_event(&error.received(encoding));
        assert_eq!(&*session.lines.last().unwrap().text, "no such user");
        assert_eq!(session.lines.last().unwrap().kind, LineKind::Error);
    }

    #[test]
    fn server_banner_ignores_an_image_banner() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let before = session.lines.len();
        let banner = test_support::push(
            Opcode::ServerBanner,
            vec![Field::bytes(FieldId::ServerBannerType, b"JPEG".to_vec())],
        );
        let _ = session.apply_server_event(&banner);
        assert_eq!(session.lines.len(), before);
    }

    #[test]
    fn user_broadcast_names_the_sender() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let broadcast = test_support::push(
            Opcode::UserBroadcast,
            vec![
                Field::u16(FieldId::UserID, 1),
                Field::string(FieldId::Data, "going down"),
                Field::string(FieldId::UserName, "root"),
            ],
        );
        let _ = session.apply_server_event(&broadcast);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "[Administrator Message from root] going down"
        );
    }

    #[test]
    fn download_info_shows_the_place_in_the_queue() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let info = test_support::push(
            Opcode::DownloadInfo,
            vec![
                Field::u32(FieldId::RefNum, 9),
                Field::u32(FieldId::WaitingCount, 2),
            ],
        );
        let _ = session.apply_server_event(&info);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "a download waits, 2 ahead in the queue"
        );
    }

    #[tokio::test]
    async fn download_info_shows_the_place_of_its_download() {
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let task = tokio::spawn(std::future::pending::<()>());
        let progress = std::sync::Arc::new(running::Progress::default());
        progress.set_reference(9);
        session.track(
            running::Kind::Download,
            "doom.wad",
            std::time::Instant::now(),
            progress,
            task.abort_handle(),
        );
        let info = test_support::push(
            Opcode::DownloadInfo,
            vec![
                Field::u32(FieldId::RefNum, 9),
                Field::u32(FieldId::WaitingCount, 2),
            ],
        );
        let _ = session.apply_server_event(&info);
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "doom.wad waits, 2 ahead in the queue"
        );
        assert_eq!(
            session.running_transfers()[0].label,
            "All downloads slots are full. “doom.wad” is number 2 in the server queue. Please wait..."
        );
        task.abort();
    }

    #[tokio::test]
    async fn set_client_user_info_keeps_the_new_name_and_options() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, mut events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        let info = ClientUserInfo {
            name: "you".into(),
            icon: 3,
            options: UserChangeOptions::REFUSE_CHAT,
            automatic_response: Box::default(),
        };

        session.set_client_user_info(&client, info.clone()).await;
        assert_eq!(session.user_info(), info);
        let change = test_support::next_push(&mut events, Opcode::NotifyChangeUser).await;
        let user = change.user_entry().unwrap();
        assert_eq!((&*user.name, user.icon), ("you", 3));
    }

    #[test]
    fn chat_msg_shows_each_line_on_its_own() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let chat = test_support::push(
            Opcode::ChatMsg,
            vec![Field::string(FieldId::Data, "\rAlice: one\rtwo")],
        );
        let _ = session.apply_server_event(&chat);
        let texts: Vec<&str> = session.lines.iter().map(|l| &*l.text).collect();
        assert!(texts.ends_with(&["Alice: one", "two"]));
    }

    #[test]
    fn display_text_strips_leading_cr() {
        assert_eq!(display_text("\rAlice: hi"), "Alice: hi");
    }

    #[test]
    fn display_text_leaves_normal_text_alone() {
        assert_eq!(display_text("Alice: hi"), "Alice: hi");
    }

    /// [`myTran_InviteToChat`] (113) is both the client's invite request
    /// and the server's resulting push; a value-based classification
    /// would misread this push and silently drop it here.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    #[test]
    fn chat_invite_push_is_recognized_despite_sharing_its_wire_value_with_a_client_opcode() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let invite = test_support::push(
            Opcode::InviteToChat,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u32(FieldId::ChatID, 7),
            ],
        );
        let _ = session.apply_server_event(&invite);
        assert!(session
            .lines
            .iter()
            .any(|line| line.text.contains("Alice invited you to a private chat")));
        assert_eq!(
            session.invites.front(),
            Some(&ChatInvite {
                chat_id: 7,
                from: "Alice".into()
            })
        );
    }

    #[test]
    fn notify_change_user_carries_admin_color_for_new_and_existing_users() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let admin_change = test_support::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, UserFlags::ADMIN.bits()),
            ],
        );

        // New user: flags should carry the admin bit from the start.
        let _ = session.apply_server_event(&admin_change);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(user.flags.contains(UserFlags::ADMIN));

        // Existing user: a later update should still reflect current flags.
        let demoted = test_support::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&demoted);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(!user.flags.contains(UserFlags::ADMIN));
    }

    #[test]
    fn notify_change_user_notes_a_rename() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
        let joined = test_support::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&joined);

        let renamed = test_support::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alicia"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&renamed);

        assert_eq!(
            &*session.lines.last().unwrap().text,
            " <<< Alice is now known as Alicia >>>"
        );
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert_eq!(&*user.name, "Alicia");
    }
}
