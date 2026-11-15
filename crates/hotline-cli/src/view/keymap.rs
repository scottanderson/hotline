//! The key table: each key, where it works, and what it does. The dispatch
//! and the hint line read the same table.

use std::borrow::Cow;
use std::cmp::Reverse;

use hotline_proto::Access;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rustc_hash::{FxHashMap, FxHashSet};

use super::is_modified_char;
use super::modal::ModalKind;

/// What a key does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Intent {
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    /// Select the row above in a list.
    SelectUp,
    /// Select the row below in a list.
    SelectDown,
    /// Run the selected command.
    Run,
    /// Choose the selected item of a menu.
    Choose,
    /// Invite the selected user.
    Invite,
    /// Move to the row above in a form.
    RowUp,
    /// Move to the row below in a form.
    RowDown,
    /// Move to the next row in a form, or to the first after the last.
    NextRow,
    /// Move to the previous row in a form, or to the last before the first.
    PrevRow,
    /// Turn the checkbox of the row on or off.
    Toggle,
    /// Connect to the server in the form.
    Connect,
    /// Save the server in the form as a bookmark.
    SaveBookmark,
    /// Resume a transfer.
    Resume,
    /// Replace a file or a folder.
    Replace,
    /// Disconnect a user without a ban.
    DisconnectUser,
    /// Disconnect a user, and ban the address for a while.
    TemporaryBan,
    /// Disconnect a user, and ban the address until an administrator lifts the ban.
    PermanentBan,
    /// Delete the item that the box names.
    Delete,
    /// Write a reply to an article.
    Reply,
    /// Join a private chat room.
    Join,
    /// Connect again to the last server.
    Reconnect,
    /// Open the Connect to form.
    ConnectTo,
    /// Move between the title and the text of an article.
    SwitchField,
    /// Send a message.
    Send,
    /// Upload the path in the box.
    Upload,
    /// Save the text in the box.
    Save,
    /// Set the subject of a chat room.
    SetSubject,
    /// Post an article.
    Post,
    /// Create a news bundle or category.
    Create,
    /// Open the account with the login in the box.
    OpenAccount,
    /// Start a new line in a message.
    NewLine,
    /// Open the selected file, or download it.
    OpenFile,
    /// Open the selected news folder.
    OpenFolder,
    /// Read the selected article.
    Read,
    /// Edit the selected account.
    EditAccount,
    /// Send a message to the selected user.
    Message,
    /// Close the selected news folder.
    Collapse,
    /// Open the selected news folder in the tree.
    Expand,
    /// Go to the parent folder.
    ParentFolder,
    /// Move to the news folders.
    FocusFolders,
    /// Download the selected item.
    Download,
    /// Make a folder.
    NewFolder,
    /// Make an account.
    NewAccount,
    /// Get the information of the selected item.
    Info,
    /// Rename the selected file or folder.
    Rename,
    /// Set the comment of the selected file or folder.
    Comment,
    /// Move the selected file or folder.
    MoveFile,
    /// Make an alias of the selected file or folder.
    Alias,
    /// Make a news bundle.
    NewBundle,
    /// Make a news category.
    NewCategory,
    /// Stop the selected task.
    CancelTask,
    /// Stop the login of the selected task, and disconnect.
    StopLogin,
    /// Start the selected queued transfer.
    StartTransfer,
    /// Open the selected bookmark in the Connect to form.
    EditBookmark,
    /// Turn on or off the connection to the selected bookmark at the start.
    AutoConnect,
    /// Move to the next pane.
    NextPane,
    /// Go back to the previous entry of the row under the tab bar.
    PrevSub,
    /// Go to the next entry of the row under the tab bar.
    NextSub,
    /// Go to the tab of the number of the key.
    GoToTab,
    /// Open the command palette.
    Palette,
    /// Open the actions of the focused pane.
    Actions,
    /// Go to the Tasks tab, or back from it.
    TasksTab,
    /// Open the bookmarks.
    Bookmarks,
    /// Send a broadcast.
    Broadcast,
    /// Open the options.
    Options,
    /// Invite a user to this room.
    InviteToRoom,
    /// Set the subject of this room.
    RoomSubject,
    /// Leave this room.
    LeaveRoom,
    /// Quit, or ask whether to quit.
    Interrupt,
    /// Ask whether to quit `hlc`.
    AskQuit,
    /// Show the Chat tab.
    ChatTab,
    /// Quit `hlc`.
    Quit,
    /// Close a box that only shows text.
    Close,
    /// Close a box, and do not do what it asks.
    Cancel,
    /// Refuse an invitation to a private chat room.
    Decline,
}

impl Intent {
    /// Each intent, in hint order.
    pub const ALL: &[Self] = &[
        Self::ScrollUp,
        Self::ScrollDown,
        Self::SelectUp,
        Self::SelectDown,
        Self::Run,
        Self::Choose,
        Self::Invite,
        Self::RowUp,
        Self::RowDown,
        Self::PageUp,
        Self::PageDown,
        Self::NextRow,
        Self::PrevRow,
        Self::Toggle,
        Self::OpenFile,
        Self::OpenFolder,
        Self::Read,
        Self::EditAccount,
        Self::Message,
        Self::Collapse,
        Self::Expand,
        Self::ParentFolder,
        Self::FocusFolders,
        Self::Download,
        Self::Upload,
        Self::NewFolder,
        Self::NewAccount,
        Self::OpenAccount,
        Self::Reply,
        Self::Join,
        Self::Reconnect,
        Self::ConnectTo,
        Self::SwitchField,
        Self::Send,
        Self::Post,
        Self::Save,
        Self::SetSubject,
        Self::Create,
        Self::Delete,
        Self::Info,
        Self::DisconnectUser,
        Self::TemporaryBan,
        Self::PermanentBan,
        Self::Rename,
        Self::Comment,
        Self::MoveFile,
        Self::Alias,
        Self::NewBundle,
        Self::NewCategory,
        Self::CancelTask,
        Self::StopLogin,
        Self::StartTransfer,
        Self::EditBookmark,
        Self::AutoConnect,
        Self::Connect,
        Self::SaveBookmark,
        Self::Resume,
        Self::Replace,
        Self::NewLine,
        Self::NextPane,
        Self::PrevSub,
        Self::NextSub,
        Self::GoToTab,
        Self::Palette,
        Self::Actions,
        Self::TasksTab,
        Self::Bookmarks,
        Self::Broadcast,
        Self::Options,
        Self::InviteToRoom,
        Self::RoomSubject,
        Self::LeaveRoom,
        Self::Interrupt,
        Self::AskQuit,
        Self::ChatTab,
        Self::Quit,
        Self::Close,
        Self::Cancel,
        Self::Decline,
    ];

    /// The text of the hint.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ScrollUp | Self::ScrollDown => "scroll",
            Self::PageUp | Self::PageDown => "page",
            Self::SelectUp | Self::SelectDown => "choose",
            Self::Run => "run",
            Self::Choose => "select",
            Self::Invite => "invite",
            Self::RowUp | Self::RowDown => "row",
            Self::NextRow => "next row",
            Self::PrevRow => "previous row",
            Self::Toggle => "toggle",
            Self::Connect => "connect",
            Self::SaveBookmark => "bookmark",
            Self::Resume => "resume",
            Self::Replace => "replace",
            Self::DisconnectUser => "disconnect",
            Self::TemporaryBan => "temporary ban",
            Self::PermanentBan => "permanent ban",
            Self::Delete => "delete",
            Self::Reply => "reply",
            Self::Join => "join",
            Self::Reconnect => "reconnect",
            Self::ConnectTo => "connect to",
            Self::SwitchField => "title or text",
            Self::Send => "send",
            Self::Upload => "upload",
            Self::Save => "save",
            Self::SetSubject => "set",
            Self::Post => "post",
            Self::Create => "create",
            Self::OpenAccount => "open",
            Self::NewLine => "new line",
            Self::OpenFile => "open or download",
            Self::OpenFolder => "open",
            Self::Read => "read",
            Self::EditAccount => "edit",
            Self::Message => "message",
            Self::Collapse | Self::Expand => "close/open",
            Self::ParentFolder => "up",
            Self::FocusFolders => "folders",
            Self::Download => "download",
            Self::NewFolder => "new folder",
            Self::NewAccount => "new",
            Self::Info => "info",
            Self::Rename => "rename",
            Self::Comment => "comment",
            Self::MoveFile => "move",
            Self::Alias => "alias",
            Self::NewBundle => "new bundle",
            Self::NewCategory => "new category",
            Self::CancelTask => "cancel",
            Self::StopLogin => "disconnect",
            Self::StartTransfer => "start",
            Self::EditBookmark => "edit",
            Self::AutoConnect => "auto-connect",
            Self::NextPane => "next pane",
            Self::PrevSub | Self::NextSub => "switch",
            Self::GoToTab => "tab",
            Self::Palette => "commands",
            Self::Actions => "actions",
            Self::TasksTab => "tasks",
            Self::Bookmarks => "bookmarks",
            Self::Broadcast => "broadcast",
            Self::Options => "options",
            Self::InviteToRoom => "invite",
            Self::RoomSubject => "subject",
            Self::LeaveRoom => "leave",
            Self::Interrupt => "quit",
            Self::AskQuit => "quit",
            Self::ChatTab => "chat",
            Self::Quit => "quit",
            Self::Close => "close",
            Self::Cancel => "cancel",
            Self::Decline => "decline",
        }
    }

    /// The privileges, any one of which turns the intent on. Empty for an
    /// intent that needs none.
    pub const fn enabled_by(self) -> Access {
        match self {
            Self::Reply => Access::POST_NEWS_ART,
            _ => Access::empty(),
        }
    }
}

/// Where a binding works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scope {
    Modal(ModalKind),
    Pane(Pane),
    /// A checkbox row of a form.
    Checkbox,
    /// A box that asks whether to resume a transfer that can also replace.
    Replace,
    /// A task that the user can cancel.
    Cancellable,
    /// A login that the user can stop.
    Login,
    /// A transfer that waits in `hlc`'s queue.
    Queued,
    /// Each place with no modal.
    Global,
    /// Each place, with a modal too.
    Always,
}

/// The pane that takes the keys when no modal is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Pane {
    Users,
    /// The input line of the Chat tab.
    Chat,
    /// The input line of a room tab.
    Room,
    /// The input line of the message board.
    News,
    Files,
    Accounts,
    NewsFolders,
    NewsThreads,
    NewsPreview,
    Tasks,
}

/// A key and the modifiers that it needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeyChord {
    /// `code` with no modifier.
    pub const fn plain(code: KeyCode) -> Self {
        Self::new(code, KeyModifiers::NONE)
    }

    pub const fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }

    /// `true` if `key` presses this chord. A chord with no modifier takes
    /// a character only without Ctrl and Alt, and any other key with any
    /// modifier. A chord with modifiers takes only Shift in addition.
    pub fn matches(self, key: KeyEvent) -> bool {
        key.code == self.code
            && if self.modifiers.is_empty() {
                !is_modified_char(key)
            } else {
                key.modifiers.contains(self.modifiers)
                    && key
                        .modifiers
                        .difference(self.modifiers)
                        .difference(KeyModifiers::SHIFT)
                        .is_empty()
            }
    }

    /// The chord of the next key with the same modifiers, as `F2` after
    /// `F1`.
    fn successor(self) -> Option<Self> {
        let code = match self.code {
            KeyCode::F(n) => KeyCode::F(n.checked_add(1)?),
            KeyCode::Char(c) => {
                KeyCode::Char(u32::from(c).checked_add(1).and_then(char::from_u32)?)
            }
            _ => return None,
        };
        Some(Self {
            code,
            modifiers: self.modifiers,
        })
    }

    /// The chord as a hint shows it, such as `Esc`, `↑`, `y`, `⇧Enter`,
    /// `^J`, or `Alt-T`.
    pub fn label(self) -> Cow<'static, str> {
        let key = match self.code {
            KeyCode::Char(c)
                if self
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                c.to_ascii_uppercase().to_string().into()
            }
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(c) => c.to_string().into(),
            KeyCode::Esc => "Esc".into(),
            KeyCode::Enter => "Enter".into(),
            KeyCode::Up => "↑".into(),
            KeyCode::Down => "↓".into(),
            KeyCode::PageUp => "PgUp".into(),
            KeyCode::PageDown => "PgDn".into(),
            KeyCode::Left => "←".into(),
            KeyCode::Right => "→".into(),
            KeyCode::Backspace => "⌫".into(),
            code => code.to_string().into(),
        };
        let mut label: String = [
            (KeyModifiers::ALT, "Alt-"),
            (KeyModifiers::CONTROL, "^"),
            (KeyModifiers::SHIFT, "⇧"),
        ]
        .into_iter()
        .filter(|&(modifier, _)| self.modifiers.contains(modifier))
        .map(|(_, prefix)| prefix)
        .collect();
        if label.is_empty() {
            return key;
        }
        label.push_str(&key);
        label.into()
    }
}

/// The bindings of keys to intents.
#[derive(Debug)]
pub struct Keymap {
    bindings: Vec<(Scope, KeyChord, Intent)>,
    /// The label of each chord.
    labels: FxHashMap<KeyChord, Cow<'static, str>>,
    /// The chords of each intent in each scope, in the order of `bindings`.
    chords: FxHashMap<(Scope, Intent), Vec<KeyChord>>,
    /// Each chord that each scope binds.
    bound: FxHashSet<(Scope, KeyChord)>,
}

impl Default for Keymap {
    fn default() -> Self {
        const fn key(code: KeyCode, intent: Intent) -> (KeyChord, Intent) {
            (KeyChord::plain(code), intent)
        }
        const fn chord(
            code: KeyCode,
            modifiers: KeyModifiers,
            intent: Intent,
        ) -> (KeyChord, Intent) {
            (KeyChord::new(code, modifiers), intent)
        }
        const CANCEL: (KeyChord, Intent) = key(KeyCode::Esc, Intent::Cancel);
        const CLOSE: &[(KeyChord, Intent)] = &[
            key(KeyCode::Esc, Intent::Close),
            key(KeyCode::Enter, Intent::Close),
        ];
        const DELETE: &[(KeyChord, Intent)] = &[key(KeyCode::Char('y'), Intent::Delete), CANCEL];
        const NEW_LINE: [(KeyChord, Intent); 3] = [
            chord(
                KeyCode::Enter,
                KeyModifiers::ALT.union(KeyModifiers::SHIFT),
                Intent::NewLine,
            ),
            chord(KeyCode::Enter, KeyModifiers::ALT, Intent::NewLine),
            chord(KeyCode::Enter, KeyModifiers::SHIFT, Intent::NewLine),
        ];
        const MESSAGE: &[(KeyChord, Intent)] = &[
            key(KeyCode::Enter, Intent::Send),
            NEW_LINE[0],
            NEW_LINE[1],
            NEW_LINE[2],
            CANCEL,
        ];
        const SAVE: &[(KeyChord, Intent)] = &[key(KeyCode::Enter, Intent::Save), CANCEL];
        const SELECT: [(KeyChord, Intent); 2] = [
            key(KeyCode::Up, Intent::SelectUp),
            key(KeyCode::Down, Intent::SelectDown),
        ];
        const ROWS: [(KeyChord, Intent); 2] = [
            key(KeyCode::Up, Intent::RowUp),
            key(KeyCode::Down, Intent::RowDown),
        ];
        let modals: [(ModalKind, &[(KeyChord, Intent)]); 29] = [
            (
                ModalKind::Account,
                &[
                    ROWS[0],
                    ROWS[1],
                    key(KeyCode::PageUp, Intent::PageUp),
                    key(KeyCode::PageDown, Intent::PageDown),
                    key(KeyCode::Enter, Intent::Save),
                    CANCEL,
                ],
            ),
            (
                ModalKind::Options,
                &[ROWS[0], ROWS[1], key(KeyCode::Enter, Intent::Save), CANCEL],
            ),
            (
                ModalKind::Connect,
                &[
                    ROWS[0],
                    ROWS[1],
                    key(KeyCode::Tab, Intent::NextRow),
                    key(KeyCode::BackTab, Intent::PrevRow),
                    key(KeyCode::Enter, Intent::Connect),
                    chord(KeyCode::Char('s'), KeyModifiers::ALT, Intent::SaveBookmark),
                    CANCEL,
                ],
            ),
            (
                ModalKind::Confirm,
                &[key(KeyCode::Char('r'), Intent::Resume), CANCEL],
            ),
            (
                ModalKind::Palette,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Run),
                    key(KeyCode::Esc, Intent::Close),
                ],
            ),
            (
                ModalKind::UserMenu,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Choose),
                    key(KeyCode::Esc, Intent::Close),
                ],
            ),
            (
                ModalKind::Bookmarks,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Connect),
                    key(KeyCode::Char('e'), Intent::EditBookmark),
                    key(KeyCode::Char('a'), Intent::AutoConnect),
                    key(KeyCode::Char('x'), Intent::Delete),
                    CANCEL,
                ],
            ),
            (
                ModalKind::PickUser,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Invite),
                    CANCEL,
                ],
            ),
            (ModalKind::Info, CLOSE),
            (ModalKind::FileInfo, CLOSE),
            (
                ModalKind::Quit,
                &[key(KeyCode::Char('y'), Intent::Quit), CANCEL],
            ),
            (
                ModalKind::Disconnect,
                &[
                    key(KeyCode::Char('d'), Intent::DisconnectUser),
                    key(KeyCode::Char('t'), Intent::TemporaryBan),
                    key(KeyCode::Char('p'), Intent::PermanentBan),
                    CANCEL,
                ],
            ),
            (ModalKind::DeleteFile, DELETE),
            (ModalKind::DeleteNews, DELETE),
            (ModalKind::DeleteAccount, DELETE),
            (ModalKind::DeleteBookmark, DELETE),
            (
                ModalKind::Disconnected,
                &[
                    key(KeyCode::Char('r'), Intent::Reconnect),
                    key(KeyCode::Char('c'), Intent::ConnectTo),
                    key(KeyCode::Char('q'), Intent::Quit),
                    key(KeyCode::Esc, Intent::Close),
                    key(KeyCode::Enter, Intent::Close),
                ],
            ),
            (
                ModalKind::Invite,
                &[
                    key(KeyCode::Enter, Intent::Join),
                    key(KeyCode::Esc, Intent::Decline),
                ],
            ),
            (
                ModalKind::Article,
                &[
                    key(KeyCode::Char('r'), Intent::Reply),
                    key(KeyCode::Esc, Intent::Close),
                    key(KeyCode::Enter, Intent::Close),
                ],
            ),
            (ModalKind::InstantMsg, MESSAGE),
            (ModalKind::Broadcast, MESSAGE),
            (
                ModalKind::Upload,
                &[key(KeyCode::Enter, Intent::Upload), CANCEL],
            ),
            (ModalKind::SaveAs, SAVE),
            (ModalKind::BookmarkName, SAVE),
            (ModalKind::FileEdit, SAVE),
            (
                ModalKind::Subject,
                &[key(KeyCode::Enter, Intent::SetSubject), CANCEL],
            ),
            (
                ModalKind::Post,
                &[
                    key(KeyCode::Tab, Intent::SwitchField),
                    key(KeyCode::Enter, Intent::Post),
                    NEW_LINE[0],
                    NEW_LINE[1],
                    NEW_LINE[2],
                    CANCEL,
                ],
            ),
            (
                ModalKind::NewsName,
                &[key(KeyCode::Enter, Intent::Create), CANCEL],
            ),
            (
                ModalKind::OpenAccount,
                &[key(KeyCode::Enter, Intent::OpenAccount), CANCEL],
            ),
        ];
        const CHAT: (KeyChord, Intent) = key(KeyCode::Esc, Intent::ChatTab);
        const NEXT: (KeyChord, Intent) = key(KeyCode::Tab, Intent::NextPane);
        const POST: (KeyChord, Intent) = key(KeyCode::Char('p'), Intent::Post);
        const SCROLL: [(KeyChord, Intent); 4] = [
            key(KeyCode::Up, Intent::ScrollUp),
            key(KeyCode::Down, Intent::ScrollDown),
            key(KeyCode::PageUp, Intent::PageUp),
            key(KeyCode::PageDown, Intent::PageDown),
        ];
        let panes: [(Pane, &[(KeyChord, Intent)]); 10] = [
            (
                Pane::Users,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Message),
                    key(KeyCode::Char('i'), Intent::Info),
                    key(KeyCode::Char('k'), Intent::DisconnectUser),
                    NEXT,
                    key(KeyCode::Esc, Intent::NextPane),
                ],
            ),
            (
                Pane::Chat,
                &[
                    key(KeyCode::Enter, Intent::Send),
                    NEW_LINE[0],
                    NEW_LINE[1],
                    NEW_LINE[2],
                    NEXT,
                    key(KeyCode::Esc, Intent::AskQuit),
                ],
            ),
            (
                Pane::Room,
                &[
                    key(KeyCode::Enter, Intent::Send),
                    NEW_LINE[0],
                    NEW_LINE[1],
                    NEW_LINE[2],
                    NEXT,
                    CHAT,
                ],
            ),
            (
                Pane::News,
                &[
                    SCROLL[0],
                    SCROLL[1],
                    SCROLL[2],
                    SCROLL[3],
                    key(KeyCode::Enter, Intent::Post),
                    NEW_LINE[0],
                    NEW_LINE[1],
                    NEW_LINE[2],
                    CHAT,
                ],
            ),
            (
                Pane::Files,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::OpenFile),
                    key(KeyCode::Backspace, Intent::ParentFolder),
                    key(KeyCode::Left, Intent::ParentFolder),
                    key(KeyCode::Char('d'), Intent::Download),
                    key(KeyCode::Char('u'), Intent::Upload),
                    key(KeyCode::Char('n'), Intent::NewFolder),
                    key(KeyCode::Char('x'), Intent::Delete),
                    key(KeyCode::Char('i'), Intent::Info),
                    key(KeyCode::Char('r'), Intent::Rename),
                    key(KeyCode::Char('c'), Intent::Comment),
                    key(KeyCode::Char('m'), Intent::MoveFile),
                    key(KeyCode::Char('a'), Intent::Alias),
                    NEXT,
                    CHAT,
                ],
            ),
            (
                Pane::Accounts,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::EditAccount),
                    key(KeyCode::Char('n'), Intent::NewAccount),
                    key(KeyCode::Char('o'), Intent::OpenAccount),
                    key(KeyCode::Char('x'), Intent::Delete),
                    NEXT,
                    CHAT,
                ],
            ),
            (
                Pane::NewsFolders,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::OpenFolder),
                    key(KeyCode::Left, Intent::Collapse),
                    key(KeyCode::Backspace, Intent::Collapse),
                    key(KeyCode::Right, Intent::Expand),
                    POST,
                    key(KeyCode::Char('x'), Intent::Delete),
                    key(KeyCode::Char('b'), Intent::NewBundle),
                    key(KeyCode::Char('c'), Intent::NewCategory),
                    NEXT,
                    CHAT,
                ],
            ),
            (
                Pane::NewsThreads,
                &[
                    SELECT[0],
                    SELECT[1],
                    key(KeyCode::Enter, Intent::Read),
                    key(KeyCode::Left, Intent::FocusFolders),
                    POST,
                    key(KeyCode::Char('x'), Intent::Delete),
                    NEXT,
                    CHAT,
                ],
            ),
            (
                Pane::NewsPreview,
                &[SCROLL[0], SCROLL[1], SCROLL[2], SCROLL[3], POST, NEXT, CHAT],
            ),
            (Pane::Tasks, &[SELECT[0], SELECT[1], CHAT]),
        ];
        let alt = |c, intent| chord(KeyCode::Char(c), KeyModifiers::ALT, intent);
        let ctrl = |c, intent| chord(KeyCode::Char(c), KeyModifiers::CONTROL, intent);
        let global = (1..=9)
            .map(|n| key(KeyCode::F(n), Intent::GoToTab))
            .chain(('1'..='9').map(|c| alt(c, Intent::GoToTab)))
            .chain([
                alt(',', Intent::PrevSub),
                alt('.', Intent::NextSub),
                ctrl('p', Intent::Palette),
                alt('x', Intent::Palette),
                ctrl('o', Intent::Actions),
                chord(KeyCode::F(10), KeyModifiers::SHIFT, Intent::Actions),
                key(KeyCode::Menu, Intent::Actions),
                alt('t', Intent::TasksTab),
                alt('r', Intent::Bookmarks),
                alt('a', Intent::Broadcast),
                alt('o', Intent::Options),
                alt('i', Intent::InviteToRoom),
                alt('s', Intent::RoomSubject),
                alt('w', Intent::LeaveRoom),
            ])
            .map(|(chord, intent)| (Scope::Global, chord, intent));
        let always = [(
            Scope::Always,
            KeyChord::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            Intent::Interrupt,
        )];
        let others: [(Scope, &[(KeyChord, Intent)]); 5] = [
            (Scope::Checkbox, &[key(KeyCode::Char(' '), Intent::Toggle)]),
            (Scope::Replace, &[key(KeyCode::Char('o'), Intent::Replace)]),
            (
                Scope::Cancellable,
                &[key(KeyCode::Enter, Intent::CancelTask)],
            ),
            (Scope::Login, &[key(KeyCode::Enter, Intent::StopLogin)]),
            (
                Scope::Queued,
                &[key(KeyCode::Char('s'), Intent::StartTransfer)],
            ),
        ];
        Self::new(
            modals
                .into_iter()
                .map(|(kind, keys)| (Scope::Modal(kind), keys))
                .chain(panes.map(|(pane, keys)| (Scope::Pane(pane), keys)))
                .chain(others)
                .flat_map(|(scope, keys)| {
                    keys.iter()
                        .map(move |&(chord, intent)| (scope, chord, intent))
                })
                .chain(global)
                .chain(always)
                .collect(),
        )
    }
}

impl Keymap {
    /// The key table of `bindings`, with the label of each chord and an
    /// index of the chords.
    pub(super) fn new(bindings: Vec<(Scope, KeyChord, Intent)>) -> Self {
        let labels = bindings
            .iter()
            .map(|&(_, chord, _)| (chord, chord.label()))
            .collect();
        let mut chords: FxHashMap<_, Vec<_>> = FxHashMap::default();
        for &(scope, chord, intent) in &bindings {
            chords.entry((scope, intent)).or_default().push(chord);
        }
        let bound = bindings
            .iter()
            .map(|&(scope, chord, _)| (scope, chord))
            .collect();
        Self {
            bindings,
            labels,
            chords,
            bound,
        }
    }

    /// The label of `chord`, or `""` for a chord that the table does not bind.
    pub fn label(&self, chord: KeyChord) -> &str {
        self.labels.get(&chord).map_or("", AsRef::as_ref)
    }

    /// The keys of a hint for `chords`: a range of three or more keys in a
    /// row, as in `F1-F9`, or each key with `/` between, as in `Esc/Enter`.
    pub fn hint_key(&self, chords: impl Iterator<Item = KeyChord> + Clone) -> Cow<'_, str> {
        let mut rest = chords.clone();
        let Some(first) = rest.next() else {
            return "".into();
        };
        let (count, last, in_a_row) =
            rest.fold((1_usize, first, true), |(count, prev, in_a_row), chord| {
                (
                    count.saturating_add(1),
                    chord,
                    in_a_row && prev.successor() == Some(chord),
                )
            });
        match count {
            1 => self.label(first).into(),
            3.. if in_a_row => format!("{}-{}", self.label(first), self.label(last)).into(),
            _ => chords
                .map(|chord| self.label(chord))
                .collect::<Vec<_>>()
                .join("/")
                .into(),
        }
    }

    /// The intent of `key` in the first scope of `scopes` that binds it. The
    /// chord with the most modifiers wins, as `⇧Enter` over `Enter`.
    pub fn intent(&self, scopes: &[Scope], key: KeyEvent) -> Option<Intent> {
        scopes.iter().find_map(|&scope| {
            self.bindings
                .iter()
                .filter(|&&(s, chord, _)| s == scope && chord.matches(key))
                .min_by_key(|&&(_, chord, _)| Reverse(chord.modifiers.bits().count_ones()))
                .map(|&(_, _, intent)| intent)
        })
    }

    /// The chords that reach `intent` from `scopes`. A chord that a nearer
    /// scope binds to another intent does not reach it.
    pub fn chords(
        &self,
        scopes: &[Scope],
        intent: Intent,
    ) -> impl Iterator<Item = KeyChord> + Clone {
        scopes.iter().flat_map(move |&scope| {
            self.chords_in(scope, intent)
                .filter(move |&chord| self.first_scope(scopes, chord) == Some(scope))
        })
    }

    /// The chords that bind `intent` in `scope`.
    fn chords_in(&self, scope: Scope, intent: Intent) -> impl Iterator<Item = KeyChord> + Clone {
        self.chords
            .get(&(scope, intent))
            .into_iter()
            .flatten()
            .copied()
    }

    /// The first scope of `scopes` that binds `chord`.
    fn first_scope(&self, scopes: &[Scope], chord: KeyChord) -> Option<Scope> {
        scopes
            .iter()
            .copied()
            .find(|&scope| self.bound.contains(&(scope, chord)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: Scope = Scope::Modal(ModalKind::Info);
    const QUIT: Scope = Scope::Modal(ModalKind::Quit);

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn matches_refuses_a_character_with_ctrl_or_alt() {
        let y = KeyChord::plain(KeyCode::Char('y'));
        assert!(y.matches(key(KeyCode::Char('y'), KeyModifiers::NONE)));
        assert!(!y.matches(key(KeyCode::Char('y'), KeyModifiers::CONTROL)));
        assert!(!y.matches(key(KeyCode::Char('y'), KeyModifiers::ALT)));
    }

    #[test]
    fn matches_takes_another_key_with_any_modifier() {
        let esc = KeyChord::plain(KeyCode::Esc);
        assert!(esc.matches(key(KeyCode::Esc, KeyModifiers::SHIFT)));
    }

    #[test]
    fn intent_takes_the_first_scope_that_binds_the_key() {
        let keymap = Keymap::default();
        let esc = key(KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(keymap.intent(&[QUIT, INFO], esc), Some(Intent::Cancel));
        assert_eq!(keymap.intent(&[INFO, QUIT], esc), Some(Intent::Close));
        let up = key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(keymap.intent(&[QUIT], up), None);
    }

    #[test]
    fn intent_prefers_the_chord_with_more_modifiers() {
        let keymap = Keymap::default();
        let message = [Scope::Modal(ModalKind::InstantMsg)];
        let intent = |code, modifiers| keymap.intent(&message, key(code, modifiers));
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::NONE),
            Some(Intent::Send)
        );
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::SHIFT),
            Some(Intent::NewLine)
        );
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::ALT),
            Some(Intent::NewLine)
        );
        assert_eq!(
            intent(KeyCode::Enter, KeyModifiers::ALT | KeyModifiers::SHIFT),
            Some(Intent::NewLine)
        );
        assert_eq!(intent(KeyCode::Char('j'), KeyModifiers::CONTROL), None);
    }

    #[test]
    fn label_shows_the_modifiers() {
        let label = |code, modifiers| KeyChord::new(code, modifiers).label();
        assert_eq!(label(KeyCode::Enter, KeyModifiers::SHIFT), "⇧Enter");
        assert_eq!(label(KeyCode::Char('j'), KeyModifiers::CONTROL), "^J");
        assert_eq!(label(KeyCode::Char('t'), KeyModifiers::ALT), "Alt-T");
        assert_eq!(label(KeyCode::Tab, KeyModifiers::NONE), "Tab");
    }

    #[test]
    fn chords_leave_out_a_chord_that_a_nearer_scope_takes() {
        let keymap = Keymap::default();
        assert_eq!(
            keymap
                .chords(&[QUIT, INFO], Intent::Close)
                .collect::<Vec<_>>(),
            [KeyChord::plain(KeyCode::Enter)]
        );
        assert_eq!(
            keymap.chords(&[INFO], Intent::Close).collect::<Vec<_>>(),
            [
                KeyChord::plain(KeyCode::Esc),
                KeyChord::plain(KeyCode::Enter)
            ]
        );
    }

    /// The Key column of the key table in the README.
    fn readme_keys() -> Vec<Cow<'static, str>> {
        include_str!("../../README.md")
            .lines()
            .skip_while(|line| !line.starts_with("| Key "))
            .skip(2)
            .take_while(|line| line.starts_with('|'))
            .filter_map(|line| line.split('|').nth(1))
            .map(|key| key.trim().trim_matches('`').replace("Ctrl-", "^"))
            .map(Cow::from)
            .collect()
    }

    #[test]
    fn readme_key_table_names_the_first_key_of_each_global_and_user_list_intent() {
        let keymap = Keymap::default();
        let keys = readme_keys();
        let scopes = [Scope::Global, Scope::Pane(Pane::Users)];
        let missing: Vec<_> = Intent::ALL
            .iter()
            .copied()
            .filter(|&intent| {
                !matches!(
                    intent,
                    Intent::GoToTab | Intent::SelectUp | Intent::SelectDown | Intent::NextPane
                )
            })
            .flat_map(|intent| scopes.map(|scope| keymap.chords(&[scope], intent).next()))
            .flatten()
            .map(KeyChord::label)
            .filter(|label| !keys.contains(label))
            .collect();
        assert!(missing.is_empty(), "not in the README: {missing:?}");
    }

    #[test]
    fn all_has_each_intent_of_the_default_table_once() {
        let missing: Vec<_> = Keymap::default()
            .bindings
            .iter()
            .map(|&(_, _, intent)| intent)
            .filter(|intent| !Intent::ALL.contains(intent))
            .collect();
        assert!(missing.is_empty(), "not in Intent::ALL: {missing:?}");
        let unique: FxHashSet<_> = Intent::ALL.iter().collect();
        assert_eq!(unique.len(), Intent::ALL.len());
    }

    #[test]
    fn hint_key_shows_a_range_only_for_three_or_more_keys_in_a_row() {
        let f = |n| KeyChord::plain(KeyCode::F(n));
        let keymap = Keymap::new([1, 2, 3, 5, 9].map(|n| (QUIT, f(n), Intent::Cancel)).into());
        let key = |numbers: &[u8]| keymap.hint_key(numbers.iter().map(|&n| f(n))).into_owned();
        assert_eq!(key(&[1]), "F1");
        assert_eq!(key(&[1, 2]), "F1/F2");
        assert_eq!(key(&[1, 2, 3]), "F1-F3");
        assert_eq!(key(&[1, 5, 9]), "F1/F5/F9");
        assert_eq!(key(&[1, 2, 3, 5]), "F1/F2/F3/F5");
    }
}
