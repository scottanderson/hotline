//! [`View`]: the tab, focus, selection, and modals. Pure: turns key presses
//! and mouse clicks into an [`Action`] for the caller to run.

mod account_form;
mod action;
mod command;
mod connect_form;
mod divider;
mod hint;
mod keymap;
mod modal;
mod options_form;
mod tab;
mod tasks_box;
mod text_box;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use hotline_client::Ban;
use hotline_proto::Access;
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::{ListState, TableState};

use crate::file_edit::FileEdit;
use crate::session::{NewsFolder, NewsTarget};
use crate::tasks::TaskRow;
use crate::transfer::Start;
use crate::ui::{self, Areas};

pub use account_form::{AccountForm, privileges};
pub use action::Action;
pub use command::{Command, Palette};
pub use connect_form::ConnectForm;
pub use divider::{Divider, Splits};
pub use hint::Hint;
pub use keymap::{Intent, KeyChord, Keymap, Pane, Scope};
pub use modal::{BookmarkRow, MenuItem, Modal};
pub use options_form::{OPTIONS, OptionsForm, QUEUE_TRANSFERS};
pub use tab::Tab;
pub use text_box::TextBox;

/// The longest gap between the two clicks of a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// Whether `key` is a character with Ctrl or Alt.
fn is_modified_char(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// `true` if `enabled_by` is empty, or `access` has one of its privileges.
pub const fn allows(access: Access, enabled_by: Access) -> bool {
    enabled_by.is_empty() || access.intersects(enabled_by)
}

/// The pane that takes keys when no modal is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Users,
    /// The file list on the Files tab.
    Files,
    /// The folder tree on the Threaded News tab.
    NewsFolders,
    /// The thread list on the Threaded News tab.
    NewsThreads,
    /// The article preview on the Threaded News tab.
    NewsPreview,
    /// The account list on the Admin tab.
    Accounts,
    /// The task list on the Tasks tab.
    Tasks,
}

impl Focus {
    /// The name of the pane, for a hint.
    const fn name(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Users => "users",
            Self::Files => "files",
            Self::NewsFolders => "folders",
            Self::NewsThreads => "threads",
            Self::NewsPreview => "preview",
            Self::Accounts => "accounts",
            Self::Tasks => "tasks",
        }
    }

    /// The pane of the focus, for a list pane.
    const fn pane(self) -> Pane {
        match self {
            Self::Input => Pane::Chat,
            Self::Users => Pane::Users,
            Self::Files => Pane::Files,
            Self::NewsFolders => Pane::NewsFolders,
            Self::NewsThreads => Pane::NewsThreads,
            Self::NewsPreview => Pane::NewsPreview,
            Self::Accounts => Pane::Accounts,
            Self::Tasks => Pane::Tasks,
        }
    }

    /// The focus that leaves the user list on `tab`.
    const fn main(tab: Tab) -> Self {
        match tab {
            Tab::Chat | Tab::News | Tab::Room(_) => Self::Input,
            Tab::Files => Self::Files,
            Tab::Threaded => Self::NewsFolders,
            Tab::Tasks => Self::Tasks,
            Tab::Admin => Self::Accounts,
        }
    }
}

/// The row that the first click of a double click hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clicked {
    User(u16),
    File(usize),
    NewsFolder(usize),
    News(usize),
    Account(usize),
}

pub struct View {
    pub tab: Tab,
    pub focus: Focus,
    pub modal: Option<Modal>,
    /// Each tab and its label: the tab bar in order, then the tabs in the
    /// rows under it.
    pub tabs: Vec<(Tab, String)>,
    pub chat_input: TextBox,
    pub post_input: TextBox,
    /// The input line of each room tab.
    pub room_inputs: HashMap<u32, TextBox>,
    /// Lines of news scrolled past the top.
    pub news_scroll: u16,
    /// The first user list row on screen, as the last draw left it.
    pub users_offset: usize,
    /// The selection and scroll position of the file list.
    pub files: TableState,
    /// The selection and scroll position of the threaded news folder tree.
    pub news_folders: TableState,
    /// The number of rows in the folder tree.
    pub news_folder_count: usize,
    /// The selection and scroll position of the thread list.
    pub news: TableState,
    /// The number of articles in the thread list.
    pub news_count: usize,
    /// Lines of the article preview scrolled past the top.
    pub preview_scroll: u16,
    /// The selection and scroll position of the account list.
    pub accounts: ListState,
    /// The number of accounts listed.
    pub account_count: usize,
    /// The privileges from [`crate::session::ClientSession::granted`].
    pub access: Access,
    /// `true` after the connection ends.
    pub disconnected: bool,
    /// The rows of the task list.
    pub tasks: Vec<TaskRow>,
    /// The selected row of the task list.
    task_selected: usize,
    /// The first row that the task list shows.
    pub tasks_offset: usize,
    /// The tab that Alt-T goes back to from the Tasks tab.
    tab_before_tasks: Tab,
    /// The tabs that changed since the user last showed them.
    pub updated: HashSet<Tab>,
    /// The revision of each tab when the user last showed it.
    seen: HashMap<Tab, u64>,
    selected_user: Option<u16>,
    last_click: Option<(Clicked, Instant)>,
    /// The pane sizes that the user set with a drag.
    pub splits: Splits,
    /// The divider that the user drags.
    dragging: Option<Divider>,
    /// The keys of each intent.
    pub keymap: Keymap,
}

impl Default for View {
    fn default() -> Self {
        Self {
            tab: Tab::Chat,
            focus: Focus::Input,
            modal: None,
            tabs: Tab::FIXED
                .iter()
                .map(|&(tab, label)| (tab, label.to_string()))
                .chain([(Tab::Threaded, Tab::THREADED_LABEL.to_string())])
                .collect(),
            chat_input: TextBox::new(),
            post_input: TextBox::new(),
            room_inputs: HashMap::new(),
            news_scroll: 0,
            users_offset: 0,
            files: TableState::default().with_selected(Some(0)),
            news_folders: TableState::default().with_selected(Some(0)),
            news_folder_count: 0,
            news: TableState::default().with_selected(Some(0)),
            news_count: 0,
            preview_scroll: 0,
            accounts: ListState::default().with_selected(Some(0)),
            account_count: 0,
            access: Access::empty(),
            disconnected: false,
            tasks: Vec::new(),
            task_selected: 0,
            tasks_offset: 0,
            tab_before_tasks: Tab::Chat,
            updated: HashSet::new(),
            seen: HashMap::new(),
            selected_user: None,
            last_click: None,
            splits: Splits::default(),
            dragging: None,
            keymap: Keymap::default(),
        }
    }
}

impl View {
    /// The selected user's ID, if that user is still connected.
    pub fn selected_user(&self, users: &[UserNameWithInfo]) -> Option<u16> {
        self.selected_user
            .filter(|&id| users.iter().any(|u| u.user_id == id))
    }

    /// The input line for the current tab, `None` on a tab with no input line.
    pub fn input(&self) -> Option<&TextBox> {
        static EMPTY: TextBox = TextBox::new();
        match self.tab {
            Tab::Chat => Some(&self.chat_input),
            Tab::News => Some(&self.post_input),
            Tab::Files | Tab::Threaded | Tab::Tasks | Tab::Admin => None,
            Tab::Room(id) => Some(self.room_inputs.get(&id).unwrap_or(&EMPTY)),
        }
    }

    fn input_mut(&mut self) -> Option<&mut TextBox> {
        match self.tab {
            Tab::Chat => Some(&mut self.chat_input),
            Tab::News => Some(&mut self.post_input),
            Tab::Files | Tab::Threaded | Tab::Tasks | Tab::Admin => None,
            Tab::Room(id) => Some(self.room_inputs.entry(id).or_default()),
        }
    }

    /// Replace the tab list. A room tab that is gone goes back to chat.
    pub fn set_tabs(&mut self, tabs: Vec<(Tab, String)>) {
        self.room_inputs
            .retain(|&id, _| tabs.iter().any(|&(tab, _)| tab == Tab::Room(id)));
        self.updated
            .retain(|&old| tabs.iter().any(|&(tab, _)| tab == old));
        self.seen
            .retain(|&old, _| tabs.iter().any(|&(tab, _)| tab == old));
        self.tabs = tabs;
        if !self.tabs.iter().any(|&(tab, _)| tab == self.tab) {
            let _ = self.set_tab(Tab::Chat);
        }
    }

    /// The index in `tabs` of the first tab in a row under the tab bar.
    fn first_sub(&self) -> usize {
        self.tabs
            .iter()
            .position(|&(tab, _)| tab != tab.top())
            .unwrap_or(self.tabs.len())
    }

    /// The tabs of the tab bar.
    pub fn bar_tabs(&self) -> &[(Tab, String)] {
        self.tabs.get(..self.first_sub()).unwrap_or_default()
    }

    /// The row under the shown tab of the tab bar: Public and each room
    /// under Chat, or the message board and the threaded news under News.
    /// It is empty when the bar tab has nothing under it.
    pub fn sub_row(&self) -> Vec<(Tab, String)> {
        let top = self.tab.top();
        let subs = self.tabs.get(self.first_sub()..).unwrap_or_default();
        let mut subs = subs.iter().filter(|&&(tab, _)| tab.top() == top).peekable();
        let Some(label) = top.row_label().filter(|_| subs.peek().is_some()) else {
            return Vec::new();
        };
        std::iter::once((top, label.to_string()))
            .chain(subs.cloned())
            .collect()
    }

    /// Whether the row under the tab bar shows.
    pub fn has_sub_row(&self) -> bool {
        !self.sub_row().is_empty()
    }

    /// The entry of the row under the tab bar after the shown one, or
    /// before it, with wraparound.
    fn step_sub(&self, forward: bool) -> Option<Tab> {
        let row = self.sub_row();
        let at = row.iter().position(|&(tab, _)| tab == self.tab)?;
        let next = if forward {
            row.get(at.saturating_add(1)).or(row.first())
        } else {
            at.checked_sub(1).and_then(|i| row.get(i)).or(row.last())
        };
        next.map(|&(tab, _)| tab)
    }

    /// Mark each tab whose revision changed while it was hidden. Clear the
    /// mark of the shown tab.
    pub fn note_revisions(&mut self, revisions: impl IntoIterator<Item = (Tab, u64)>) {
        for (tab, revision) in revisions {
            let seen = self.seen.entry(tab).or_insert(revision);
            if tab == self.tab {
                *seen = revision;
                self.updated.remove(&tab);
            } else if *seen != revision {
                self.updated.insert(tab);
            }
        }
    }

    /// `true` if `enabled_by` is empty, or the user has one of its
    /// privileges.
    pub const fn may(&self, enabled_by: Access) -> bool {
        allows(self.access, enabled_by)
    }

    /// Show the room tab `chat_id`.
    pub fn show_room(&mut self, chat_id: u32) {
        let _ = self.set_tab(Tab::Room(chat_id));
    }

    /// The keys of the modal or the focused pane.
    pub fn hints(&self, files: &[FileNameWithInfo]) -> Vec<Hint<'_>> {
        self.keymap_hints(files)
    }

    /// The selected item of the file list.
    fn selected_file<'a>(&self, files: &'a [FileNameWithInfo]) -> Option<&'a FileNameWithInfo> {
        files.get(selected(self.files.selected(), files.len()))
    }

    /// The pane that takes the keys when no modal is open.
    const fn pane(&self) -> Pane {
        match (self.focus, self.tab) {
            (Focus::Input, Tab::Chat) => Pane::Chat,
            (Focus::Input, Tab::Room(_)) => Pane::Room,
            (Focus::Input, Tab::News) => Pane::News,
            (Focus::Input, tab) => Focus::main(tab).pane(),
            (focus, _) => focus.pane(),
        }
    }

    /// The focus that Tab gives, `None` where Tab keeps the focus.
    fn next_focus(&self) -> Option<Focus> {
        let next = match self.focus {
            Focus::Users => Focus::main(self.tab),
            Focus::NewsFolders => Focus::NewsThreads,
            Focus::NewsThreads => Focus::NewsPreview,
            _ => self.side_focus(),
        };
        (next != self.focus).then_some(next)
    }

    /// The scopes of the keys, in the order that the dispatch reads them:
    /// the global keys first, then from the most specific.
    pub fn scopes(&self) -> Vec<Scope> {
        match &self.modal {
            Some(modal) => modal_scopes(modal),
            None => [Scope::Global]
                .into_iter()
                .chain(self.task_scopes().filter(|_| self.pane() == Pane::Tasks))
                .chain([Scope::Pane(self.pane())])
                .collect(),
        }
    }

    /// The privileges, any one of which turns `intent` on now.
    fn enabled_by(&self, intent: Intent, files: &[FileNameWithInfo]) -> Access {
        match &self.modal {
            Some(modal) => modal_enabled_by(modal, intent),
            None => self.pane_enabled_by(intent, files),
        }
    }

    /// The privileges, any one of which turns `intent` on in the focused
    /// pane.
    fn pane_enabled_by(&self, intent: Intent, files: &[FileNameWithInfo]) -> Access {
        let folder = self
            .selected_file(files)
            .is_some_and(FileNameWithInfo::is_folder);
        let edit = |edit: FileEdit| edit.enabled_by(folder);
        match (self.pane(), intent) {
            (Pane::Users, Intent::Message) => MenuItem::SendMessage.enabled_by(),
            (Pane::Users, Intent::Info) => MenuItem::GetInfo.enabled_by(),
            (Pane::Users, Intent::DisconnectUser) => MenuItem::Disconnect.enabled_by(),
            (_, Intent::Send) => Access::SEND_CHAT,
            (_, Intent::Post) => Access::POST_NEWS_ART,
            (Pane::Files, Intent::OpenFile) if !folder => Access::DOWNLOAD_FILE,
            (Pane::Files, Intent::Download) if folder => Access::DOWNLOAD_FOLDER,
            (Pane::Files, Intent::Download) => Access::DOWNLOAD_FILE,
            (Pane::Files, Intent::Upload) => Access::UPLOAD_FILE | Access::UPLOAD_FOLDER,
            (Pane::Files, Intent::NewFolder) => edit(FileEdit::NewFolder),
            (Pane::Files, Intent::Delete) if folder => Access::DELETE_FOLDER,
            (Pane::Files, Intent::Delete) => Access::DELETE_FILE,
            (Pane::Files, Intent::Rename) => edit(FileEdit::Rename),
            (Pane::Files, Intent::Comment) => edit(FileEdit::Comment),
            (Pane::Files, Intent::MoveFile) => edit(FileEdit::Move),
            (Pane::Files, Intent::Alias) => edit(FileEdit::Alias),
            (Pane::Accounts, Intent::EditAccount) => Access::OPEN_USER | Access::CREATE_USER,
            (Pane::Accounts, Intent::NewAccount) => Access::CREATE_USER,
            (Pane::Accounts, Intent::OpenAccount) => Access::OPEN_USER,
            (Pane::Accounts, Intent::Delete) => Access::DELETE_USER,
            (Pane::NewsFolders, Intent::Delete) => Access::NEWS_DELETE_CAT,
            (Pane::NewsFolders, Intent::NewBundle) => Access::NEWS_CREATE_FLDR,
            (Pane::NewsFolders, Intent::NewCategory) => Access::NEWS_CREATE_CAT,
            (Pane::NewsThreads, Intent::Delete) => Access::NEWS_DELETE_ART,
            _ => intent.enabled_by(),
        }
    }

    /// The text of the hint of `intent` now, `None` for no hint.
    fn label(&self, intent: Intent) -> Option<&'static str> {
        match intent {
            Intent::NextPane => self.next_focus().map(Focus::name),
            Intent::PrevSub | Intent::NextSub => match self.pane() {
                _ if !self.has_sub_row() => None,
                Pane::Chat | Pane::Room => Some("room"),
                Pane::News | Pane::NewsFolders => Some("news"),
                _ => None,
            },
            Intent::GoToTab => (self.pane() == Pane::Chat).then_some("tab"),
            Intent::Palette => matches!(self.pane(), Pane::Chat | Pane::Room).then_some("commands"),
            Intent::Actions => matches!(self.pane(), Pane::Users | Pane::Room).then_some("actions"),
            Intent::TasksTab => (self.pane() == Pane::Tasks).then_some("back"),
            Intent::PrevRow
            | Intent::Bookmarks
            | Intent::Broadcast
            | Intent::Options
            | Intent::InviteToRoom
            | Intent::RoomSubject
            | Intent::LeaveRoom
            | Intent::Interrupt => None,
            _ => Some(intent.label()),
        }
    }

    /// The hints of the intents that [`Self::keymap`] binds in
    /// [`Self::scopes`]. The keys of one intent with the same modifiers show as
    /// a range of three or more keys in a row, as in `F1-F9`, or with `/`
    /// between, as in `Esc/Enter`. Two hints in a row with one label and the
    /// same privileges merge, as in `↑↓ scroll`, `PgUp/PgDn page`, and
    /// `⌥,. room`.
    fn keymap_hints(&self, files: &[FileNameWithInfo]) -> Vec<Hint<'_>> {
        let scopes = self.scopes();
        Intent::ALL
            .iter()
            .copied()
            .filter_map(|intent| {
                let chords = self.keymap.chords(&scopes, intent);
                let modifiers = chords.clone().next()?.modifiers;
                let key = self
                    .keymap
                    .hint_key(chords.filter(move |chord| chord.modifiers == modifiers));
                let does = self.label(intent)?;
                Some(Hint::new(key, does).enabled_by(self.enabled_by(intent, files)))
            })
            .fold(Vec::new(), |mut hints: Vec<Hint>, hint| {
                match hints.last_mut() {
                    Some(last) if last.does == hint.does && last.enabled_by == hint.enabled_by => {
                        let tail = ["^", "⌥", "⇧"]
                            .into_iter()
                            .filter(|prefix| last.key.starts_with(prefix))
                            .find_map(|prefix| hint.key.strip_prefix(prefix))
                            .unwrap_or(&hint.key);
                        join_key(last.key.to_mut(), tail);
                    }
                    _ => hints.push(hint),
                }
                hints
            })
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        users: &[UserNameWithInfo],
        files: &[FileNameWithInfo],
    ) -> Action {
        if self.keymap.intent(&[Scope::Always], key) == Some(Intent::Interrupt) {
            if matches!(self.modal, Some(Modal::Quit | Modal::Disconnected(_))) {
                return Action::Quit;
            }
            self.modal = Some(Modal::Quit);
            return Action::None;
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key, users);
        }
        if let Some(intent) = self.keymap.intent(&[Scope::Global], key) {
            return self.global_intent(intent, key, users);
        }
        if let Some(intent) = self.keymap.intent(&self.scopes(), key) {
            return self.pane_intent(intent, users, files);
        }
        if self.focus == Focus::Input {
            self.input_mut().map(|input| input.edit(key));
        }
        Action::None
    }

    /// The commands that work now, in palette order.
    fn commands(&self, users: &[UserNameWithInfo]) -> Vec<Command> {
        let mut commands: Vec<Command> = self
            .tabs
            .iter()
            .map(|&(tab, _)| Command::OpenTab(tab))
            .collect();
        commands.extend(self.room_commands());
        if self.focus == Focus::Users {
            commands.extend(self.user_commands(users));
        }
        commands.extend([
            Command::Broadcast,
            Command::Options,
            Command::Tasks,
            if self.disconnected {
                Command::Reconnect
            } else {
                Command::Disconnect
            },
            Command::ConnectTo,
            Command::Bookmarks,
            Command::Quit,
        ]);
        commands
    }

    /// The commands of the room tab, if one is open.
    fn room_commands(&self) -> Vec<Command> {
        match self.tab {
            Tab::Room(chat_id) => vec![
                Command::Invite(chat_id),
                Command::Subject(chat_id),
                Command::Leave(chat_id),
            ],
            _ => Vec::new(),
        }
    }

    /// The commands of the user menu on the selected user, if one is selected.
    fn user_commands(&self, users: &[UserNameWithInfo]) -> Vec<Command> {
        self.selected_user(users).map_or_else(Vec::new, |user_id| {
            MenuItem::ALL
                .iter()
                .map(|&item| Command::User(user_id, item))
                .collect()
        })
    }

    /// The commands of the action menu for the item with the focus.
    fn focus_commands(&self, users: &[UserNameWithInfo]) -> Vec<Command> {
        match self.focus {
            Focus::Users => self.user_commands(users),
            Focus::Input => self.room_commands(),
            Focus::Files
            | Focus::NewsFolders
            | Focus::NewsThreads
            | Focus::NewsPreview
            | Focus::Accounts
            | Focus::Tasks => Vec::new(),
        }
    }

    /// Do the global `intent` of `key`.
    fn global_intent(
        &mut self,
        intent: Intent,
        key: KeyEvent,
        users: &[UserNameWithInfo],
    ) -> Action {
        let chat_id = self.tab.chat_id();
        let command = match intent {
            Intent::GoToTab => {
                let number = tab_number(key).unwrap_or_default();
                return match self.bar_tabs().get(number.saturating_sub(1)) {
                    Some(&(tab, _)) => self.set_tab(tab),
                    None => Action::None,
                };
            }
            Intent::PrevSub | Intent::NextSub => {
                return self
                    .step_sub(intent == Intent::NextSub)
                    .map_or(Action::None, |tab| self.set_tab(tab));
            }
            Intent::Palette => {
                let commands = self.commands(users);
                self.modal = Some(Modal::Palette(self.palette("commands", commands)));
                return Action::None;
            }
            Intent::Actions => {
                let commands = self.focus_commands(users);
                if !commands.is_empty() {
                    self.modal = Some(Modal::Palette(self.palette("actions", commands)));
                }
                return Action::None;
            }
            Intent::TasksTab => Some(Command::Tasks),
            Intent::Bookmarks => Some(Command::Bookmarks),
            Intent::Broadcast => Some(Command::Broadcast),
            Intent::Options => Some(Command::Options),
            Intent::InviteToRoom => chat_id.map(Command::Invite),
            Intent::RoomSubject => chat_id.map(Command::Subject),
            Intent::LeaveRoom => chat_id.map(Command::Leave),
            _ => None,
        };
        match command {
            Some(command) if self.commands(users).contains(&command) => self.run(command, users),
            _ => Action::None,
        }
    }

    /// A palette of `commands`, each with its key from the key table.
    fn palette(&self, title: &'static str, commands: Vec<Command>) -> Palette {
        let entries = commands
            .into_iter()
            .map(|command| (command, self.command_key(command)))
            .collect::<Vec<_>>();
        Palette::new(title, entries, &self.tabs)
    }

    /// The key that runs `command` without the palette, or `""`.
    fn command_key(&self, command: Command) -> Cow<'static, str> {
        let (scope, intent) = match command {
            Command::OpenTab(tab) => {
                let Some(number) = self.bar_tabs().iter().position(|&(t, _)| t == tab) else {
                    return "".into();
                };
                return self
                    .keymap
                    .chords(&[Scope::Global], Intent::GoToTab)
                    .find(|chord| {
                        tab_number(KeyEvent::new(chord.code, chord.modifiers))
                            == Some(number.saturating_add(1))
                    })
                    .map(KeyChord::label)
                    .unwrap_or_default();
            }
            Command::Broadcast => (Scope::Global, Intent::Broadcast),
            Command::Options => (Scope::Global, Intent::Options),
            Command::Tasks => (Scope::Global, Intent::TasksTab),
            Command::Bookmarks => (Scope::Global, Intent::Bookmarks),
            Command::Invite(_) => (Scope::Global, Intent::InviteToRoom),
            Command::Subject(_) => (Scope::Global, Intent::RoomSubject),
            Command::Leave(_) => (Scope::Global, Intent::LeaveRoom),
            Command::Quit => (Scope::Always, Intent::Interrupt),
            Command::User(_, MenuItem::SendMessage) => (Scope::Pane(Pane::Users), Intent::Message),
            Command::User(_, MenuItem::GetInfo) => (Scope::Pane(Pane::Users), Intent::Info),
            Command::User(_, MenuItem::Disconnect) => {
                (Scope::Pane(Pane::Users), Intent::DisconnectUser)
            }
            Command::Reconnect
            | Command::Disconnect
            | Command::ConnectTo
            | Command::User(_, MenuItem::PrivateChat) => return "".into(),
        };
        self.keymap
            .chords(&[scope], intent)
            .next()
            .map(KeyChord::label)
            .unwrap_or_default()
    }

    /// Run `command`, if the user may.
    fn run(&mut self, command: Command, users: &[UserNameWithInfo]) -> Action {
        if !self.may(command.enabled_by()) {
            return Action::None;
        }
        match command {
            Command::OpenTab(tab) => self.set_tab(tab),
            Command::Broadcast => {
                self.modal = Some(Modal::Broadcast {
                    input: TextBox::new(),
                });
                Action::None
            }
            Command::Options => Action::EditOptions,
            Command::Tasks if self.tab == Tab::Tasks => {
                let back = self.tab_before_tasks;
                let back = if self.tabs.iter().any(|&(tab, _)| tab == back) {
                    back
                } else {
                    Tab::Chat
                };
                self.set_tab(back)
            }
            Command::Tasks => self.set_tab(Tab::Tasks),
            Command::Invite(chat_id) => Action::PickInvitee(chat_id),
            Command::Subject(chat_id) => Action::EditChatSubject(chat_id),
            Command::Leave(chat_id) => Action::LeaveChat(chat_id),
            Command::Reconnect => Action::Reconnect,
            Command::ConnectTo => Action::OpenConnect,
            Command::Bookmarks => Action::OpenBookmarks(None),
            Command::Disconnect => Action::Disconnect,
            Command::Quit => {
                self.modal = Some(Modal::Quit);
                Action::None
            }
            Command::User(user_id, item) => self.choose(user_id, item, users),
        }
    }

    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        if let Some(intent) = self.keymap.intent(&modal_scopes(&modal), key) {
            return self.modal_intent(modal, intent, users);
        }
        if let Modal::Palette(mut palette) = modal {
            palette.edit(|input| input.edit(key));
            self.modal = Some(Modal::Palette(palette));
            return Action::None;
        }
        match &mut modal {
            Modal::Account(form) => {
                form.text_mut().map(|text| text.edit(key));
            }
            Modal::Options(form) => {
                form.text_mut().map(|text| text.edit(key));
            }
            Modal::Connect(form) => {
                form.text_mut().edit(key);
            }
            _ => {}
        }
        if let Some(input) = modal.input_mut() {
            input.edit(key);
        }
        self.modal = Some(modal);
        Action::None
    }

    /// Do `intent` in `modal`, if the user has a privilege that it needs.
    fn modal_intent(
        &mut self,
        mut modal: Modal,
        intent: Intent,
        users: &[UserNameWithInfo],
    ) -> Action {
        if !self.may(modal_enabled_by(&modal, intent)) {
            self.modal = Some(modal);
            return Action::None;
        }
        match (&mut modal, intent) {
            (Modal::BookmarkName { form, .. }, Intent::Cancel) => {
                self.modal = Some(Modal::Connect(form.clone()));
                return Action::None;
            }
            (Modal::Connect(form), Intent::Cancel) if form.edits.is_some() => {
                return Action::OpenBookmarks(form.edits.take());
            }
            (Modal::DeleteBookmark { rows, selected }, Intent::Cancel) => {
                self.modal = Some(Modal::Bookmarks {
                    rows: std::mem::take(rows),
                    selected: *selected,
                });
                return Action::None;
            }
            (_, Intent::Close | Intent::Cancel) => return Action::None,
            (_, Intent::Quit) => return Action::Quit,
            (Modal::Confirm(conflict), Intent::Resume) => {
                return Action::retry(conflict.clone(), Start::Resume);
            }
            (Modal::Confirm(conflict), Intent::Replace) => {
                return Action::retry(conflict.clone(), Start::Replace);
            }
            (Modal::Account(form), Intent::RowUp) => form.row = form.row.saturating_sub(1),
            (Modal::Account(form), Intent::RowDown) => {
                form.row = form
                    .row
                    .saturating_add(1)
                    .min(AccountForm::rows().saturating_sub(1));
            }
            (Modal::Account(form), Intent::PageUp) => form.row = form.row.saturating_sub(10),
            (Modal::Account(form), Intent::PageDown) => {
                form.row = form
                    .row
                    .saturating_add(10)
                    .min(AccountForm::rows().saturating_sub(1));
            }
            (Modal::Account(form), Intent::Toggle) => form.toggle(),
            (Modal::Account(form), Intent::Save) if !form.login.is_empty() => {
                return Action::SaveAccount {
                    old_login: form.old_login.clone(),
                    account: form.account(),
                };
            }
            (Modal::Options(form), Intent::RowUp) => form.row = form.row.saturating_sub(1),
            (Modal::Options(form), Intent::RowDown) => {
                form.row = form
                    .row
                    .saturating_add(1)
                    .min(OptionsForm::rows().saturating_sub(1));
            }
            (Modal::Options(form), Intent::Toggle) => form.toggle(),
            (Modal::Options(form), Intent::Save) => {
                if let Some(info) = form.info() {
                    return Action::SetOptions {
                        info,
                        queue_transfers: form.queue_transfers,
                    };
                }
            }
            (Modal::Connect(form), Intent::RowUp) => form.row = form.row.saturating_sub(1),
            (Modal::Connect(form), Intent::RowDown) => {
                form.row = form.row.saturating_add(1).min(ConnectForm::ROWS - 1);
            }
            (Modal::Connect(form), Intent::NextRow) => form.next_row(),
            (Modal::Connect(form), Intent::PrevRow) => form.previous_row(),
            (Modal::Connect(form), Intent::Connect) => {
                if let Some((host, port)) = form.server() {
                    let _ = self.set_tab(Tab::Chat);
                    return Action::Connect {
                        host,
                        port,
                        login: form.login.take(),
                        password: form.password.take(),
                    };
                }
            }
            (Modal::Connect(form), Intent::SaveBookmark) if form.server().is_some() => {
                let input = (*form.name).into();
                self.modal = Some(Modal::BookmarkName {
                    form: form.clone(),
                    input,
                });
                return Action::None;
            }
            (Modal::BookmarkName { form, input }, Intent::Save) => {
                form.name = input.take().into();
                let action = form.bookmark().map_or(Action::None, |bookmark| {
                    let old_name = form.edits.replace(bookmark.name().into());
                    Action::SaveBookmark { old_name, bookmark }
                });
                self.modal = Some(Modal::Connect(form.clone()));
                return action;
            }
            (Modal::Palette(palette), Intent::SelectUp) => palette.select_previous(),
            (Modal::Palette(palette), Intent::SelectDown) => palette.select_next(),
            (Modal::Palette(palette), Intent::Run) => match palette.chosen() {
                Some(command) if self.may(command.enabled_by()) => return self.run(command, users),
                _ => {}
            },
            (
                Modal::UserMenu { selected, .. }
                | Modal::PickUser { selected, .. }
                | Modal::Bookmarks { selected, .. },
                Intent::SelectUp,
            ) => *selected = selected.saturating_sub(1),
            (Modal::Bookmarks { selected, rows }, Intent::SelectDown) => {
                *selected = selected.saturating_add(1).min(rows.len().saturating_sub(1));
            }
            (Modal::Bookmarks { selected, rows }, intent) if *selected < rows.len() => {
                let index = *selected;
                match intent {
                    Intent::Connect => {
                        let _ = self.set_tab(Tab::Chat);
                        return Action::ConnectBookmark(index);
                    }
                    Intent::EditBookmark => return Action::EditBookmark(index),
                    Intent::AutoConnect => return Action::ToggleAutoConnect(index),
                    Intent::Delete => {
                        self.modal = Some(Modal::DeleteBookmark {
                            rows: std::mem::take(rows),
                            selected: index,
                        });
                        return Action::None;
                    }
                    _ => {}
                }
            }
            (Modal::DeleteBookmark { selected, .. }, Intent::Delete) => {
                return Action::DeleteBookmark(*selected);
            }
            (
                Modal::UserMenu {
                    selected, items, ..
                },
                Intent::SelectDown,
            ) => {
                *selected = selected
                    .saturating_add(1)
                    .min(items.len().saturating_sub(1));
            }
            (
                Modal::PickUser {
                    selected, users, ..
                },
                Intent::SelectDown,
            ) => {
                *selected = selected
                    .saturating_add(1)
                    .min(users.len().saturating_sub(1));
            }
            (
                Modal::UserMenu {
                    user_id,
                    selected,
                    items,
                    ..
                },
                Intent::Choose,
            ) if let Some(&item) = items.get(*selected)
                && self.may(item.enabled_by()) =>
            {
                return self.choose(*user_id, item, users);
            }
            (
                Modal::PickUser {
                    chat_id,
                    users,
                    selected,
                },
                Intent::Invite,
            ) => {
                return users.get(*selected).map_or(Action::None, |&(user_id, _)| {
                    Action::InviteToChat {
                        chat_id: *chat_id,
                        user_id,
                    }
                });
            }
            (Modal::Disconnect { user_id, .. }, Intent::DisconnectUser) => {
                return Action::DisconnectUser {
                    user_id: *user_id,
                    ban: Ban::None,
                };
            }
            (Modal::Disconnect { user_id, .. }, Intent::TemporaryBan) => {
                return Action::DisconnectUser {
                    user_id: *user_id,
                    ban: Ban::Temporary,
                };
            }
            (Modal::Disconnect { user_id, .. }, Intent::PermanentBan) => {
                return Action::DisconnectUser {
                    user_id: *user_id,
                    ban: Ban::Permanent,
                };
            }
            (Modal::DeleteFile { name }, Intent::Delete) => {
                return Action::DeleteFile(std::mem::take(name));
            }
            (Modal::DeleteNews { target, .. }, Intent::Delete) => {
                return Action::DeleteNewsItem(*target);
            }
            (Modal::DeleteAccount { login }, Intent::Delete) => {
                return Action::DeleteAccount(std::mem::take(login));
            }
            (Modal::Article(article), Intent::Reply) => {
                let title = match article.title.strip_prefix("Re: ") {
                    Some(_) => article.title.to_string(),
                    None => format!("Re: {}", article.title),
                };
                self.modal = Some(Modal::Post {
                    parent_id: article.id,
                    title: title.into(),
                    body: TextBox::new(),
                    on_body: true,
                });
                return Action::None;
            }
            (Modal::Invite(invite), Intent::Join) => return Action::JoinChat(invite.chat_id),
            (Modal::Invite(invite), Intent::Decline) => {
                return Action::RejectChatInvite(invite.chat_id);
            }
            (Modal::Disconnected(_), Intent::Reconnect) => return Action::Reconnect,
            (Modal::Disconnected(_), Intent::ConnectTo) => return Action::OpenConnect,
            (Modal::InstantMsg { user_id, input, .. }, Intent::Send) if !input.is_empty() => {
                return Action::SendInstantMsg {
                    user_id: *user_id,
                    text: input.take(),
                };
            }
            (Modal::Broadcast { input }, Intent::Send) if !input.is_empty() => {
                return Action::UserBroadcast(input.take());
            }
            (Modal::InstantMsg { input, .. } | Modal::Broadcast { input }, Intent::NewLine) => {
                input.insert('\n');
            }
            (Modal::Upload { input }, Intent::Upload) if !input.is_empty() => {
                return Action::Upload {
                    path: input.take(),
                    start: Start::Ask,
                };
            }
            (Modal::SaveAs { name, input }, Intent::Save) if !input.is_empty() => {
                return Action::DownloadFile {
                    name: std::mem::take(name),
                    save_as: input.take().into(),
                    start: Start::Ask,
                };
            }
            (Modal::FileEdit { edit, name, input }, Intent::Save) if !input.is_empty() => {
                return Action::EditFile {
                    edit: *edit,
                    name: std::mem::take(name),
                    text: input.take(),
                };
            }
            (Modal::Subject { chat_id, input }, Intent::SetSubject) => {
                return Action::SetChatSubject {
                    chat_id: *chat_id,
                    subject: input.take(),
                };
            }
            (Modal::Post { on_body, .. }, Intent::SwitchField) => *on_body = !*on_body,
            (
                Modal::Post {
                    parent_id,
                    title,
                    body,
                    ..
                },
                Intent::Post,
            ) if !title.is_empty() => {
                return Action::PostArticle {
                    parent_id: *parent_id,
                    title: title.take(),
                    text: body.take(),
                };
            }
            (
                Modal::Post {
                    body,
                    on_body: true,
                    ..
                },
                Intent::NewLine,
            ) => body.insert('\n'),
            (Modal::NewsName { kind, input }, Intent::Create) if !input.is_empty() => {
                return Action::NewNewsItem {
                    name: input.take(),
                    kind: *kind,
                };
            }
            (Modal::OpenAccount { input }, Intent::OpenAccount) if !input.is_empty() => {
                return Action::OpenAccount(input.take());
            }
            _ => {}
        }
        self.modal = Some(modal);
        Action::None
    }

    /// Do `intent` in the focused pane, if the user has a privilege that it
    /// needs.
    fn pane_intent(
        &mut self,
        intent: Intent,
        users: &[UserNameWithInfo],
        files: &[FileNameWithInfo],
    ) -> Action {
        if !self.may(self.pane_enabled_by(intent, files)) {
            return Action::None;
        }
        let pane = self.pane();
        match (pane, intent) {
            (_, Intent::NextPane) => {
                if let Some(focus) = self.next_focus() {
                    self.focus = focus;
                }
            }
            (_, Intent::ChatTab) => return self.set_tab(Tab::Chat),
            (_, Intent::AskQuit) => self.modal = Some(Modal::Quit),
            (Pane::Users, Intent::SelectUp | Intent::SelectDown) => {
                self.move_selection(users, intent == Intent::SelectDown);
            }
            (Pane::Users, Intent::Message | Intent::Info | Intent::DisconnectUser) => {
                let item = match intent {
                    Intent::Message => MenuItem::SendMessage,
                    Intent::Info => MenuItem::GetInfo,
                    _ => MenuItem::Disconnect,
                };
                if let Some(user_id) = self.selected_user(users) {
                    return self.choose(user_id, item, users);
                }
            }
            (Pane::Chat | Pane::Room | Pane::News, _) => return self.input_intent(intent),
            (Pane::Files, _) => return self.files_intent(intent, files),
            (Pane::Accounts, _) => return self.accounts_intent(intent),
            (Pane::NewsFolders, _) => return self.news_folders_intent(intent),
            (Pane::NewsThreads, _) => return self.news_threads_intent(intent),
            (Pane::NewsPreview, Intent::ScrollUp) => self.scroll_preview(-1),
            (Pane::NewsPreview, Intent::ScrollDown) => self.scroll_preview(1),
            (Pane::NewsPreview, Intent::PageUp) => self.scroll_preview(-10),
            (Pane::NewsPreview, Intent::PageDown) => self.scroll_preview(10),
            (Pane::NewsPreview, Intent::Post) => return Action::NewPost,
            (Pane::Tasks, _) => return self.tasks_intent(intent),
            _ => {}
        }
        Action::None
    }

    /// Do `intent` in the input line of the tab.
    fn input_intent(&mut self, intent: Intent) -> Action {
        let tab = self.tab;
        if let (
            Tab::News,
            Intent::ScrollUp | Intent::ScrollDown | Intent::PageUp | Intent::PageDown,
        ) = (tab, intent)
        {
            self.scroll_news(match intent {
                Intent::ScrollUp => -1,
                Intent::ScrollDown => 1,
                Intent::PageUp => -10,
                _ => 10,
            });
            return Action::None;
        }
        let Some(input) = self.input_mut() else {
            return Action::None;
        };
        match (intent, tab) {
            (Intent::NewLine, _) => input.insert('\n'),
            (Intent::Send, Tab::Chat) if !input.is_empty() => {
                return Action::SendChat(input.take());
            }
            (Intent::Send, Tab::Room(chat_id)) if !input.is_empty() => {
                return Action::SendRoomChat {
                    chat_id,
                    text: input.take(),
                };
            }
            (Intent::Post, Tab::News) if !input.is_empty() => {
                return Action::OldPostNews(input.take());
            }
            _ => {}
        }
        Action::None
    }

    /// Do `intent` in the file list.
    fn files_intent(&mut self, intent: Intent, files: &[FileNameWithInfo]) -> Action {
        let index = selected(self.files.selected(), files.len());
        let edit = match intent {
            Intent::SelectUp => {
                self.files.select_previous();
                return Action::None;
            }
            Intent::SelectDown => {
                self.files.select_next();
                return Action::None;
            }
            Intent::OpenFile => return self.open_file(index, files),
            Intent::ParentFolder => return Action::ParentFolder,
            Intent::Download => return self.download(index, files),
            Intent::Upload => {
                self.modal = Some(Modal::Upload {
                    input: TextBox::new(),
                });
                return Action::None;
            }
            Intent::NewFolder => {
                return Action::AskFileEdit {
                    edit: FileEdit::NewFolder,
                    name: Box::default(),
                };
            }
            Intent::Rename => FileEdit::Rename,
            Intent::Comment => FileEdit::Comment,
            Intent::MoveFile => FileEdit::Move,
            Intent::Alias => FileEdit::Alias,
            Intent::Delete | Intent::Info => {
                let Some(file) = self.selected_file(files) else {
                    return Action::None;
                };
                let name = file.name.clone();
                if intent == Intent::Info {
                    return Action::GetFileInfo(name);
                }
                self.modal = Some(Modal::DeleteFile { name });
                return Action::None;
            }
            _ => return Action::None,
        };
        self.selected_file(files)
            .map_or(Action::None, |file| Action::AskFileEdit {
                edit,
                name: file.name.clone(),
            })
    }

    /// Do `intent` in the account list.
    fn accounts_intent(&mut self, intent: Intent) -> Action {
        let count = self.account_count;
        let index = selected(self.accounts.selected(), count);
        match intent {
            Intent::SelectUp => self.accounts.select_previous(),
            Intent::SelectDown => self.accounts.select_next(),
            Intent::EditAccount if count > 0 => return Action::EditAccount(index),
            Intent::NewAccount => self.modal = Some(Modal::Account(AccountForm::blank())),
            Intent::OpenAccount => {
                self.modal = Some(Modal::OpenAccount {
                    input: TextBox::new(),
                });
            }
            Intent::Delete if count > 0 => return Action::AskDeleteAccount(index),
            _ => {}
        }
        Action::None
    }

    /// Do `intent` in the news folder tree.
    fn news_folders_intent(&mut self, intent: Intent) -> Action {
        let count = self.news_folder_count;
        let index = selected(self.news_folders.selected(), count);
        if count == 0
            && !matches!(
                intent,
                Intent::Post | Intent::NewBundle | Intent::NewCategory
            )
        {
            return Action::None;
        }
        match intent {
            Intent::SelectUp | Intent::SelectDown => {
                let index = step(index, count, intent == Intent::SelectDown);
                self.news_folders.select(Some(index));
                Action::HighlightNewsFolder(index)
            }
            Intent::OpenFolder => Action::OpenNewsFolder(index),
            Intent::Expand => Action::ExpandNewsFolder(index),
            Intent::Collapse => Action::CollapseNewsFolder(index),
            Intent::Post => Action::NewPost,
            Intent::Delete => Action::AskDeleteNews(NewsTarget::Folder(index)),
            Intent::NewBundle | Intent::NewCategory => {
                self.modal = Some(Modal::NewsName {
                    kind: if intent == Intent::NewCategory {
                        NewsFolder::Category
                    } else {
                        NewsFolder::Bundle
                    },
                    input: TextBox::new(),
                });
                Action::None
            }
            _ => Action::None,
        }
    }

    /// Do `intent` in the thread list.
    fn news_threads_intent(&mut self, intent: Intent) -> Action {
        let count = self.news_count;
        let index = selected(self.news.selected(), count);
        match intent {
            Intent::FocusFolders => self.focus = Focus::NewsFolders,
            Intent::Post => return Action::NewPost,
            _ if count == 0 => {}
            Intent::SelectUp | Intent::SelectDown => {
                let index = step(index, count, intent == Intent::SelectDown);
                self.news.select(Some(index));
                return Action::PreviewArticle(index);
            }
            Intent::Read => return Action::OpenArticle(index),
            Intent::Delete => return Action::AskDeleteNews(NewsTarget::Article(index)),
            _ => {}
        }
        Action::None
    }

    pub fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        areas: &Areas,
        users: &[UserNameWithInfo],
        files: &[FileNameWithInfo],
        now: Instant,
    ) -> Action {
        let (x, y) = (mouse.column, mouse.row);
        if let Some(modal) = &self.modal {
            let MouseEventKind::Down(button) = mouse.kind else {
                return Action::None;
            };
            match modal {
                Modal::UserMenu {
                    user_id, at, items, ..
                } => {
                    let user_id = *user_id;
                    let menu = ui::menu_rect(*at, items, areas.screen);
                    let item = ui::menu_item_at(menu, items, x, y);
                    self.modal = None;
                    if let (MouseButton::Left, Some(item)) = (button, item) {
                        return self.choose(user_id, item, users);
                    }
                }
                Modal::Info(_) | Modal::FileInfo(_) | Modal::Article(_) => self.modal = None,
                Modal::InstantMsg { .. }
                | Modal::Disconnect { .. }
                | Modal::FileEdit { .. }
                | Modal::DeleteFile { .. }
                | Modal::Broadcast { .. }
                | Modal::Upload { .. }
                | Modal::SaveAs { .. }
                | Modal::Confirm(_)
                | Modal::Invite(_)
                | Modal::Subject { .. }
                | Modal::PickUser { .. }
                | Modal::Bookmarks { .. }
                | Modal::DeleteBookmark { .. }
                | Modal::Palette(_)
                | Modal::Post { .. }
                | Modal::NewsName { .. }
                | Modal::DeleteNews { .. }
                | Modal::Quit
                | Modal::Disconnected(_)
                | Modal::Connect(_)
                | Modal::BookmarkName { .. }
                | Modal::Account(_)
                | Modal::Options(_)
                | Modal::OpenAccount { .. }
                | Modal::DeleteAccount { .. } => {}
            }
            return Action::None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(divider) = self.divider_at(areas, x, y) {
                    self.dragging = Some(divider);
                    return Action::None;
                }
                if let Some(tab) = ui::tab_at(areas.tabs, x, y, self.bar_tabs()) {
                    return self.set_tab(tab);
                }
                if let Some(tab) = ui::tab_at(areas.sub_row, x, y, &self.sub_row()) {
                    return self.set_tab(tab);
                }
                if let Some(user_id) = self.user_at(areas.users, x, y, users) {
                    return self.click_user(user_id, users, now);
                }
                if let Some(index) = self.task_at(areas.body, x, y) {
                    self.task_selected = index;
                    self.focus = Focus::Tasks;
                    return Action::None;
                }
                if let Some(index) = self.file_at(areas.body, x, y, files) {
                    return self.click_file(index, files, now);
                }
                if self.tab == Tab::Threaded {
                    return self.click_threaded_news(areas.body, x, y, now);
                }
                if let Some(index) = self.account_at(areas.body, x, y) {
                    return self.click_account(index, now);
                }
                if contains(areas.input, x, y) {
                    self.focus = Focus::Input;
                }
            }
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(user_id) = self.user_at(areas.users, x, y, users) {
                    self.selected_user = Some(user_id);
                    self.focus = Focus::Users;
                    self.modal = Some(Modal::UserMenu {
                        user_id,
                        at: (x, y),
                        items: MenuItem::ALL,
                        selected: 0,
                    });
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(divider) = self.dragging {
                    self.drag(divider, areas, x, y);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => self.dragging = None,
            MouseEventKind::ScrollUp if self.tab == Tab::News => self.scroll_news(-3),
            MouseEventKind::ScrollDown if self.tab == Tab::News => self.scroll_news(3),
            MouseEventKind::ScrollUp if self.in_preview(areas.body, x, y) => {
                self.scroll_preview(-3)
            }
            MouseEventKind::ScrollDown if self.in_preview(areas.body, x, y) => {
                self.scroll_preview(3)
            }
            _ => {}
        }
        Action::None
    }

    /// Select `user_id`, or open a message to it on the second click of a double click.
    fn click_user(&mut self, user_id: u16, users: &[UserNameWithInfo], now: Instant) -> Action {
        self.selected_user = Some(user_id);
        self.focus = Focus::Users;
        if self.double_click(Clicked::User(user_id), now) {
            return self.choose(user_id, MenuItem::SendMessage, users);
        }
        Action::None
    }

    /// Open the folder at `index`, or download the file there, if the user
    /// may.
    fn open_file(&self, index: usize, files: &[FileNameWithInfo]) -> Action {
        match files.get(index) {
            Some(file) if file.is_folder() => Action::OpenFolder(file.name.clone()),
            Some(file) if self.may(Access::DOWNLOAD_FILE) => download_file(file),
            _ => Action::None,
        }
    }

    /// Download the file or folder at `index`, if the user may.
    fn download(&self, index: usize, files: &[FileNameWithInfo]) -> Action {
        match files.get(index) {
            Some(file) if file.is_folder() && self.may(Access::DOWNLOAD_FOLDER) => {
                Action::DownloadFolder {
                    name: file.name.clone(),
                    start: Start::Ask,
                }
            }
            Some(file) if !file.is_folder() && self.may(Access::DOWNLOAD_FILE) => {
                download_file(file)
            }
            _ => Action::None,
        }
    }

    /// Select the file at `index`, or open it on the second click of a double click.
    fn click_file(&mut self, index: usize, files: &[FileNameWithInfo], now: Instant) -> Action {
        self.files.select(Some(index));
        self.focus = Focus::Files;
        if self.double_click(Clicked::File(index), now) {
            return self.open_file(index, files);
        }
        Action::None
    }

    /// Select the folder or the article at `x`, `y` of the Threaded News
    /// tab, or open it on the second click of a double click. A click on
    /// the preview focuses it.
    fn click_threaded_news(&mut self, body: Rect, x: u16, y: u16, now: Instant) -> Action {
        let [folders, threads, preview] = ui::news_areas(body, self.splits);
        if let Some(index) = self.news_folder_at(folders, x, y) {
            self.news_folders.select(Some(index));
            self.focus = Focus::NewsFolders;
            if self.double_click(Clicked::NewsFolder(index), now) {
                return Action::OpenNewsFolder(index);
            }
            return Action::HighlightNewsFolder(index);
        }
        if let Some(index) = self.news_at(threads, x, y) {
            self.news.select(Some(index));
            self.focus = Focus::NewsThreads;
            if self.double_click(Clicked::News(index), now) {
                return Action::OpenArticle(index);
            }
            return Action::PreviewArticle(index);
        }
        if contains(preview, x, y) {
            self.focus = Focus::NewsPreview;
        }
        Action::None
    }

    /// Select the account at `index`, or edit it on the second click of a
    /// double click.
    fn click_account(&mut self, index: usize, now: Instant) -> Action {
        self.accounts.select(Some(index));
        self.focus = Focus::Accounts;
        if self.double_click(Clicked::Account(index), now) {
            return Action::EditAccount(index);
        }
        Action::None
    }

    /// `true` if `clicked` is the second click of a double click.
    fn double_click(&mut self, clicked: Clicked, now: Instant) -> bool {
        let double = matches!(
            self.last_click,
            Some((last, at)) if last == clicked && now.duration_since(at) <= DOUBLE_CLICK
        );
        self.last_click = (!double).then_some((clicked, now));
        double
    }

    /// Run the menu `item` on the user `user_id`, if the user may.
    fn choose(&mut self, user_id: u16, item: MenuItem, users: &[UserNameWithInfo]) -> Action {
        if !self.may(item.enabled_by()) {
            return Action::None;
        }
        match item {
            MenuItem::SendMessage => {
                if let Some(user) = users.iter().find(|u| u.user_id == user_id) {
                    self.modal = Some(Modal::InstantMsg {
                        user_id,
                        name: user.name.clone(),
                        input: TextBox::new(),
                    });
                }
                Action::None
            }
            MenuItem::GetInfo => Action::GetClientInfoText(user_id),
            MenuItem::PrivateChat => Action::InviteNewChat(user_id),
            MenuItem::Disconnect => {
                if let Some(user) = users.iter().find(|u| u.user_id == user_id) {
                    self.modal = Some(Modal::Disconnect {
                        user_id,
                        name: user.name.clone(),
                    });
                }
                Action::None
            }
        }
    }

    fn set_tab(&mut self, tab: Tab) -> Action {
        if self.tab == tab {
            return Action::None;
        }
        if tab == Tab::Tasks {
            self.tab_before_tasks = self.tab;
        }
        self.tab = tab;
        if self.focus != Focus::Users || !self.shows_users() {
            self.focus = Focus::main(tab);
        }
        if !self.may(tab.enabled_by()) {
            return Action::None;
        }
        match tab {
            Tab::Chat | Tab::Room(_) | Tab::Tasks => Action::None,
            Tab::News => {
                self.news_scroll = 0;
                Action::GetMsgs
            }
            Tab::Files => Action::ListFiles,
            Tab::Threaded => Action::ListThreadedNews,
            Tab::Admin => Action::ListAccounts,
        }
    }

    fn move_selection(&mut self, users: &[UserNameWithInfo], down: bool) {
        let Some(last) = users.len().checked_sub(1) else {
            return;
        };
        let current = self
            .selected_user
            .and_then(|id| users.iter().position(|u| u.user_id == id));
        let next = match (current, down) {
            (None, true) => 0,
            (None, false) => last,
            (Some(i), true) => i.saturating_add(1).min(last),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.selected_user = users.get(next).map(|u| u.user_id);
    }

    /// `true` when `x`, `y` is in the article preview of the Threaded News tab.
    fn in_preview(&self, body: Rect, x: u16, y: u16) -> bool {
        self.tab == Tab::Threaded && contains(ui::news_areas(body, self.splits)[2], x, y)
    }

    const fn scroll_preview(&mut self, lines: i16) {
        self.preview_scroll = self.preview_scroll.saturating_add_signed(lines);
    }

    const fn scroll_news(&mut self, lines: i16) {
        self.news_scroll = self.news_scroll.saturating_add_signed(lines);
    }

    /// The user on the list row at `x`, `y`.
    fn user_at(&self, list: Rect, x: u16, y: u16, users: &[UserNameWithInfo]) -> Option<u16> {
        let inner = ui::inner(list);
        if !contains(inner, x, y) {
            return None;
        }
        row_at(self.users_offset, y.saturating_sub(inner.y), users.len())
            .and_then(|index| users.get(index))
            .map(|u| u.user_id)
    }

    /// The index of the file on the list row at `x`, `y` of the Files tab.
    fn file_at(&self, body: Rect, x: u16, y: u16, files: &[FileNameWithInfo]) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Files || !contains(inner, x, y) {
            return None;
        }
        row_at(self.files.offset(), y.saturating_sub(inner.y), files.len())
    }

    /// The index of the account on the list row at `x`, `y` of the Admin tab.
    fn account_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Admin || !contains(inner, x, y) {
            return None;
        }
        row_at(
            self.accounts.offset(),
            y.saturating_sub(inner.y),
            self.account_count,
        )
    }

    /// The row of the folder tree at `x`, `y`.
    fn news_folder_at(&self, folders: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(folders);
        if !contains(inner, x, y) {
            return None;
        }
        row_at(
            self.news_folders.offset(),
            y.saturating_sub(inner.y),
            self.news_folder_count,
        )
    }

    /// The row of the thread list at `x`, `y`, below the header row.
    fn news_at(&self, threads: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(threads);
        if !contains(inner, x, y) || y == inner.y {
            return None;
        }
        let row = y.saturating_sub(inner.y).saturating_sub(1);
        row_at(self.news.offset(), row, self.news_count)
    }

    /// `true` on a chat tab: Chat or a room.
    pub const fn shows_users(&self) -> bool {
        matches!(self.tab, Tab::Chat | Tab::Room(_))
    }

    /// The focus that Tab gives from the main pane: the user list, else the
    /// main pane.
    const fn side_focus(&self) -> Focus {
        if self.shows_users() {
            Focus::Users
        } else {
            Focus::main(self.tab)
        }
    }
}

/// The selected row of a list of `len` rows, kept in range.
/// The privileges, any one of which turns `intent` on in `modal`.
const fn modal_enabled_by(modal: &Modal, intent: Intent) -> Access {
    match (modal, intent) {
        (Modal::Account(form), Intent::Save) => form.saved_by(),
        _ => intent.enabled_by(),
    }
}

/// The scopes of the keys in `modal`, from the most specific.
fn modal_scopes(modal: &Modal) -> Vec<Scope> {
    let extra = match modal {
        Modal::Account(form) if form.row >= account_form::TEXT_ROWS => Some(Scope::Checkbox),
        Modal::Options(form) if form.row >= options_form::TEXT_ROWS => Some(Scope::Checkbox),
        Modal::Confirm(conflict) if conflict.can_replace() => Some(Scope::Replace),
        _ => None,
    };
    extra
        .into_iter()
        .chain([Scope::Modal(modal.kind())])
        .collect()
}

fn selected(selected: Option<usize>, len: usize) -> usize {
    selected.unwrap_or(0).min(len.saturating_sub(1))
}

/// The row after `index` in a list of `len` rows, or the row before it if
/// not `down`, kept in range.
const fn step(index: usize, len: usize, down: bool) -> usize {
    if down {
        let next = index.saturating_add(1);
        if next < len { next } else { index }
    } else {
        index.saturating_sub(1)
    }
}

/// The index of the row on screen line `row` of a list of `len` rows
/// that starts at `offset`.
fn row_at(offset: usize, row: u16, len: usize) -> Option<usize> {
    offset
        .checked_add(usize::from(row))
        .filter(|&index| index < len)
}

/// Download `file` under its own name.
fn download_file(file: &FileNameWithInfo) -> Action {
    Action::DownloadFile {
        name: file.name.clone(),
        save_as: file.name.clone(),
        start: Start::Ask,
    }
}

pub fn contains(rect: Rect, x: u16, y: u16) -> bool {
    (rect.left()..rect.right()).contains(&x) && (rect.top()..rect.bottom()).contains(&y)
}

/// The tab number of F1 to F9, or of Alt with 1 to 9.
fn tab_number(key: KeyEvent) -> Option<usize> {
    match key.code {
        KeyCode::F(n @ 1..=9) => Some(usize::from(n)),
        KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::ALT) => {
            c.to_digit(10).map(|n| n as usize)
        }
        _ => None,
    }
}

/// Add the key `next` to `keys`, the keys of a merged hint. A `/` goes
/// between them unless `next` and the last key are each one symbol, as
/// in `↑↓` and `PgUp/PgDn`.
fn join_key(keys: &mut String, next: &str) {
    let symbols =
        next.chars().count() == 1 && !keys.chars().rev().nth(1).is_some_and(char::is_alphanumeric);
    if !symbols {
        keys.push('/');
    }
    keys.push_str(next);
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_client::{ClientUserInfo, Password, UserAccount};
    use hotline_proto::types::{HlDate, UserChangeOptions, UserFlags};

    use crate::config::{Bookmark, Config};
    use crate::session::{ArticleText, ChatInvite, Disconnect};
    use crate::transfer::Conflict;
    use crate::ui::Side;

    fn users() -> Vec<UserNameWithInfo> {
        ["Alice", "Bob"]
            .into_iter()
            .zip(1..)
            .map(|(name, user_id)| UserNameWithInfo {
                user_id,
                icon: 0,
                flags: UserFlags::empty(),
                name: name.into(),
            })
            .collect()
    }

    fn files() -> Vec<FileNameWithInfo> {
        [(*b"fldr", "Games"), (*b"TEXT", "readme.txt")]
            .into_iter()
            .map(|(file_type, name)| FileNameWithInfo {
                file_type,
                creator: [0; 4],
                size: 0,
                name: name.into(),
            })
            .collect()
    }

    fn areas() -> Areas {
        ui::areas(
            Rect::new(0, 0, 80, 24),
            Some(""),
            &[],
            Some(Side::USERS),
            false,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn click(kind: MouseEventKind, x: u16, y: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// The screen position of the `index`th user list row.
    fn user_row(index: u16) -> (u16, u16) {
        let inner = ui::inner(areas().users);
        (inner.x, inner.y + index)
    }

    fn type_text(view: &mut View, text: &str) {
        for c in text.chars() {
            let _ = view.handle_key(key(KeyCode::Char(c)), &users(), &files());
        }
    }

    #[test]
    fn join_key_puts_a_slash_between_two_words() {
        let join = |keys: &str, next| {
            let mut keys = keys.to_string();
            join_key(&mut keys, next);
            keys
        };
        assert_eq!(join("↑", "↓"), "↑↓");
        assert_eq!(join("⌥,", "."), "⌥,.");
        assert_eq!(join("PgUp", "PgDn"), "PgUp/PgDn");
        assert_eq!(join("Esc", "Enter"), "Esc/Enter");
    }

    #[test]
    fn a_line_starting_with_a_slash_is_sent_as_chat() {
        let mut view = granted();
        type_text(&mut view, "/users");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendChat("/users".into())
        );
    }

    #[test]
    fn shift_enter_starts_a_new_line() {
        let mut view = granted();
        type_text(&mut view, "one");
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(
            view.handle_key(shift_enter, &users(), &files()),
            Action::None
        );
        type_text(&mut view, "two");
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt_enter, &users(), &files()), Action::None);
        type_text(&mut view, "three");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendChat("one\ntwo\nthree".into())
        );
    }

    #[test]
    fn a_single_click_only_selects_a_user() {
        let mut view = View::default();
        let (x, y) = user_row(1);
        let left = click(MouseEventKind::Down(MouseButton::Left), x, y);
        let action = view.handle_mouse(left, &areas(), &users(), &files(), Instant::now());
        assert_eq!(action, Action::None);
        assert_eq!(view.selected_user(&users()), Some(2));
        assert_eq!(view.focus, Focus::Users);
        assert_eq!(view.modal, None);
    }

    #[test]
    fn a_double_click_opens_a_message_to_the_user() {
        let mut view = granted();
        let (x, y) = user_row(1);
        let left = click(MouseEventKind::Down(MouseButton::Left), x, y);
        let now = Instant::now();
        let _ = view.handle_mouse(left, &areas(), &users(), &files(), now);
        let _ = view.handle_mouse(
            left,
            &areas(),
            &users(),
            &files(),
            now + Duration::from_millis(200),
        );
        assert_eq!(
            view.modal,
            Some(Modal::InstantMsg {
                user_id: 2,
                name: "Bob".into(),
                input: TextBox::new()
            })
        );

        type_text(&mut view, "/hi");
        let alt_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT);
        let _ = view.handle_key(alt_enter, &users(), &files());
        type_text(&mut view, "there");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendInstantMsg {
                user_id: 2,
                text: "/hi\nthere".into()
            }
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn two_slow_clicks_do_not_open_a_message() {
        let mut view = View::default();
        let (x, y) = user_row(0);
        let left = click(MouseEventKind::Down(MouseButton::Left), x, y);
        let now = Instant::now();
        let _ = view.handle_mouse(left, &areas(), &users(), &files(), now);
        let _ = view.handle_mouse(
            left,
            &areas(),
            &users(),
            &files(),
            now + Duration::from_secs(1),
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn the_right_click_menu_gets_a_users_info() {
        let mut view = granted();
        let (x, y) = user_row(0);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), &files(), Instant::now());
        assert!(matches!(
            view.modal,
            Some(Modal::UserMenu { user_id: 1, .. })
        ));

        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::GetClientInfoText(1)
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn a_click_on_a_menu_item_chooses_it() {
        let mut view = granted();
        let (x, y) = user_row(0);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), &files(), Instant::now());

        let menu = ui::menu_rect((x, y), MenuItem::ALL, areas().screen);
        let inner = ui::inner(menu);
        let left = click(
            MouseEventKind::Down(MouseButton::Left),
            inner.x,
            inner.y + 1,
        );
        assert_eq!(
            view.handle_mouse(left, &areas(), &users(), &files(), Instant::now()),
            Action::GetClientInfoText(1)
        );
    }

    #[test]
    fn ctrl_w_deletes_a_word_and_ctrl_u_the_line() {
        let mut view = View::default();
        type_text(&mut view, "héllo wörld  ");
        let _ = view.handle_key(ctrl('w'), &users(), &files());
        assert_eq!(view.chat_input.as_str(), "héllo ");
        let _ = view.handle_key(ctrl('w'), &users(), &files());
        assert_eq!(view.chat_input.as_str(), "");

        type_text(&mut view, "ab cd");
        let _ = view.handle_key(ctrl('h'), &users(), &files());
        assert_eq!(view.chat_input.as_str(), "ab c");
        let ctrl_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::CONTROL);
        let _ = view.handle_key(ctrl_backspace, &users(), &files());
        assert_eq!(view.chat_input.as_str(), "ab ");
        type_text(&mut view, "cd");
        let alt_backspace = KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT);
        let _ = view.handle_key(alt_backspace, &users(), &files());
        assert_eq!(view.chat_input.as_str(), "ab ");
        view.chat_input = TextBox::new();

        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(&mut view, "go to");
        let _ = view.handle_key(ctrl('u'), &users(), &files());
        let Some(Modal::Palette(palette)) = &view.modal else {
            panic!("no palette");
        };
        assert_eq!(palette.input.as_str(), "");
    }

    #[test]
    fn readline_keys_move_the_cursor_in_the_input_line_and_a_modal() {
        let mut view = View {
            access: Access::all(),
            ..View::default()
        };
        type_text(&mut view, "ac d");
        let alt_b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT);
        let _ = view.handle_key(alt_b, &users(), &files());
        let _ = view.handle_key(key(KeyCode::Left), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Left), &users(), &files());
        type_text(&mut view, "b");
        let _ = view.handle_key(ctrl('a'), &users(), &files());
        type_text(&mut view, ">");
        let _ = view.handle_key(ctrl('e'), &users(), &files());
        type_text(&mut view, "!");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendChat(">abc d!".into())
        );

        view.modal = Some(Modal::Broadcast { input: "hi".into() });
        let _ = view.handle_key(key(KeyCode::Home), &users(), &files());
        type_text(&mut view, "oh ");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::UserBroadcast("oh hi".into())
        );
    }

    #[test]
    fn alt_keys_run_the_commands_that_work_now() {
        let mut view = view_with_rooms();
        view.access = Access::empty();
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt('a'), &users(), &files()), Action::None);
        assert_eq!(view.modal, None);
        assert_eq!(view.handle_key(alt('w'), &users(), &files()), Action::None);
        assert_eq!(view.chat_input.as_str(), "");

        view.show_room(9);
        assert_eq!(
            view.handle_key(alt('w'), &users(), &files()),
            Action::LeaveChat(9)
        );
        assert_eq!(
            view.handle_key(alt('o'), &users(), &files()),
            Action::EditOptions
        );
        assert_eq!(
            view.handle_key(alt('i'), &users(), &files()),
            Action::PickInvitee(9)
        );
        assert_eq!(
            view.handle_key(alt('s'), &users(), &files()),
            Action::EditChatSubject(9)
        );

        view.access = Access::BROADCAST;
        assert_eq!(view.handle_key(alt('a'), &users(), &files()), Action::None);
        assert!(matches!(view.modal, Some(Modal::Broadcast { .. })));
    }

    #[test]
    fn note_revisions_marks_a_hidden_tab_that_changed() {
        let mut view = View::default();
        view.note_revisions([(Tab::Chat, 1), (Tab::News, 1)]);
        view.note_revisions([(Tab::Chat, 2), (Tab::News, 2)]);
        assert_eq!(view.updated, HashSet::from([Tab::News]));
    }

    #[test]
    fn note_revisions_clears_the_mark_of_the_shown_tab() {
        let mut view = View::default();
        view.note_revisions([(Tab::News, 1)]);
        view.note_revisions([(Tab::News, 2)]);
        view.tab = Tab::News;
        view.note_revisions([(Tab::News, 2)]);
        view.tab = Tab::Chat;
        view.note_revisions([(Tab::News, 2)]);
        assert!(view.updated.is_empty());
    }

    #[test]
    fn set_tabs_forgets_the_mark_of_a_room_that_closed() {
        let mut view = View::default();
        view.set_tabs(vec![
            (Tab::Chat, "1 Chat".to_string()),
            (Tab::Room(7), "2 Chat 7".to_string()),
        ]);
        view.note_revisions([(Tab::Room(7), 1)]);
        view.note_revisions([(Tab::Room(7), 2)]);
        view.set_tabs(vec![(Tab::Chat, "1 Chat".to_string())]);
        assert!(view.updated.is_empty());
    }

    #[test]
    fn a_dialog_ignores_a_letter_with_alt() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('c'), &users(), &files());
        let alt_y = KeyEvent::new(KeyCode::Char('y'), KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt_y, &users(), &files()), Action::None);
        assert_eq!(view.modal, Some(Modal::Quit));
    }

    #[test]
    fn a_list_ignores_an_alt_key_with_no_command() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::F(3)), &users(), &files());
        let alt_z = KeyEvent::new(KeyCode::Char('z'), KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt_z, &users(), &files()), Action::None);
        assert_eq!(view.modal, None);
    }

    #[test]
    fn palette_key_shows_the_function_key_of_each_bar_tab() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        let Some(Modal::Palette(palette)) = &view.modal else {
            panic!("no palette");
        };
        let keys: Vec<_> = palette
            .matches()
            .filter_map(|entry| match entry.command {
                Command::OpenTab(tab) => Some((tab, &*entry.key)),
                _ => None,
            })
            .collect();
        let bar: Vec<_> = view.bar_tabs().iter().map(|&(tab, _)| tab).collect();
        assert!(!bar.is_empty());
        for (i, tab) in bar.iter().enumerate() {
            let want = format!("F{}", i + 1);
            assert!(keys.contains(&(*tab, want.as_str())), "{tab:?}");
        }
    }

    #[test]
    fn palette_key_comes_from_the_key_table() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        let Some(Modal::Palette(palette)) = &view.modal else {
            panic!("no palette");
        };
        let key = |command| {
            palette
                .matches()
                .find(|entry| entry.command == command)
                .map(|entry| &*entry.key)
        };
        assert_eq!(key(Command::Tasks), Some("⌥T"));
        assert_eq!(key(Command::Quit), Some("^C"));
    }

    #[test]
    fn alt_x_opens_the_palette() {
        let mut view = View::default();
        let alt_x = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT);
        let _ = view.handle_key(alt_x, &users(), &files());
        assert!(matches!(view.modal, Some(Modal::Palette(_))));
    }

    #[test]
    fn altgr_types_its_character() {
        let mut view = View::default();
        let altgr = KeyEvent::new(
            KeyCode::Char('@'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let _ = view.handle_key(altgr, &users(), &files());
        assert_eq!(view.chat_input.as_str(), "@");
        let ctrl_alt_x = KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL | KeyModifiers::ALT,
        );
        let _ = view.handle_key(ctrl_alt_x, &users(), &files());
        assert_eq!(view.chat_input.as_str(), "@");
    }

    #[test]
    fn ctrl_p_runs_the_command_that_matches_the_text() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(&mut view, "FILES");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::ListFiles
        );
        assert_eq!((view.tab, view.modal), (Tab::Files, None));
    }

    #[test]
    fn ctrl_p_lists_only_the_commands_that_work_now() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(&mut view, "leave");
        let Some(Modal::Palette(palette)) = &view.modal else {
            panic!("no palette");
        };
        assert_eq!(palette.chosen(), None);

        view.set_tabs(vec![
            (Tab::Chat, "Chat".into()),
            (Tab::Room(7), "Room".into()),
        ]);
        view.show_room(7);
        view.modal = None;
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(&mut view, "leave");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::LeaveChat(7)
        );
    }

    #[test]
    fn ctrl_o_opens_the_actions_of_the_selected_user() {
        let mut view = View {
            focus: Focus::Users,
            ..granted()
        };
        let _ = view.handle_key(ctrl('o'), &users(), &files());
        assert_eq!(view.modal, None);

        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        let _ = view.handle_key(ctrl('o'), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::GetClientInfoText(1)
        );
    }

    #[test]
    fn ctrl_o_opens_the_actions_of_the_room() {
        let mut view = View::default();
        view.set_tabs(vec![
            (Tab::Chat, "Chat".into()),
            (Tab::Room(7), "Room".into()),
        ]);
        view.show_room(7);
        let shift_f10 = KeyEvent::new(KeyCode::F(10), KeyModifiers::SHIFT);
        let _ = view.handle_key(shift_f10, &users(), &files());
        type_text(&mut view, "subj");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::EditChatSubject(7)
        );
    }

    #[test]
    fn broadcast_needs_the_privilege() {
        let mut view = View::default();
        assert_eq!(run_command(&mut view, "broadcast"), Action::None);
        assert!(matches!(view.modal, Some(Modal::Palette(_))));

        view.modal = None;
        view.access = Access::BROADCAST;
        let _ = run_command(&mut view, "broadcast");
        type_text(&mut view, "bye");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::UserBroadcast("bye".into())
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn the_options_form_sends_the_new_name_and_options() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(&mut view, "options");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::EditOptions
        );
        let info = ClientUserInfo {
            name: "me".into(),
            ..ClientUserInfo::default()
        };
        view.modal = Some(Modal::Options(OptionsForm::edit(&info, true)));
        type_text(&mut view, "w");
        for _ in 0..3 {
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        }
        type_text(&mut view, " ");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SetOptions {
                info: ClientUserInfo {
                    name: "mew".into(),
                    options: UserChangeOptions::REFUSE_PM,
                    ..ClientUserInfo::default()
                },
                queue_transfers: true,
            }
        );
    }

    #[test]
    fn k_disconnects_a_user_only_with_the_privilege() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Char('k')), &users(), &files());
        assert_eq!(view.modal, None);

        view.access = Access::DISCON_USER;
        let _ = view.handle_key(key(KeyCode::Char('k')), &users(), &files());
        assert_eq!(
            view.modal,
            Some(Modal::Disconnect {
                user_id: 1,
                name: "Alice".into()
            })
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('t')), &users(), &files()),
            Action::DisconnectUser {
                user_id: 1,
                ban: Ban::Temporary
            }
        );
    }

    #[test]
    fn the_user_menu_runs_disconnect_only_with_the_privilege() {
        let mut view = View::default();
        let (x, y) = user_row(1);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), &files(), Instant::now());
        for _ in 0..3 {
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        }
        let _ = view.handle_key(key(KeyCode::Enter), &users(), &files());
        assert!(matches!(view.modal, Some(Modal::UserMenu { .. })));

        view.access = Access::DISCON_USER;
        let _ = view.handle_key(key(KeyCode::Enter), &users(), &files());
        assert!(matches!(
            view.modal,
            Some(Modal::Disconnect { user_id: 2, .. })
        ));
    }

    #[test]
    fn file_keys_need_the_privilege_for_the_selected_kind() {
        let mut view = View {
            tab: Tab::Files,
            focus: Focus::Files,
            access: Access::DOWNLOAD_FILE | Access::DELETE_FOLDER | Access::RENAME_FILE,
            ..View::default()
        };
        let press = |view: &mut View, c| view.handle_key(key(KeyCode::Char(c)), &users(), &files());
        assert_eq!(press(&mut view, 'd'), Action::None);
        assert_eq!(press(&mut view, 'r'), Action::None);
        let _ = press(&mut view, 'x');
        assert!(matches!(view.modal, Some(Modal::DeleteFile { .. })));
        view.modal = None;
        assert_eq!(hint_enabled(&view, "d", "download"), Some(false));
        assert_eq!(hint_enabled(&view, "Enter", "open or download"), Some(true));

        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert!(matches!(press(&mut view, 'd'), Action::DownloadFile { .. }));
        assert!(matches!(press(&mut view, 'r'), Action::AskFileEdit { .. }));
        let _ = press(&mut view, 'x');
        assert_eq!(view.modal, None);
        assert_eq!(hint_enabled(&view, "x", "delete"), Some(false));
        for c in ['u', 'n', 'c', 'm', 'a'] {
            assert_eq!(press(&mut view, c), Action::None);
            assert_eq!(view.modal, None);
        }
    }

    #[test]
    fn file_keys_ask_for_text_info_or_a_delete_check() {
        let mut view = granted();
        let _ = view.handle_key(key(KeyCode::F(3)), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('m')), &users(), &files()),
            Action::AskFileEdit {
                edit: FileEdit::Move,
                name: "readme.txt".into()
            }
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('i')), &users(), &files()),
            Action::GetFileInfo("readme.txt".into())
        );
        let _ = view.handle_key(key(KeyCode::Char('x')), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y')), &users(), &files()),
            Action::DeleteFile("readme.txt".into())
        );

        view.modal = Some(Modal::FileEdit {
            edit: FileEdit::Rename,
            name: "readme.txt".into(),
            input: "readme".into(),
        });
        type_text(&mut view, ".md");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::EditFile {
                edit: FileEdit::Rename,
                name: "readme.txt".into(),
                text: "readme.md".into(),
            }
        );
    }

    #[test]
    fn news_tabs_fetch_nothing_without_the_read_privilege() {
        let mut view = View::default();
        assert_eq!(
            view.handle_key(key(KeyCode::F(2)), &users(), &files()),
            Action::None
        );
        assert_eq!(view.tab, Tab::News);
        type_text(&mut view, "hello");
        assert_eq!(hint_enabled(&view, "Enter", "post"), Some(false));
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );
        assert_eq!(view.handle_key(alt('.'), &users(), &files()), Action::None);
        assert_eq!(view.tab, Tab::Threaded);
    }

    #[test]
    fn threaded_news_keys_need_their_privileges() {
        let mut view = View {
            tab: Tab::Threaded,
            focus: Focus::NewsFolders,
            news_folder_count: 1,
            access: Access::NEWS_READ_ART,
            ..View::default()
        };
        for c in ['p', 'x', 'b', 'c'] {
            assert_eq!(
                view.handle_key(key(KeyCode::Char(c)), &users(), &files()),
                Action::None
            );
            assert_eq!(view.modal, None);
        }
        view.access |= Access::NEWS_DELETE_CAT | Access::POST_NEWS_ART;
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::AskDeleteNews(NewsTarget::Folder(0))
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('p')), &users(), &files()),
            Action::NewPost
        );
    }

    #[test]
    fn f2_opens_the_news_tab_and_fetches_the_news() {
        let mut view = granted();
        let f2 = key(KeyCode::F(2));
        assert_eq!(view.handle_key(f2, &users(), &files()), Action::GetMsgs);
        assert_eq!(view.tab, Tab::News);

        type_text(&mut view, "hello");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OldPostNews("hello".into())
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn a_click_on_the_news_tab_fetches_the_news() {
        let mut view = granted();
        let tabs = areas().tabs;
        let x = (tabs.x..tabs.right())
            .find(|&x| ui::tab_at(tabs, x, tabs.y, &view.tabs) == Some(Tab::News))
            .unwrap();
        let left = click(MouseEventKind::Down(MouseButton::Left), x, tabs.y);
        assert_eq!(
            view.handle_mouse(left, &areas(), &users(), &files(), Instant::now()),
            Action::GetMsgs
        );
    }

    /// A view with every privilege.
    fn granted() -> View {
        View {
            access: Access::all(),
            ..View::default()
        }
    }

    /// `Some(true)` if `view` shows the hint `key` `does` turned on,
    /// `Some(false)` if it shows it grayed, and `None` if it does not show it.
    fn hint_enabled(view: &View, key: &str, does: &str) -> Option<bool> {
        view.hints(&files())
            .iter()
            .find(|hint| hint.key == key && hint.does == does)
            .map(|hint| view.may(hint.enabled_by))
    }

    #[test]
    fn keymap_hints_come_from_the_key_table() {
        let mut view = View {
            modal: Some(Modal::Quit),
            ..View::default()
        };
        let hints = |view: &View| -> Vec<(String, &str)> {
            view.hints(&files())
                .into_iter()
                .map(|hint| (hint.key.into_owned(), hint.does))
                .collect()
        };
        assert_eq!(
            hints(&view),
            [("y".into(), "quit"), ("Esc".into(), "cancel")]
        );
        view.modal = Some(Modal::Post {
            parent_id: 0,
            title: TextBox::new(),
            body: TextBox::new(),
            on_body: false,
        });
        assert_eq!(
            hints(&view),
            [
                ("Tab".into(), "title or text"),
                ("Enter".into(), "post"),
                ("⌥⇧Enter".into(), "new line"),
                ("Esc".into(), "cancel"),
            ]
        );
    }

    /// The key and the text of each hint of `view`.
    fn hint_pairs(view: &View) -> Vec<(String, &'static str)> {
        view.hints(&files())
            .into_iter()
            .map(|hint| (hint.key.into_owned(), hint.does))
            .collect()
    }

    #[test]
    fn keymap_hints_of_the_list_boxes() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "choose"),
                ("Enter".into(), "run"),
                ("Esc".into(), "close"),
            ]
        );
        view.modal = Some(Modal::UserMenu {
            user_id: 1,
            at: (0, 0),
            items: MenuItem::ALL,
            selected: 0,
        });
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "choose"),
                ("Enter".into(), "select"),
                ("Esc".into(), "close"),
            ]
        );
        view.modal = Some(Modal::PickUser {
            chat_id: 1,
            users: Vec::new(),
            selected: 0,
        });
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "choose"),
                ("Enter".into(), "invite"),
                ("Esc".into(), "cancel"),
            ]
        );
    }

    #[test]
    fn keymap_hints_of_the_forms_show_toggle_on_a_checkbox_row() {
        let mut form = AccountForm::blank();
        let mut view = View {
            modal: Some(Modal::Account(form.clone())),
            ..View::default()
        };
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "row"),
                ("PgUp/PgDn".into(), "page"),
                ("Enter".into(), "save"),
                ("Esc".into(), "cancel"),
            ]
        );
        assert_eq!(hint_enabled(&view, "Enter", "save"), Some(false));
        form.row = account_form::TEXT_ROWS;
        view.modal = Some(Modal::Account(form));
        view.access = Access::CREATE_USER;
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "row"),
                ("PgUp/PgDn".into(), "page"),
                ("Space".into(), "toggle"),
                ("Enter".into(), "save"),
                ("Esc".into(), "cancel"),
            ]
        );
        assert_eq!(hint_enabled(&view, "Enter", "save"), Some(true));
        view.modal = Some(Modal::Connect(ConnectForm::new("", "", "")));
        assert_eq!(
            hint_pairs(&view),
            [
                ("↑↓".into(), "row"),
                ("Tab".into(), "next row"),
                ("Enter".into(), "connect"),
                ("⌥S".into(), "bookmark"),
                ("Esc".into(), "cancel"),
            ]
        );
    }

    #[test]
    fn keymap_hints_of_a_confirm_offer_replace_only_where_it_works() {
        let mut view = View {
            modal: Some(Modal::Confirm(Conflict::PartialUpload {
                local: "/tmp/a.txt".into(),
            })),
            ..View::default()
        };
        assert_eq!(
            hint_pairs(&view),
            [("r".into(), "resume"), ("Esc".into(), "cancel")]
        );
        view.modal = Some(Modal::Confirm(Conflict::PartialDownload {
            name: "a.txt".into(),
            save_as: "a.txt".into(),
        }));
        assert_eq!(
            hint_pairs(&view),
            [
                ("r".into(), "resume"),
                ("o".into(), "replace"),
                ("Esc".into(), "cancel"),
            ]
        );
    }

    #[test]
    fn keymap_hints_gray_a_reply_without_the_privilege() {
        let mut view = View {
            modal: Some(Modal::Article(ArticleText {
                id: 4,
                title: "Hello".into(),
                poster: "Alice".into(),
                date: HlDate::default(),
                text: "hi".into(),
            })),
            ..View::default()
        };
        assert_eq!(hint_enabled(&view, "r", "reply"), Some(false));
        assert_eq!(hint_enabled(&view, "Esc/Enter", "close"), Some(true));
        assert_eq!(
            view.handle_key(key(KeyCode::Char('r')), &users(), &files()),
            Action::None
        );
        assert!(matches!(view.modal, Some(Modal::Article(_))));
        view.access = Access::POST_NEWS_ART;
        assert_eq!(hint_enabled(&view, "r", "reply"), Some(true));
    }

    #[test]
    fn keymap_hints_of_the_panes() {
        let pairs = |view: &View| -> Vec<String> {
            hint_pairs(view)
                .into_iter()
                .map(|(key, does)| format!("{key} {does}"))
                .collect()
        };
        let mut view = granted();
        let _ = view.set_tab(Tab::Files);
        assert_eq!(
            pairs(&view),
            [
                "↑↓ choose",
                "Enter open or download",
                "⌫/← up",
                "d download",
                "u upload",
                "n new folder",
                "x delete",
                "i info",
                "r rename",
                "c comment",
                "m move",
                "a alias",
                "Esc chat",
            ]
        );
        let _ = view.set_tab(Tab::Threaded);
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(
            pairs(&view),
            [
                "↑↓ choose",
                "Enter read",
                "← folders",
                "p post",
                "x delete",
                "Tab preview",
                "Esc chat",
            ]
        );
        let _ = view.set_tab(Tab::Tasks);
        assert_eq!(pairs(&view), ["↑↓ choose", "⌥T back", "Esc chat"]);
        let _ = view.set_tab(Tab::Chat);
        assert_eq!(
            pairs(&view),
            [
                "Enter send",
                "⌥⇧Enter new line",
                "Tab users",
                "F1-F9 tab",
                "^P commands",
                "Esc quit",
            ]
        );
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(
            pairs(&view),
            [
                "↑↓ choose",
                "Enter message",
                "i info",
                "k disconnect",
                "Tab/Esc input",
                "^O actions",
            ]
        );
    }

    #[test]
    fn hints_follow_the_focus() {
        let mut view = View::default();
        assert!(hint_enabled(&view, "Enter", "send").is_some());
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert!(hint_enabled(&view, "Enter", "message").is_some());
    }

    #[test]
    fn hints_gray_disconnect_without_the_privilege() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(hint_enabled(&view, "k", "disconnect"), Some(false));
        view.access = Access::DISCON_USER;
        assert_eq!(hint_enabled(&view, "k", "disconnect"), Some(true));
    }

    #[test]
    fn chat_enter_needs_send_chat() {
        let mut view = View::default();
        *view.input_mut().unwrap() = "hi".into();
        assert_eq!(hint_enabled(&view, "Enter", "send"), Some(false));
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );
        view.access = Access::SEND_CHAT;
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendChat("hi".into())
        );
    }

    #[test]
    fn a_list_ignores_a_letter_with_ctrl() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::F(3)), &users(), &files());
        assert_eq!(view.handle_key(ctrl('x'), &users(), &files()), Action::None);
        assert_eq!(view.modal, None);
    }

    #[test]
    fn f3_opens_the_files_tab_and_walks_the_folders() {
        let mut view = granted();
        let f3 = key(KeyCode::F(3));
        assert_eq!(view.handle_key(f3, &users(), &files()), Action::ListFiles);
        assert_eq!((view.tab, view.focus), (Tab::Files, Focus::Files));

        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OpenFolder("Games".into())
        );
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::DownloadFile {
                name: "readme.txt".into(),
                save_as: "readme.txt".into(),
                start: Start::Ask,
            }
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Backspace), &users(), &files()),
            Action::ParentFolder
        );

        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!((view.tab, view.focus), (Tab::Chat, Focus::Input));
    }

    #[test]
    fn only_the_chat_tabs_show_the_user_list() {
        let mut view = View::default();
        assert!(view.shows_users());
        view.show_room(7);
        assert!(view.shows_users());
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        assert!(!view.shows_users());
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::Files);
    }

    #[test]
    fn a_double_click_on_a_folder_opens_it() {
        let mut view = View::default();
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        let areas = ui::areas(Rect::new(0, 0, 80, 24), None, &[], Some(Side::USERS), false);
        let inner = ui::inner(areas.body);
        let left = click(MouseEventKind::Down(MouseButton::Left), inner.x, inner.y);
        let now = Instant::now();

        let first = view.handle_mouse(left, &areas, &users(), &files(), now);
        assert_eq!(first, Action::None);
        let second = view.handle_mouse(
            left,
            &areas,
            &users(),
            &files(),
            now + Duration::from_millis(200),
        );
        assert_eq!(second, Action::OpenFolder("Games".into()));
    }

    #[test]
    fn d_on_the_file_list_downloads_a_folder_or_a_file() {
        let mut view = granted();
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('d')), &users(), &files()),
            Action::DownloadFolder {
                name: "Games".into(),
                start: Start::Ask,
            }
        );
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('d')), &users(), &files()),
            Action::DownloadFile {
                name: "readme.txt".into(),
                save_as: "readme.txt".into(),
                start: Start::Ask,
            }
        );
    }

    #[test]
    fn u_on_the_file_list_asks_for_a_file_to_upload() {
        let mut view = granted();
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        let _ = view.handle_key(key(KeyCode::Char('u')), &users(), &files());
        assert_eq!(
            view.modal,
            Some(Modal::Upload {
                input: TextBox::new()
            })
        );

        type_text(&mut view, "/tmp/a.txx");
        let _ = view.handle_key(key(KeyCode::Backspace), &users(), &files());
        type_text(&mut view, "t");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::Upload {
                path: "/tmp/a.txt".into(),
                start: Start::Ask,
            }
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn confirm_resumes_or_replaces_a_partial_download() {
        let conflict = Conflict::PartialDownload {
            name: "a.txt".into(),
            save_as: "a.txt".into(),
        };
        let mut view = View {
            modal: Some(Modal::ask(conflict.clone())),
            ..View::default()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Char('o')), &users(), &files()),
            Action::retry(conflict.clone(), Start::Replace)
        );

        view.modal = Some(Modal::ask(conflict.clone()));
        assert_eq!(
            view.handle_key(key(KeyCode::Char('r')), &users(), &files()),
            Action::retry(conflict, Start::Resume)
        );
        assert_eq!(view.modal, None);
    }

    /// Open the threaded news with F2 and Alt-`.`.
    fn open_threaded(view: &mut View) -> Action {
        let _ = view.handle_key(key(KeyCode::F(2)), &users(), &files());
        view.handle_key(alt('.'), &users(), &files())
    }

    fn alt(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT)
    }

    /// A view whose tab bar has rooms 7 and 9.
    fn view_with_rooms() -> View {
        let mut view = granted();
        let mut tabs = view.tabs.clone();
        tabs.push((Tab::Room(7), "Chat 7".to_string()));
        tabs.push((Tab::Room(9), "plans".to_string()));
        view.set_tabs(tabs);
        view
    }

    /// Run the first palette command that matches `text`.
    fn run_command(view: &mut View, text: &str) -> Action {
        let _ = view.handle_key(ctrl('p'), &users(), &files());
        type_text(view, text);
        view.handle_key(key(KeyCode::Enter), &users(), &files())
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    #[test]
    fn a_room_tab_sends_its_own_chat() {
        let mut view = view_with_rooms();
        view.show_room(7);
        type_text(&mut view, "hi");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendRoomChat {
                chat_id: 7,
                text: "hi".into()
            }
        );
        assert_eq!(view.chat_input.as_str(), "");
    }

    #[test]
    fn a_tab_number_picks_its_tab_and_skips_the_rooms() {
        let mut view = view_with_rooms();
        let alt_3 = KeyEvent::new(KeyCode::Char('3'), KeyModifiers::ALT);
        let _ = view.handle_key(alt_3, &users(), &files());
        assert_eq!(view.tab, Tab::Files);
        let _ = view.handle_key(key(KeyCode::F(4)), &users(), &files());
        assert_eq!(view.tab, Tab::Tasks);
        let _ = view.handle_key(key(KeyCode::F(5)), &users(), &files());
        assert_eq!(view.tab, Tab::Tasks);
        let _ = view.handle_key(key(KeyCode::F(1)), &users(), &files());
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn alt_comma_and_dot_step_through_the_room_row_and_wrap() {
        let mut view = view_with_rooms();
        for tab in [Tab::Room(7), Tab::Room(9), Tab::Chat] {
            let _ = view.handle_key(alt('.'), &users(), &files());
            assert_eq!(view.tab, tab);
        }
        let _ = view.handle_key(alt(','), &users(), &files());
        assert_eq!(view.tab, Tab::Room(9));
    }

    #[test]
    fn altgr_comma_types_and_keeps_the_tab() {
        let mut view = view_with_rooms();
        let altgr = KeyEvent::new(
            KeyCode::Char(','),
            KeyModifiers::ALT | KeyModifiers::CONTROL,
        );
        let _ = view.handle_key(altgr, &users(), &files());
        assert_eq!(view.tab, Tab::Chat);
        assert_eq!(view.chat_input.as_str(), ",");
    }

    #[test]
    fn alt_dot_does_nothing_off_the_chat_tab() {
        let mut view = view_with_rooms();
        let _ = view.handle_key(key(KeyCode::F(3)), &users(), &files());
        let alt = KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt, &users(), &files()), Action::None);
        assert_eq!(view.tab, Tab::Files);
    }

    #[test]
    fn sub_row_lists_public_and_each_room_on_the_chat_tab() {
        let mut view = granted();
        assert!(view.sub_row().is_empty());
        assert!(!view.has_sub_row());
        assert_eq!(hint_enabled(&view, "⌥,.", "room"), None);
        view = view_with_rooms();
        assert_eq!(hint_enabled(&view, "⌥,.", "room"), Some(true));
        let row: Vec<Tab> = view.sub_row().into_iter().map(|(tab, _)| tab).collect();
        assert_eq!(row, [Tab::Chat, Tab::Room(7), Tab::Room(9)]);
        assert!(view.has_sub_row());
        assert_eq!(view.bar_tabs().len(), Tab::FIXED.len());
        let _ = view.handle_key(key(KeyCode::F(3)), &users(), &files());
        assert!(view.sub_row().is_empty());
        assert!(!view.has_sub_row());
    }

    #[test]
    fn sub_row_lists_the_message_board_and_the_threaded_news_on_the_news_tab() {
        let mut view = granted();
        assert!(view.sub_row().is_empty());
        let _ = view.handle_key(key(KeyCode::F(2)), &users(), &files());
        let row: Vec<Tab> = view.sub_row().into_iter().map(|(tab, _)| tab).collect();
        assert_eq!(row, [Tab::News, Tab::Threaded]);
        assert_eq!(view.bar_tabs().len(), Tab::FIXED.len());
        assert_eq!(
            view.handle_key(alt(','), &users(), &files()),
            Action::ListThreadedNews
        );
        assert_eq!(
            view.handle_key(alt(','), &users(), &files()),
            Action::GetMsgs
        );
        assert_eq!(view.tab, Tab::News);
    }

    #[test]
    fn a_click_on_the_room_row_shows_its_room() {
        let mut view = view_with_rooms();
        let areas = ui::areas(
            Rect::new(0, 0, 80, 24),
            Some(""),
            &[],
            Some(Side::USERS),
            true,
        );
        let row = areas.sub_row;
        let x = (row.x..row.right())
            .find(|&x| ui::tab_at(row, x, row.y, &view.sub_row()) == Some(Tab::Room(7)))
            .unwrap();
        let left = click(MouseEventKind::Down(MouseButton::Left), x, row.y);
        let _ = view.handle_mouse(left, &areas, &users(), &files(), Instant::now());
        assert_eq!(view.tab, Tab::Room(7));
    }

    #[test]
    fn room_commands_leave_set_the_subject_and_invite() {
        let mut view = view_with_rooms();
        view.show_room(9);
        assert_eq!(run_command(&mut view, "leave"), Action::LeaveChat(9));
        assert_eq!(
            run_command(&mut view, "subject"),
            Action::EditChatSubject(9)
        );
        assert_eq!(run_command(&mut view, "invite"), Action::PickInvitee(9));
    }

    #[test]
    fn a_room_tab_that_is_gone_goes_back_to_chat() {
        let mut view = view_with_rooms();
        view.show_room(9);
        view.set_tabs(View::default().tabs);
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn an_invite_joins_on_enter_and_declines_on_esc() {
        let invite = || {
            Some(Modal::Invite(ChatInvite {
                chat_id: 7,
                from: "Alice".into(),
            }))
        };
        let mut view = View {
            modal: invite(),
            ..View::default()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::JoinChat(7)
        );
        view.modal = invite();
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::RejectChatInvite(7)
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn pick_user_invites_the_chosen_user() {
        let mut view = View {
            modal: Some(Modal::PickUser {
                chat_id: 7,
                users: vec![(1, "Alice".into()), (2, "Bob".into())],
                selected: 0,
            }),
            ..View::default()
        };
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::InviteToChat {
                chat_id: 7,
                user_id: 2
            }
        );
    }

    #[test]
    fn the_user_menu_opens_a_private_chat() {
        let mut view = View {
            modal: Some(Modal::UserMenu {
                user_id: 2,
                at: (0, 0),
                items: MenuItem::ALL,
                selected: 0,
            }),
            ..granted()
        };
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::InviteNewChat(2)
        );
    }

    #[test]
    fn confirm_of_an_upload_offers_no_replace() {
        let conflict = Conflict::PartialUpload {
            local: "/tmp/a.txt".into(),
        };
        let mut view = View {
            modal: Some(Modal::ask(conflict.clone())),
            ..View::default()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Char('o')), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, Some(Modal::Confirm(conflict)));

        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn save_as_downloads_under_the_typed_name() {
        let mut view = View {
            modal: Some(Modal::ask(Conflict::LocalFileExists {
                name: "a.txt".into(),
                save_as: "a.txt".into(),
            })),
            ..View::default()
        };
        let _ = view.handle_key(key(KeyCode::Backspace), &users(), &files());
        type_text(&mut view, "md");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::DownloadFile {
                name: "a.txt".into(),
                save_as: "a.txmd".into(),
                start: Start::Ask,
            }
        );
    }

    #[test]
    fn a_click_on_threaded_news_selects_a_folder_or_previews_an_article() {
        let mut view = View {
            tab: Tab::Threaded,
            news_folder_count: 2,
            news_count: 2,
            ..View::default()
        };
        let body = Rect::new(0, 0, 80, 20);
        let [folders, threads, preview] = ui::news_areas(body, view.splits);
        let now = Instant::now();
        let folder = ui::inner(folders);
        assert_eq!(
            view.click_threaded_news(body, folder.x, folder.y + 1, now),
            Action::HighlightNewsFolder(1)
        );
        assert_eq!(view.focus, Focus::NewsFolders);
        let thread = ui::inner(threads);
        assert_eq!(
            view.click_threaded_news(body, thread.x, thread.y, now),
            Action::None
        );
        assert_eq!(
            view.click_threaded_news(body, thread.x, thread.y + 1, now),
            Action::PreviewArticle(0)
        );
        assert_eq!(
            view.click_threaded_news(body, thread.x, thread.y + 1, now),
            Action::OpenArticle(0)
        );
        let preview = ui::inner(preview);
        let _ = view.click_threaded_news(body, preview.x, preview.y, now);
        assert_eq!(view.focus, Focus::NewsPreview);
    }

    #[test]
    fn the_wheel_scrolls_the_preview_only_over_it() {
        let mut view = View {
            tab: Tab::Threaded,
            ..View::default()
        };
        let areas = areas();
        let [folders, _, preview] = ui::news_areas(areas.body, view.splits);
        let wheel = |x, y| click(MouseEventKind::ScrollDown, x, y);
        let _ = view.handle_mouse(
            wheel(folders.x, folders.y),
            &areas,
            &users(),
            &files(),
            Instant::now(),
        );
        assert_eq!(view.preview_scroll, 0);
        let _ = view.handle_mouse(
            wheel(preview.x, preview.y),
            &areas,
            &users(),
            &files(),
            Instant::now(),
        );
        assert_eq!(view.preview_scroll, 3);
    }

    #[test]
    fn esc_on_the_chat_input_asks_before_it_quits() {
        let mut view = View::default();
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, Some(Modal::Quit));
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, None);
        let _ = view.handle_key(key(KeyCode::Esc), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y')), &users(), &files()),
            Action::Quit
        );
    }

    #[test]
    fn disconnected_reconnects_quits_or_closes() {
        let disconnected = || View {
            modal: Some(Modal::Disconnected(Disconnect::Closed)),
            ..View::default()
        };
        for (key, action) in [
            (key(KeyCode::Char('r')), Action::Reconnect),
            (key(KeyCode::Char('q')), Action::Quit),
            (ctrl('c'), Action::Quit),
            (key(KeyCode::Enter), Action::None),
            (key(KeyCode::Esc), Action::None),
        ] {
            let mut view = disconnected();
            assert_eq!(view.handle_key(key, &users(), &files()), action);
            if action != Action::Quit {
                assert_eq!(view.modal, None);
            }
        }
        let mut view = disconnected();
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, Some(Modal::Disconnected(Disconnect::Closed)));
    }

    #[test]
    fn connect_form_sends_the_server_and_the_login() {
        let mut view = View {
            modal: Some(Modal::Connect(ConnectForm::new(
                "example.com:x",
                "alice",
                "",
            ))),
            ..View::default()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );
        assert!(matches!(view.modal, Some(Modal::Connect(_))));
        view.handle_key(key(KeyCode::Backspace), &users(), &files());
        view.handle_key(key(KeyCode::Char('1')), &users(), &files());
        view.handle_key(key(KeyCode::Down), &users(), &files());
        view.handle_key(key(KeyCode::Down), &users(), &files());
        view.handle_key(key(KeyCode::Char('p')), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::Connect {
                host: "example.com".into(),
                port: 1,
                login: "alice".into(),
                password: "p".into(),
            }
        );
        assert_eq!(view.modal, None);
    }

    fn bookmarks_config() -> Config {
        let bookmark = |name: &str| Bookmark {
            name: Some(name.into()),
            address: "example.com".into(),
            login: "".into(),
            password: "".into(),
        };
        Config {
            bookmarks: vec![bookmark("One"), bookmark("Two")],
            auto_connect: Some("Two".into()),
            ..Config::default()
        }
    }

    #[test]
    fn bookmarks_marks_the_auto_connect_bookmark_and_keeps_the_selection_in_the_list() {
        let Modal::Bookmarks { rows, selected } = Modal::bookmarks(&bookmarks_config(), 5) else {
            panic!("no bookmarks");
        };
        let marks: Vec<_> = rows
            .iter()
            .map(|row| (&*row.name, row.auto_connect))
            .collect();
        assert_eq!(marks, [("One", false), ("Two", true)]);
        assert_eq!(selected, 1);
    }

    #[test]
    fn alt_r_opens_the_bookmarks() {
        let mut view = View::default();
        assert_eq!(
            view.handle_key(alt('r'), &users(), &files()),
            Action::OpenBookmarks(None)
        );
    }

    #[test]
    fn bookmarks_runs_each_key_on_the_selected_bookmark() {
        let config = bookmarks_config();
        let mut view = View::default();
        let mut press = |code| {
            view.modal = Some(Modal::bookmarks(&config, 0));
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
            view.handle_key(key(code), &users(), &files())
        };
        assert_eq!(press(KeyCode::Char('e')), Action::EditBookmark(1));
        assert_eq!(press(KeyCode::Char('a')), Action::ToggleAutoConnect(1));
        assert_eq!(press(KeyCode::Down), Action::None);
        assert!(matches!(
            view.modal,
            Some(Modal::Bookmarks { selected: 1, .. })
        ));
    }

    #[test]
    fn bookmarks_delete_asks_first_and_a_cancel_goes_back_to_the_list() {
        let mut view = View {
            modal: Some(Modal::bookmarks(&bookmarks_config(), 1)),
            ..View::default()
        };
        let mut press = |code| view.handle_key(key(code), &users(), &files());
        assert_eq!(press(KeyCode::Char('x')), Action::None);
        assert_eq!(press(KeyCode::Esc), Action::None);
        assert_eq!(press(KeyCode::Char('x')), Action::None);
        assert_eq!(press(KeyCode::Char('y')), Action::DeleteBookmark(1));
        view.modal = Some(Modal::bookmarks(&bookmarks_config(), 1));
        view.handle_key(key(KeyCode::Char('x')), &users(), &files());
        assert!(matches!(
            view.modal,
            Some(Modal::DeleteBookmark { selected: 1, .. })
        ));
        view.handle_key(key(KeyCode::Esc), &users(), &files());
        assert_eq!(view.modal, Some(Modal::bookmarks(&bookmarks_config(), 1)));
    }

    #[test]
    fn connect_form_of_a_bookmark_goes_back_to_the_bookmarks_on_esc() {
        let config = bookmarks_config();
        let mut view = View {
            modal: Some(Modal::Connect(ConnectForm::edit(&config.bookmarks[1]))),
            ..View::default()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::OpenBookmarks(Some("Two".into()))
        );
        view.modal = Some(Modal::Connect(ConnectForm::new("", "", "")));
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, None);
    }

    #[test]
    fn bookmarks_enter_connects_and_shows_the_public_chat() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::F(4)), &users(), &files());
        view.modal = Some(Modal::bookmarks(&bookmarks_config(), 0));
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::ConnectBookmark(0)
        );
        assert_eq!((view.tab, view.modal.is_none()), (Tab::Chat, true));
    }

    #[test]
    fn bookmarks_with_no_bookmark_does_nothing() {
        let mut view = View {
            modal: Some(Modal::bookmarks(&Config::default(), 0)),
            ..View::default()
        };
        for code in [KeyCode::Enter, KeyCode::Char('e'), KeyCode::Char('x')] {
            assert_eq!(view.handle_key(key(code), &users(), &files()), Action::None);
        }
        assert!(matches!(view.modal, Some(Modal::Bookmarks { .. })));
    }

    #[test]
    fn connect_form_tab_moves_to_the_next_row_and_back() {
        let mut view = View {
            modal: Some(Modal::Connect(ConnectForm::new("", "", ""))),
            ..View::default()
        };
        let mut row = |code| {
            view.handle_key(key(code), &users(), &files());
            match &view.modal {
                Some(Modal::Connect(form)) => form.row,
                _ => panic!("the form closed"),
            }
        };
        assert_eq!(row(KeyCode::Tab), 1);
        assert_eq!(row(KeyCode::Tab), 2);
        assert_eq!(row(KeyCode::Tab), 0);
        assert_eq!(row(KeyCode::BackTab), 2);
        assert_eq!(row(KeyCode::BackTab), 1);
    }

    #[test]
    fn connect_form_asks_for_the_name_then_saves_a_bookmark_and_stays_open() {
        let form = ConnectForm::new("example.com", "alice", "secret");
        let mut view = View {
            modal: Some(Modal::Connect(form.clone())),
            ..View::default()
        };
        assert_eq!(view.handle_key(alt('s'), &users(), &files()), Action::None);
        assert!(matches!(view.modal, Some(Modal::BookmarkName { .. })));
        let bookmark = form.bookmark().unwrap();
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SaveBookmark {
                old_name: None,
                bookmark: bookmark.clone(),
            }
        );
        assert_eq!(
            view.modal,
            Some(Modal::Connect(ConnectForm {
                edits: Some(bookmark.name().into()),
                ..form
            }))
        );
    }

    #[test]
    fn bookmark_name_saves_under_a_typed_name_and_keeps_it() {
        let mut view = View {
            modal: Some(Modal::Connect(ConnectForm::new("example.com", "", ""))),
            ..View::default()
        };
        view.handle_key(alt('s'), &users(), &files());
        view.handle_key(key(KeyCode::Char('H')), &users(), &files());
        let Action::SaveBookmark { bookmark, .. } =
            view.handle_key(key(KeyCode::Enter), &users(), &files())
        else {
            panic!("no bookmark");
        };
        assert_eq!(bookmark.name.as_deref(), Some("H"));
        view.handle_key(alt('s'), &users(), &files());
        let Some(Modal::BookmarkName { input, .. }) = &view.modal else {
            panic!("no name box");
        };
        assert_eq!(input.as_str(), "H");
    }

    #[test]
    fn bookmark_name_cancel_goes_back_to_the_connect_form() {
        let form = ConnectForm::new("example.com", "", "");
        let mut view = View {
            modal: Some(Modal::Connect(form.clone())),
            ..View::default()
        };
        view.handle_key(alt('s'), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Esc), &users(), &files()),
            Action::None
        );
        assert_eq!(view.modal, Some(Modal::Connect(form)));
    }

    #[test]
    fn connect_form_saves_no_bookmark_for_a_bad_address() {
        let form = ConnectForm::new("example.com:x", "", "");
        let mut view = View {
            modal: Some(Modal::Connect(form.clone())),
            ..View::default()
        };
        assert_eq!(view.handle_key(alt('s'), &users(), &files()), Action::None);
        assert_eq!(view.modal, Some(Modal::Connect(form)));
    }

    #[test]
    fn palette_offers_disconnect_or_reconnect() {
        let mut view = View::default();
        assert!(view.commands(&[]).contains(&Command::Disconnect));
        assert!(!view.commands(&[]).contains(&Command::Reconnect));
        view.disconnected = true;
        assert!(view.commands(&[]).contains(&Command::Reconnect));
        assert!(!view.commands(&[]).contains(&Command::Disconnect));
        assert!(view.commands(&[]).contains(&Command::ConnectTo));
    }

    #[test]
    fn ctrl_c_twice_quits() {
        let mut view = View::default();
        assert_eq!(view.handle_key(ctrl('c'), &users(), &files()), Action::None);
        assert_eq!(view.handle_key(ctrl('c'), &users(), &files()), Action::Quit);
    }

    #[test]
    fn alt_dot_opens_threaded_news_and_walks_its_panes() {
        let mut view = granted();
        assert_eq!(open_threaded(&mut view), Action::ListThreadedNews);
        assert_eq!((view.tab, view.focus), (Tab::Threaded, Focus::NewsFolders));
        assert!(!view.shows_users());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );

        view.news_folder_count = 2;
        assert_eq!(
            view.handle_key(key(KeyCode::Down), &users(), &files()),
            Action::HighlightNewsFolder(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Down), &users(), &files()),
            Action::HighlightNewsFolder(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OpenNewsFolder(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Right), &users(), &files()),
            Action::ExpandNewsFolder(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Left), &users(), &files()),
            Action::CollapseNewsFolder(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::AskDeleteNews(NewsTarget::Folder(1))
        );

        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::NewsThreads);
        view.news_count = 3;
        assert_eq!(
            view.handle_key(key(KeyCode::Down), &users(), &files()),
            Action::PreviewArticle(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OpenArticle(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::AskDeleteNews(NewsTarget::Article(1))
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('p')), &users(), &files()),
            Action::NewPost
        );

        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::NewsPreview);
        let _ = view.handle_key(key(KeyCode::PageDown), &users(), &files());
        assert_eq!(view.preview_scroll, 10);
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::NewsFolders);

        assert_eq!(
            view.handle_key(key(KeyCode::F(1)), &users(), &files()),
            Action::None
        );
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn a_reply_to_an_article_posts_its_title_and_text() {
        let mut view = View {
            modal: Some(Modal::Article(ArticleText {
                id: 4,
                title: "Hello".into(),
                poster: "Alice".into(),
                date: HlDate::default(),
                text: "hi".into(),
            })),
            ..granted()
        };
        let _ = view.handle_key(key(KeyCode::Char('r')), &users(), &files());
        type_text(&mut view, "a");
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        let _ = view.handle_key(shift_enter, &users(), &files());
        type_text(&mut view, "b");
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        type_text(&mut view, "!");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::PostArticle {
                parent_id: 4,
                title: "Re: Hello!".into(),
                text: "a\nb".into(),
            }
        );
    }

    #[test]
    fn a_new_category_takes_its_name_and_a_delete_takes_y() {
        let mut view = granted();
        let _ = open_threaded(&mut view);
        let _ = view.handle_key(key(KeyCode::Char('c')), &users(), &files());
        type_text(&mut view, "Gen");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::NewNewsItem {
                name: "Gen".into(),
                kind: NewsFolder::Category,
            }
        );

        view.modal = Some(Modal::DeleteNews {
            target: NewsTarget::Article(3),
            name: "Gen".into(),
        });
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y')), &users(), &files()),
            Action::DeleteNewsItem(NewsTarget::Article(3))
        );
    }

    #[test]
    fn account_keys_need_their_privileges() {
        let mut view = View {
            tab: Tab::Admin,
            focus: Focus::Accounts,
            account_count: 1,
            access: Access::CREATE_USER,
            ..View::default()
        };
        for c in ['o', 'x'] {
            assert_eq!(
                view.handle_key(key(KeyCode::Char(c)), &users(), &files()),
                Action::None
            );
            assert_eq!(view.modal, None);
        }
        let _ = view.handle_key(key(KeyCode::Char('n')), &users(), &files());
        let Some(Modal::Account(form)) = &mut view.modal else {
            panic!("no account form");
        };
        form.login = "bo".into();
        form.old_login = Some("bo".into());
        assert_eq!(hint_enabled(&view, "Enter", "save"), Some(false));
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );
        view.access = Access::MODIFY_USER;
        assert!(matches!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SaveAccount { .. }
        ));
    }

    #[test]
    fn f5_opens_the_admin_tab_only_when_it_is_shown() {
        let mut view = granted();
        assert_eq!(
            view.handle_key(key(KeyCode::F(5)), &users(), &files()),
            Action::None
        );
        assert_eq!(view.tab, Tab::Chat);

        let mut tabs = view.tabs.clone();
        tabs.insert(Tab::FIXED.len(), (Tab::Admin, Tab::ADMIN_LABEL.to_string()));
        view.set_tabs(tabs);
        assert_eq!(
            view.handle_key(key(KeyCode::F(5)), &users(), &files()),
            Action::ListAccounts
        );
        assert_eq!((view.tab, view.focus), (Tab::Admin, Focus::Accounts));
        view.account_count = 2;
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::EditAccount(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::AskDeleteAccount(1)
        );
        let _ = view.handle_key(key(KeyCode::Char('o')), &users(), &files());
        type_text(&mut view, "bo");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OpenAccount("bo".into())
        );
    }

    #[test]
    fn a_new_account_saves_its_text_and_toggled_privileges() {
        let mut view = View {
            modal: Some(Modal::Account(AccountForm::blank())),
            ..granted()
        };
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );
        type_text(&mut view, "Bo B");
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        type_text(&mut view, "bo");
        for _ in 0..3 {
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        }
        type_text(&mut view, " ");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SaveAccount {
                old_login: None,
                account: UserAccount {
                    name: "Bo B".into(),
                    login: "bo".into(),
                    password: Password::Empty,
                    access: Access::UPLOAD_FILE,
                },
            }
        );
    }
}
