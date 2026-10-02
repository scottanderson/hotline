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
        selected: usize,
    },
    /// A user's info text.
    Info { name: Box<str>, text: Box<str> },
    /// The local path of a file or folder to upload to the current folder.
    Upload { input: String },
    /// A transfer that waits for resume, replace, or cancel.
    Confirm(Conflict),
    /// A new local name for the download of `name`.
    SaveAs { name: Box<str>, input: String },
    /// An invitation to a private chat room.
    Invite { chat_id: u32, from: Box<str> },
    /// A new subject for the room `chat_id`.
    Subject { chat_id: u32, input: String },
    /// A threaded news article to read.
    Article {
        id: u32,
        title: Box<str>,
        poster: Box<str>,
        text: Box<str>,
    },
    /// A new article: a reply to `parent_id`, or a new thread if `0`.
    Post {
        parent_id: u32,
        title: String,
        body: String,
        /// `true` while keys go to `body`.
        on_body: bool,
    },
    /// The name of a new bundle, or a category if `category`.
    NewsName { category: bool, input: String },
    /// A check before the threaded news item at `index` is deleted.
    DeleteNews { index: usize, name: Box<str> },
    /// The users to invite to the room `chat_id`, as `(user ID, name)`.
    PickUser {
        chat_id: u32,
        users: Vec<(u16, Box<str>)>,
        selected: usize,
    },
}

impl Modal {
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
}

impl MenuItem {
    pub const ALL: [Self; 3] = [Self::SendMessage, Self::GetInfo, Self::PrivateChat];

    pub const fn label(self) -> &'static str {
        match self {
            Self::SendMessage => "Send message",
            Self::GetInfo => "Get info",
            Self::PrivateChat => "Private chat",
        }
    }
}
