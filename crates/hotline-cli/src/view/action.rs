use hotline_client::{Ban, ClientUserInfo, UserAccount};

use crate::file_edit::FileEdit;
use crate::session::{NewsFolder, NewsTarget};
use crate::transfer::{Conflict, Start};

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
    DisconnectUser {
        user_id: u16,
        ban: Ban,
    },
    /// Ask for this user's new name, icon, and options.
    EditOptions,
    SetClientUserInfo(ClientUserInfo),
    /// Send a message to every connected user.
    UserBroadcast(String),
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
    /// Ask which download to cancel.
    PickDownload,
    /// Cancel the download with this ID.
    KillDownload(u64),
    /// Upload the local file or folder at `path` to the current folder.
    Upload {
        path: String,
        start: Start,
    },
    /// Ask for the text of `edit` on the file `name` in the current folder.
    AskFileEdit {
        edit: FileEdit,
        name: Box<str>,
    },
    /// Apply `edit` with `text` to the file `name` in the current folder.
    EditFile {
        edit: FileEdit,
        name: Box<str>,
        text: String,
    },
    /// Delete the file or folder with this name in the current folder.
    DeleteFile(Box<str>),
    /// Show the info of the file or folder with this name in the current folder.
    GetFileInfo(Box<str>),
    /// List the root of the threaded news tree again.
    ListThreadedNews,
    /// Show the threads of the category on this row of the folder tree.
    HighlightNewsFolder(usize),
    /// Open or close the bundle on this row of the folder tree, or move to
    /// the threads of the category there.
    OpenNewsFolder(usize),
    /// Show the contents of the bundle on this row of the folder tree.
    ExpandNewsFolder(usize),
    /// Hide the contents of the bundle on this row of the folder tree, or
    /// select the bundle that holds the folder there.
    CollapseNewsFolder(usize),
    /// Show the article on this row of the thread list in the preview.
    PreviewArticle(usize),
    /// Read the article on this row of the thread list.
    OpenArticle(usize),
    /// Ask for a new article in the open category.
    NewPost,
    /// Post an article as a reply to `parent_id`, or `0` for a new thread.
    PostArticle {
        parent_id: u32,
        title: String,
        text: String,
    },
    /// Ask before this threaded news folder or article is deleted.
    AskDeleteNews(NewsTarget),
    DeleteNewsItem(NewsTarget),
    /// Create a bundle or category in the selected bundle.
    NewNewsItem {
        name: String,
        kind: NewsFolder,
    },
    /// List the accounts again.
    ListAccounts,
    /// Edit the account at this index.
    EditAccount(usize),
    /// Read the account with this login.
    OpenAccount(String),
    /// Create `account`, or replace the account `old_login`.
    SaveAccount {
        old_login: Option<Box<str>>,
        account: UserAccount,
    },
    /// Ask before the account at this index is deleted.
    AskDeleteAccount(usize),
    /// Delete the account with this login.
    DeleteAccount(Box<str>),
}

impl Action {
    /// The transfer that `conflict` stopped, run with `start`.
    pub fn retry(conflict: Conflict, start: Start) -> Self {
        match conflict {
            Conflict::LocalFileExists { name, save_as }
            | Conflict::PartialDownload { name, save_as } => Self::DownloadFile {
                name,
                save_as,
                start,
            },
            Conflict::LocalFolderExists { name } => Self::DownloadFolder { name, start },
            Conflict::PartialUpload { local } | Conflict::RemoteFolderExists { local } => {
                Self::Upload {
                    path: local.into(),
                    start,
                }
            }
        }
    }
}
