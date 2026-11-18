use hotline_client::{ClientInfoText, FileInfo};
use hotline_proto::Access;

use super::{AccountForm, ConnectForm, OptionsForm, Palette, TextBox};
use crate::config::Config;
use crate::file_edit::FileEdit;
use crate::session::{ArticleText, ChatInvite, Disconnect, NewsFolder, NewsTarget};
use crate::transfer::Conflict;

/// A box drawn over the main screen that takes every key until it closes.
#[derive(Debug, PartialEq, Eq)]
pub enum Modal {
    /// A private message to one user.
    InstantMsg {
        user_id: u16,
        name: Box<str>,
        input: TextBox,
    },
    /// The right-click menu for one user, drawn at `at`.
    UserMenu {
        user_id: u16,
        at: (u16, u16),
        items: &'static [MenuItem],
        selected: usize,
    },
    /// A choice to disconnect a user, with or without a ban.
    Disconnect { user_id: u16, name: Box<str> },
    /// A message to every connected user.
    Broadcast { input: TextBox },
    /// This user's name, icon, and options.
    Options(OptionsForm),
    /// A user's info text.
    Info(ClientInfoText),
    /// The local path of a file or folder to upload to the current folder.
    Upload { input: TextBox },
    /// A transfer that waits for resume, replace, or cancel.
    Confirm(Conflict),
    /// The text of `edit` on the file `name`.
    FileEdit {
        edit: FileEdit,
        name: Box<str>,
        input: TextBox,
    },
    /// A check before `hlc` closes the connection and quits.
    Quit,
    /// The reason the server ended the connection.
    Disconnected(Disconnect),
    /// A server to connect to.
    Connect(ConnectForm),
    /// The name of the bookmark of `form`. An empty `input` gives the
    /// default name.
    BookmarkName { form: ConnectForm, input: TextBox },
    /// The bookmarks of the settings file.
    Bookmarks {
        rows: Vec<BookmarkRow>,
        selected: usize,
    },
    /// A check before the bookmark at `selected` of `rows` is deleted.
    /// `rows` and `selected` come back to [`Modal::Bookmarks`] on a cancel.
    DeleteBookmark {
        rows: Vec<BookmarkRow>,
        selected: usize,
    },
    /// A check before the file or folder `name` is deleted.
    DeleteFile { name: Box<str> },
    /// The info of a file or folder.
    FileInfo(FileInfo),
    /// A new local name for the download of `name`.
    SaveAs { name: Box<str>, input: TextBox },
    /// An invitation to a private chat room.
    Invite(ChatInvite),
    /// A new subject for the room `chat_id`.
    Subject { chat_id: u32, input: TextBox },
    /// A threaded news article to read.
    Article(ArticleText),
    /// A new article: a reply to `parent_id`, or a new thread if `0`.
    Post {
        parent_id: u32,
        title: TextBox,
        body: TextBox,
        /// `true` while keys go to `body`.
        on_body: bool,
    },
    /// The name of a new bundle or category.
    NewsName { kind: NewsFolder, input: TextBox },
    /// A check before the threaded news folder or article `target` is
    /// deleted.
    DeleteNews { target: NewsTarget, name: Box<str> },
    /// An account to create or change.
    Account(AccountForm),
    /// The login of an account to read.
    OpenAccount { input: TextBox },
    /// A check before the account `login` is deleted.
    DeleteAccount { login: Box<str> },
    /// The command palette.
    Palette(Palette),
    /// The users to invite to the room `chat_id`, as `(user ID, name)`.
    PickUser {
        chat_id: u32,
        users: Vec<(u16, Box<str>)>,
        selected: usize,
    },
}

impl Modal {
    /// The kind of this modal, without its data.
    pub const fn kind(&self) -> ModalKind {
        match self {
            Self::InstantMsg { .. } => ModalKind::InstantMsg,
            Self::UserMenu { .. } => ModalKind::UserMenu,
            Self::Disconnect { .. } => ModalKind::Disconnect,
            Self::Broadcast { .. } => ModalKind::Broadcast,
            Self::Options(_) => ModalKind::Options,
            Self::Info(_) => ModalKind::Info,
            Self::Upload { .. } => ModalKind::Upload,
            Self::Confirm(_) => ModalKind::Confirm,
            Self::FileEdit { .. } => ModalKind::FileEdit,
            Self::Quit => ModalKind::Quit,
            Self::Disconnected(_) => ModalKind::Disconnected,
            Self::Connect(_) => ModalKind::Connect,
            Self::BookmarkName { .. } => ModalKind::BookmarkName,
            Self::Bookmarks { .. } => ModalKind::Bookmarks,
            Self::DeleteBookmark { .. } => ModalKind::DeleteBookmark,
            Self::DeleteFile { .. } => ModalKind::DeleteFile,
            Self::FileInfo(_) => ModalKind::FileInfo,
            Self::SaveAs { .. } => ModalKind::SaveAs,
            Self::Invite(_) => ModalKind::Invite,
            Self::Subject { .. } => ModalKind::Subject,
            Self::Article(_) => ModalKind::Article,
            Self::Post { .. } => ModalKind::Post,
            Self::NewsName { .. } => ModalKind::NewsName,
            Self::DeleteNews { .. } => ModalKind::DeleteNews,
            Self::Account(_) => ModalKind::Account,
            Self::OpenAccount { .. } => ModalKind::OpenAccount,
            Self::DeleteAccount { .. } => ModalKind::DeleteAccount,
            Self::Palette(_) => ModalKind::Palette,
            Self::PickUser { .. } => ModalKind::PickUser,
        }
    }

    /// The text box with the focus, in a modal that has one.
    pub const fn input_mut(&mut self) -> Option<&mut TextBox> {
        match self {
            Self::Post {
                title,
                body,
                on_body,
                ..
            } => Some(if *on_body { body } else { title }),
            Self::InstantMsg { input, .. }
            | Self::Broadcast { input }
            | Self::Upload { input }
            | Self::SaveAs { input, .. }
            | Self::Subject { input, .. }
            | Self::NewsName { input, .. }
            | Self::FileEdit { input, .. }
            | Self::OpenAccount { input }
            | Self::BookmarkName { input, .. } => Some(input),
            _ => None,
        }
    }

    /// The bookmarks of `config`, with the row at `selected` selected, or
    /// the last row.
    pub fn bookmarks(config: &Config, selected: usize) -> Self {
        let rows: Vec<_> = config
            .bookmarks
            .iter()
            .map(|bookmark| {
                let name = bookmark.name();
                BookmarkRow {
                    auto_connect: config.auto_connect.as_deref() == Some(&*name),
                    name: name.into(),
                    address: bookmark.address.clone(),
                    login: bookmark.login.clone(),
                }
            })
            .collect();
        Self::Bookmarks {
            selected: selected.min(rows.len().saturating_sub(1)),
            rows,
        }
    }

    /// The modal that asks how to start a transfer with `conflict`.
    pub fn ask(conflict: Conflict) -> Self {
        match conflict {
            Conflict::LocalFileExists { name, save_as } => Self::SaveAs {
                name,
                input: save_as.into(),
            },
            conflict => Self::Confirm(conflict),
        }
    }
}

/// One row of [`Modal::Bookmarks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkRow {
    pub name: Box<str>,
    pub address: Box<str>,
    pub login: Box<str>,
    /// `true` for the bookmark that connects at the start.
    pub auto_connect: bool,
}

/// A [`Modal`] variant without its data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModalKind {
    InstantMsg,
    UserMenu,
    Disconnect,
    Broadcast,
    Options,
    Info,
    Upload,
    Confirm,
    FileEdit,
    Quit,
    Disconnected,
    Connect,
    BookmarkName,
    Bookmarks,
    DeleteBookmark,
    DeleteFile,
    FileInfo,
    SaveAs,
    Invite,
    Subject,
    Article,
    Post,
    NewsName,
    DeleteNews,
    Account,
    OpenAccount,
    DeleteAccount,
    Palette,
    PickUser,
}

/// One entry in [`Modal::UserMenu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    SendMessage,
    GetInfo,
    PrivateChat,
    Disconnect,
}

impl MenuItem {
    pub const ALL: [Self; 4] = [
        Self::SendMessage,
        Self::GetInfo,
        Self::PrivateChat,
        Self::Disconnect,
    ];

    /// The privileges, any one of which turns the item on.
    pub const fn enabled_by(self) -> Access {
        match self {
            Self::SendMessage => Access::SEND_MESSAGE,
            Self::GetInfo => Access::GET_CLIENT_INFO,
            Self::PrivateChat => Access::CREATE_CHAT,
            Self::Disconnect => Access::DISCON_USER,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::SendMessage => "Send message",
            Self::GetInfo => "Get info",
            Self::PrivateChat => "Private chat",
            Self::Disconnect => "Disconnect",
        }
    }
}
