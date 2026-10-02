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
    /// Download the named file in the current folder.
    DownloadFile(Box<str>),
    /// Download the named folder in the current folder.
    DownloadFolder(Box<str>),
    /// Upload the local file at this path to the current folder.
    UploadFile(String),
}
