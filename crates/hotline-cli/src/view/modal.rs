use hotline_client::{ClientInfoText, FileInfo};

use super::{AccountForm, OptionsForm};
use crate::file_edit::FileEdit;
use crate::session::{ArticleText, ChatInvite, NewsFolder};
use crate::transfer::Conflict;

/// A box drawn over the main screen that takes every key until it closes.
#[derive(Debug, PartialEq, Eq)]
pub enum Modal {
    /// A private message to one user.
    InstantMsg {
        user_id: u16,
        name: Box<str>,
        input: String,
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
    Broadcast { input: String },
    /// This user's name, icon, and options.
    Options(OptionsForm),
    /// A user's info text.
    Info(ClientInfoText),
    /// The local path of a file or folder to upload to the current folder.
    Upload { input: String },
    /// A transfer that waits for resume, replace, or cancel.
    Confirm(Conflict),
    /// The text of `edit` on the file `name`.
    FileEdit {
        edit: FileEdit,
        name: Box<str>,
        input: String,
    },
    /// A check before `hlc` closes the connection and quits.
    Quit,
    /// A check before the file or folder `name` is deleted.
    DeleteFile { name: Box<str> },
    /// The info of a file or folder.
    FileInfo(FileInfo),
    /// A new local name for the download of `name`.
    SaveAs { name: Box<str>, input: String },
    /// An invitation to a private chat room.
    Invite(ChatInvite),
    /// A new subject for the room `chat_id`.
    Subject { chat_id: u32, input: String },
    /// A threaded news article to read.
    Article(ArticleText),
    /// A new article: a reply to `parent_id`, or a new thread if `0`.
    Post {
        parent_id: u32,
        title: String,
        body: String,
        /// `true` while keys go to `body`.
        on_body: bool,
    },
    /// The name of a new bundle or category.
    NewsName { kind: NewsFolder, input: String },
    /// A check before the threaded news item at `index` is deleted.
    DeleteNews { index: usize, name: Box<str> },
    /// An account to create or change.
    Account(AccountForm),
    /// The login of an account to read.
    OpenAccount { input: String },
    /// A check before the account `login` is deleted.
    DeleteAccount { login: Box<str> },
    /// The downloads to cancel, as `(ID, label)`.
    PickDownload {
        downloads: Vec<(u64, Box<str>)>,
        selected: usize,
    },
    /// The users to invite to the room `chat_id`, as `(user ID, name)`.
    PickUser {
        chat_id: u32,
        users: Vec<(u16, Box<str>)>,
        selected: usize,
    },
}

impl Modal {
    /// The text box with the focus, in a modal that has one.
    pub const fn input_mut(&mut self) -> Option<&mut String> {
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
            | Self::OpenAccount { input } => Some(input),
            _ => None,
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

    /// The menu items, with [`Self::Disconnect`] only if `can_disconnect`.
    pub const fn items(can_disconnect: bool) -> &'static [Self] {
        if can_disconnect {
            &Self::ALL
        } else {
            &[Self::SendMessage, Self::GetInfo, Self::PrivateChat]
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
