use crate::transfer::Start;

/// What the caller must do after one key press or mouse click.
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    /// Send public chat, sent unchanged even when it starts with `/`.
    SendChat(String),
    /// Send chat to the private chat room `chat_id`.
    SendRoomChat {
        chat_id: u32,
        text: String,
    },
    /// Open a private chat room with the user.
    InviteNewChat(u16),
    /// Ask which user to invite to the room.
    PickInvitee(u32),
    InviteToChat {
        chat_id: u32,
        user_id: u16,
    },
    JoinChat(u32),
    RejectChatInvite(u32),
    LeaveChat(u32),
    /// Ask for the room's new subject.
    EditChatSubject(u32),
    SetChatSubject {
        chat_id: u32,
        subject: String,
    },
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
    /// List the current threaded news bundle or category again.
    ListThreadedNews,
    /// Open the threaded news item at this index, or read the article there.
    OpenNewsItem(usize),
    /// List the bundle that holds the current one.
    ParentNews,
    /// Ask for a new article in the current category.
    NewPost,
    /// Post an article as a reply to `parent_id`, or `0` for a new thread.
    PostArticle {
        parent_id: u32,
        title: String,
        text: String,
    },
    /// Ask before the threaded news item at this index is deleted.
    AskDeleteNews(usize),
    DeleteNewsItem(usize),
    /// Create a bundle, or a category if `category`, in the current bundle.
    NewNewsItem {
        name: String,
        category: bool,
    },
}
