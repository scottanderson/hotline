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
}

/// One entry in [`Modal::UserMenu`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuItem {
    SendMessage,
    GetInfo,
}

impl MenuItem {
    pub const ALL: [Self; 2] = [Self::SendMessage, Self::GetInfo];

    pub const fn label(self) -> &'static str {
        match self {
            Self::SendMessage => "Send message",
            Self::GetInfo => "Get info",
        }
    }
}
