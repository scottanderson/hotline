use crate::transfer::Start;

/// What the caller must do after one key press or mouse click.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Send public chat, sent unchanged even when it starts with `/`.
    SendChat(String),
    SendInstantMsg {
        user_id: u16,
        text: String,
    },
    GetClientInfoText(u16),
    GetMsgs,
    OldPostNews(String),
    /// List the current folder again.
    ListFiles,
    /// List the named folder in the current folder.
    OpenFolder(Box<str>),
    /// List the current folder's parent.
    ParentFolder,
    /// Download the file `name` in the current folder as the local file `save_as`.
    DownloadFile {
        name: Box<str>,
        save_as: Box<str>,
        start: Start,
    },
    /// Download the folder `name` in the current folder.
    DownloadFolder {
        name: Box<str>,
        start: Start,
    },
    /// Upload the local file or folder at `path` to the current folder.
    Upload {
        path: String,
        start: Start,
    },
}
