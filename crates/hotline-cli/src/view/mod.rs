//! [`View`]: the tab, focus, selection, and modals. Pure: turns key presses
//! and mouse clicks into an [`Action`] for the caller to run.

mod account_form;
mod action;
mod command;
mod hint;
mod modal;
mod options_form;
mod tab;
mod text_box;

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use hotline_client::Ban;
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use hotline_proto::Access;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use ratatui::widgets::{ListState, TableState};

use crate::file_edit::FileEdit;
use crate::session::{NewsFolder, NewsTarget};
use crate::transfer::{Conflict, Start};
use crate::ui::{self, Areas};

pub use account_form::{privileges, AccountForm};
pub use action::Action;
pub use command::{Command, Palette};
pub use hint::Hint;
pub use modal::{MenuItem, Modal};
pub use options_form::{OptionsForm, OPTIONS};
pub use tab::Tab;
pub use text_box::TextBox;

/// The longest gap between the two clicks of a double click.
const DOUBLE_CLICK: Duration = Duration::from_millis(500);

/// Shift+Enter, or Alt+Enter or Ctrl+J where the terminal cannot report Shift+Enter.
fn is_new_line(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter => key
            .modifiers
            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT),
        KeyCode::Char('j') => key.modifiers.contains(KeyModifiers::CONTROL),
        _ => false,
    }
}

/// Whether `key` is a character with Ctrl or Alt.
fn is_modified_char(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char(_))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
}

/// The code of `key`, or [`KeyCode::Null`] for a character with Ctrl or
/// Alt.
fn plain_code(key: KeyEvent) -> KeyCode {
    if is_modified_char(key) {
        KeyCode::Null
    } else {
        key.code
    }
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
}

impl Focus {
    /// The focus that leaves the user list on `tab`.
    const fn main(tab: Tab) -> Self {
        match tab {
            Tab::Chat | Tab::News | Tab::Room(_) => Self::Input,
            Tab::Files => Self::Files,
            Tab::Threaded => Self::NewsFolders,
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
    /// Each tab and its label: the tab bar in order, then the rooms.
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
    pub files: ListState,
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
    /// The tabs that changed since the user last showed them.
    pub updated: HashSet<Tab>,
    /// The revision of each tab when the user last showed it.
    seen: HashMap<Tab, u64>,
    selected_user: Option<u16>,
    last_click: Option<(Clicked, Instant)>,
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
                .collect(),
            chat_input: TextBox::new(),
            post_input: TextBox::new(),
            room_inputs: HashMap::new(),
            news_scroll: 0,
            users_offset: 0,
            files: ListState::default().with_selected(Some(0)),
            news_folders: TableState::default().with_selected(Some(0)),
            news_folder_count: 0,
            news: TableState::default().with_selected(Some(0)),
            news_count: 0,
            preview_scroll: 0,
            accounts: ListState::default().with_selected(Some(0)),
            account_count: 0,
            access: Access::empty(),
            updated: HashSet::new(),
            seen: HashMap::new(),
            selected_user: None,
            last_click: None,
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
            Tab::Files | Tab::Threaded | Tab::Admin => None,
            Tab::Room(id) => Some(self.room_inputs.get(&id).unwrap_or(&EMPTY)),
        }
    }

    fn input_mut(&mut self) -> Option<&mut TextBox> {
        match self.tab {
            Tab::Chat => Some(&mut self.chat_input),
            Tab::News => Some(&mut self.post_input),
            Tab::Files | Tab::Threaded | Tab::Admin => None,
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

    /// The index in `tabs` of the first room.
    fn first_room(&self) -> usize {
        self.tabs
            .iter()
            .position(|&(tab, _)| tab != tab.top())
            .unwrap_or(self.tabs.len())
    }

    /// The tabs of the tab bar.
    pub fn bar_tabs(&self) -> &[(Tab, String)] {
        self.tabs.get(..self.first_room()).unwrap_or_default()
    }

    /// The row under the Chat tab: Public, then each room. It is empty
    /// with no room, or on another tab.
    pub fn room_row(&self) -> Vec<(Tab, String)> {
        let rooms = self.tabs.get(self.first_room()..).unwrap_or_default();
        if rooms.is_empty() || self.tab.top() != Tab::Chat {
            return Vec::new();
        }
        std::iter::once((Tab::Chat, Tab::PUBLIC_LABEL.to_string()))
            .chain(rooms.iter().cloned())
            .collect()
    }

    /// Whether the room row shows.
    pub fn has_room_row(&self) -> bool {
        !self.room_row().is_empty()
    }

    /// The entry of the room row after the shown one, or before it, with
    /// wraparound.
    fn step_room(&self, forward: bool) -> Option<Tab> {
        let row = self.room_row();
        let at = row.iter().position(|&(tab, _)| tab == self.tab)?;
        let next = if forward { at + 1 } else { at + row.len() - 1 };
        row.get(next % row.len()).map(|&(tab, _)| tab)
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
    pub fn hints(&self, files: &[FileNameWithInfo]) -> Vec<Hint> {
        match (&self.modal, self.focus, self.tab) {
            (None, Focus::Users, _) => self.users_hints(),
            (None, Focus::Files, _) | (None, Focus::Input, Tab::Files) => self.files_hints(files),
            _ => self.focus_hints(),
        }
    }

    /// The keys of the file list, with the privileges for the selected item.
    fn files_hints(&self, files: &[FileNameWithInfo]) -> Vec<Hint> {
        let folder = self
            .selected_file(files)
            .is_some_and(FileNameWithInfo::is_folder);
        let edit =
            |key, does, edit: FileEdit| Hint::new(key, does).enabled_by(edit.enabled_by(folder));
        let (open, download, delete) = if folder {
            (
                Access::empty(),
                Access::DOWNLOAD_FOLDER,
                Access::DELETE_FOLDER,
            )
        } else {
            (
                Access::DOWNLOAD_FILE,
                Access::DOWNLOAD_FILE,
                Access::DELETE_FILE,
            )
        };
        vec![
            Hint::new("↑↓", "select"),
            Hint::new("Enter", "open or download").enabled_by(open),
            Hint::new("⌫", "up"),
            Hint::new("d", "download").enabled_by(download),
            Hint::new("u", "upload").enabled_by(Access::UPLOAD_FILE | Access::UPLOAD_FOLDER),
            edit("n", "new folder", FileEdit::NewFolder),
            Hint::new("x", "delete").enabled_by(delete),
            Hint::new("i", "info"),
            edit("r", "rename", FileEdit::Rename),
            edit("c", "comment", FileEdit::Comment),
            edit("m", "move", FileEdit::Move),
            edit("a", "alias", FileEdit::Alias),
            Hint::new("Tab", "users"),
            Hint::new("Esc", "chat"),
        ]
    }

    /// The selected item of the file list.
    fn selected_file<'a>(&self, files: &'a [FileNameWithInfo]) -> Option<&'a FileNameWithInfo> {
        files.get(selected(self.files.selected(), files.len()))
    }

    /// The keys of the user list.
    fn users_hints(&self) -> Vec<Hint> {
        let main = match Focus::main(self.tab) {
            Focus::Files => "files",
            Focus::NewsFolders | Focus::NewsThreads | Focus::NewsPreview => "news",
            Focus::Accounts => "accounts",
            Focus::Input | Focus::Users => "input",
        };
        vec![
            Hint::new("↑↓", "select"),
            Hint::new("^O", "actions"),
            Hint::new("Enter", "message").enabled_by(MenuItem::SendMessage.enabled_by()),
            Hint::new("i", "info").enabled_by(MenuItem::GetInfo.enabled_by()),
            Hint::new("k", "disconnect").enabled_by(MenuItem::Disconnect.enabled_by()),
            Hint::new("Tab", main),
            Hint::new("Esc", main),
        ]
    }

    /// The keys of the modal or the focused area, other than the user list.
    fn focus_hints(&self) -> Vec<Hint> {
        match (&self.modal, self.focus, self.tab) {
            (Some(Modal::InstantMsg { .. } | Modal::Broadcast { .. }), ..) => {
                vec![
                    Hint::new("Enter", "send"),
                    Hint::new("⇧Enter", "new line"),
                    Hint::new("Esc", "cancel"),
                ]
            }
            (Some(Modal::Palette(_)), ..) => {
                vec![
                    Hint::new("↑↓", "choose"),
                    Hint::new("Enter", "run"),
                    Hint::new("Esc", "close"),
                ]
            }
            (Some(Modal::UserMenu { .. }), ..) => {
                vec![
                    Hint::new("↑↓", "choose"),
                    Hint::new("Enter", "select"),
                    Hint::new("Esc", "close"),
                ]
            }
            (Some(Modal::Info(_) | Modal::FileInfo(_)), ..) => vec![Hint::new("Esc", "close")],
            (Some(Modal::FileEdit { .. }), ..) => {
                vec![Hint::new("Enter", "save"), Hint::new("Esc", "cancel")]
            }
            (Some(Modal::Disconnect { .. }), ..) => vec![
                Hint::new("d", "disconnect"),
                Hint::new("t", "ban for a while"),
                Hint::new("p", "ban for good"),
                Hint::new("Esc", "cancel"),
            ],
            (Some(Modal::Upload { .. }), ..) => {
                vec![Hint::new("Enter", "upload"), Hint::new("Esc", "cancel")]
            }
            (Some(Modal::SaveAs { .. }), ..) => {
                vec![Hint::new("Enter", "save"), Hint::new("Esc", "cancel")]
            }
            (
                Some(Modal::Confirm(
                    Conflict::PartialDownload { .. } | Conflict::LocalFolderExists { .. },
                )),
                ..,
            ) => vec![
                Hint::new("r", "resume"),
                Hint::new("o", "replace"),
                Hint::new("Esc", "cancel"),
            ],
            (Some(Modal::Confirm(_)), ..) => {
                vec![Hint::new("r", "resume"), Hint::new("Esc", "cancel")]
            }
            (Some(Modal::Invite(_)), ..) => {
                vec![Hint::new("Enter", "join"), Hint::new("Esc", "decline")]
            }
            (Some(Modal::Subject { .. }), ..) => {
                vec![Hint::new("Enter", "set"), Hint::new("Esc", "cancel")]
            }
            (Some(Modal::PickDownload { .. }), ..) => {
                vec![
                    Hint::new("↑↓", "choose"),
                    Hint::new("Enter", "cancel it"),
                    Hint::new("Esc", "close"),
                ]
            }
            (Some(Modal::PickUser { .. }), ..) => {
                vec![
                    Hint::new("↑↓", "choose"),
                    Hint::new("Enter", "invite"),
                    Hint::new("Esc", "cancel"),
                ]
            }
            (Some(Modal::Article(_)), ..) => {
                vec![
                    Hint::new("r", "reply").enabled_by(Access::POST_NEWS_ART),
                    Hint::new("Esc", "close"),
                ]
            }
            (Some(Modal::Post { .. }), ..) => vec![
                Hint::new("Tab", "title or text"),
                Hint::new("Enter", "post"),
                Hint::new("⇧Enter", "new line"),
                Hint::new("Esc", "cancel"),
            ],
            (Some(Modal::NewsName { .. }), ..) => {
                vec![Hint::new("Enter", "create"), Hint::new("Esc", "cancel")]
            }
            (
                Some(
                    Modal::DeleteNews { .. }
                    | Modal::DeleteAccount { .. }
                    | Modal::DeleteFile { .. },
                ),
                ..,
            ) => vec![Hint::new("y", "delete"), Hint::new("Esc", "cancel")],
            (Some(Modal::Account(form)), ..) => vec![
                Hint::new("↑↓", "row"),
                Hint::new("Space", "toggle"),
                Hint::new("Enter", "save").enabled_by(form.saved_by()),
                Hint::new("Esc", "cancel"),
            ],
            (Some(Modal::Options(_)), ..) => vec![
                Hint::new("↑↓", "row"),
                Hint::new("Space", "toggle"),
                Hint::new("Enter", "save"),
                Hint::new("Esc", "cancel"),
            ],
            (Some(Modal::Quit), ..) => vec![Hint::new("y", "quit"), Hint::new("Esc", "cancel")],
            (Some(Modal::OpenAccount { .. }), ..) => {
                vec![Hint::new("Enter", "open"), Hint::new("Esc", "cancel")]
            }
            (None, Focus::Accounts, _) | (None, Focus::Input, Tab::Admin) => vec![
                Hint::new("↑↓", "select"),
                Hint::new("Enter", "edit").enabled_by(Access::OPEN_USER | Access::CREATE_USER),
                Hint::new("n", "new").enabled_by(Access::CREATE_USER),
                Hint::new("o", "open").enabled_by(Access::OPEN_USER),
                Hint::new("x", "delete").enabled_by(Access::DELETE_USER),
                Hint::new("Tab", "users"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::NewsFolders, _) | (None, Focus::Input, Tab::Threaded) => vec![
                Hint::new("↑↓", "select"),
                Hint::new("Enter", "open"),
                Hint::new("←→", "close/open"),
                Hint::new("p", "post").enabled_by(Access::POST_NEWS_ART),
                Hint::new("x", "delete").enabled_by(Access::NEWS_DELETE_CAT),
                Hint::new("b", "new bundle").enabled_by(Access::NEWS_CREATE_FLDR),
                Hint::new("c", "new category").enabled_by(Access::NEWS_CREATE_CAT),
                Hint::new("Tab", "threads"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::NewsThreads, _) => vec![
                Hint::new("↑↓", "select"),
                Hint::new("Enter", "read"),
                Hint::new("←", "folders"),
                Hint::new("p", "post").enabled_by(Access::POST_NEWS_ART),
                Hint::new("x", "delete").enabled_by(Access::NEWS_DELETE_ART),
                Hint::new("Tab", "preview"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::NewsPreview, _) => vec![
                Hint::new("↑↓", "scroll"),
                Hint::new("p", "post").enabled_by(Access::POST_NEWS_ART),
                Hint::new("Tab", "folders"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::Input, Tab::Chat) => [
                Hint::new("Enter", "send").enabled_by(Access::SEND_CHAT),
                Hint::new("⇧Enter", "new line"),
                Hint::new("Tab", "users"),
            ]
            .into_iter()
            .chain(self.has_room_row().then_some(Hint::new("Alt-,.", "room")))
            .chain([
                Hint::new("F1-F9", "tab"),
                Hint::new("^P", "commands"),
                Hint::new("Esc", "quit"),
            ])
            .collect(),
            (None, Focus::Input, Tab::Room(_)) => vec![
                Hint::new("Enter", "send").enabled_by(Access::SEND_CHAT),
                Hint::new("Tab", "users"),
                Hint::new("Alt-,.", "room"),
                Hint::new("^P", "commands"),
                Hint::new("^O", "actions"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::Input, Tab::News) => vec![
                Hint::new("Enter", "post").enabled_by(Access::POST_NEWS_ART),
                Hint::new("⇧Enter", "new line"),
                Hint::new("↑↓", "scroll"),
                Hint::new("Tab", "users"),
                Hint::new("Esc", "chat"),
            ],
            (None, Focus::Users | Focus::Files, _) | (None, Focus::Input, Tab::Files) => Vec::new(),
        }
    }

    pub fn handle_key(
        &mut self,
        key: KeyEvent,
        users: &[UserNameWithInfo],
        files: &[FileNameWithInfo],
    ) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            if self.modal == Some(Modal::Quit) {
                return Action::Quit;
            }
            self.modal = Some(Modal::Quit);
            return Action::None;
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key, users);
        }
        if let Some(number) = tab_number(key) {
            return match self.bar_tabs().get(number - 1) {
                Some(&(tab, _)) => self.set_tab(tab),
                None => Action::None,
            };
        }
        if let Some(forward) = room_step(key) {
            return self
                .step_room(forward)
                .map_or(Action::None, |tab| self.set_tab(tab));
        }
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if (ctrl && key.code == KeyCode::Char('p')) || (alt && key.code == KeyCode::Char('x')) {
            let palette = Palette::new("commands", self.commands(users), &self.tabs);
            self.modal = Some(Modal::Palette(palette));
            return Action::None;
        }
        if let Some(command) = self.alt_command(key, users) {
            return self.run(command, users);
        }
        if is_menu_key(key) {
            let commands = self.focus_commands(users);
            if !commands.is_empty() {
                self.modal = Some(Modal::Palette(Palette::new(
                    "actions", commands, &self.tabs,
                )));
            }
            return Action::None;
        }
        if self.focus != Focus::Input && is_modified_char(key) {
            return Action::None;
        }
        match self.focus {
            Focus::Users => self.users_key(key, users),
            Focus::Input => self.input_key(key),
            Focus::Files => self.files_key(key, files),
            Focus::NewsFolders => self.news_folders_key(key),
            Focus::NewsThreads => self.news_threads_key(key),
            Focus::NewsPreview => self.news_preview_key(key),
            Focus::Accounts => self.accounts_key(key),
        }
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
            Command::CancelDownload,
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
            | Focus::Accounts => Vec::new(),
        }
    }

    /// The command of Alt with a letter, if it works now.
    fn alt_command(&self, key: KeyEvent, users: &[UserNameWithInfo]) -> Option<Command> {
        if !key.modifiers.contains(KeyModifiers::ALT) {
            return None;
        }
        let command = match key.code {
            KeyCode::Char('a') => Command::Broadcast,
            KeyCode::Char('o') => Command::Options,
            KeyCode::Char('i') => Command::Invite(self.tab.chat_id()?),
            KeyCode::Char('s') => Command::Subject(self.tab.chat_id()?),
            KeyCode::Char('w') => Command::Leave(self.tab.chat_id()?),
            _ => return None,
        };
        self.commands(users).contains(&command).then_some(command)
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
            Command::CancelDownload => Action::PickDownload,
            Command::Invite(chat_id) => Action::PickInvitee(chat_id),
            Command::Subject(chat_id) => Action::EditChatSubject(chat_id),
            Command::Leave(chat_id) => Action::LeaveChat(chat_id),
            Command::Quit => {
                self.modal = Some(Modal::Quit);
                Action::None
            }
            Command::User(user_id, item) => self.choose(user_id, item, users),
        }
    }

    fn palette_key(
        &mut self,
        mut palette: Palette,
        key: KeyEvent,
        users: &[UserNameWithInfo],
    ) -> Action {
        match key.code {
            KeyCode::Esc => return Action::None,
            KeyCode::Enter => match palette.chosen() {
                Some(command) if self.may(command.enabled_by()) => return self.run(command, users),
                _ => {}
            },
            KeyCode::Up => palette.select_previous(),
            KeyCode::Down => palette.select_next(),
            _ => palette.edit(|input| input.edit(key)),
        }
        self.modal = Some(Modal::Palette(palette));
        Action::None
    }

    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        if let Modal::Palette(palette) = modal {
            return self.palette_key(palette, key, users);
        }
        if let Modal::Confirm(conflict) = modal {
            return self.confirm_key(conflict, key);
        }
        if let Modal::Account(form) = modal {
            return self.account_form_key(form, key);
        }
        if let Modal::Options(form) = modal {
            return self.options_form_key(form, key);
        }
        match (&mut modal, plain_code(key)) {
            (Modal::Article(article), KeyCode::Char('r')) if self.may(Access::POST_NEWS_ART) => {
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
            (Modal::Disconnect { user_id, .. }, KeyCode::Char(c @ ('d' | 't' | 'p'))) => {
                let ban = match c {
                    't' => Ban::Temporary,
                    'p' => Ban::Permanent,
                    _ => Ban::None,
                };
                return Action::DisconnectUser {
                    user_id: *user_id,
                    ban,
                };
            }
            (Modal::DeleteFile { name }, KeyCode::Char('y')) => {
                return Action::DeleteFile(std::mem::take(name));
            }
            (Modal::FileEdit { edit, name, input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::EditFile {
                    edit: *edit,
                    name: std::mem::take(name),
                    text: input.take(),
                };
            }
            (Modal::Quit, KeyCode::Char('y')) => return Action::Quit,
            (Modal::DeleteNews { target, .. }, KeyCode::Char('y')) => {
                return Action::DeleteNewsItem(*target);
            }
            (Modal::DeleteAccount { login }, KeyCode::Char('y')) => {
                return Action::DeleteAccount(std::mem::take(login));
            }
            (Modal::OpenAccount { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::OpenAccount(input.take());
            }
            (Modal::Post { body, on_body, .. }, _) if is_new_line(key) => {
                if *on_body {
                    body.insert('\n');
                }
            }
            (Modal::Post { on_body, .. }, KeyCode::Tab) => *on_body = !*on_body,
            (
                Modal::Post {
                    parent_id,
                    title,
                    body,
                    ..
                },
                KeyCode::Enter,
            ) if !title.is_empty() => {
                return Action::PostArticle {
                    parent_id: *parent_id,
                    title: title.take(),
                    text: body.take(),
                };
            }
            (Modal::NewsName { kind, input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::NewNewsItem {
                    name: input.take(),
                    kind: *kind,
                };
            }
            (Modal::Invite(invite), KeyCode::Enter) => return Action::JoinChat(invite.chat_id),
            (Modal::Invite(invite), KeyCode::Esc) => {
                return Action::RejectChatInvite(invite.chat_id);
            }
            (Modal::InstantMsg { input, .. } | Modal::Broadcast { input }, _)
                if is_new_line(key) =>
            {
                input.insert('\n');
            }
            (_, KeyCode::Esc)
            | (Modal::Info(_) | Modal::FileInfo(_) | Modal::Article(_), KeyCode::Enter) => {
                return Action::None;
            }
            (Modal::InstantMsg { user_id, input, .. }, KeyCode::Enter) if !input.is_empty() => {
                return Action::SendInstantMsg {
                    user_id: *user_id,
                    text: input.take(),
                };
            }
            (Modal::Broadcast { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::UserBroadcast(input.take());
            }
            (Modal::Upload { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::Upload {
                    path: input.take(),
                    start: Start::Ask,
                };
            }
            (Modal::Subject { chat_id, input }, KeyCode::Enter) => {
                return Action::SetChatSubject {
                    chat_id: *chat_id,
                    subject: input.take(),
                };
            }
            (
                Modal::PickUser {
                    chat_id,
                    users,
                    selected,
                },
                KeyCode::Enter,
            ) => {
                return users.get(*selected).map_or(Action::None, |&(user_id, _)| {
                    Action::InviteToChat {
                        chat_id: *chat_id,
                        user_id,
                    }
                });
            }
            (
                Modal::PickUser { selected, .. } | Modal::PickDownload { selected, .. },
                KeyCode::Up,
            ) => {
                *selected = selected.saturating_sub(1);
            }
            (
                Modal::PickDownload {
                    selected,
                    downloads,
                },
                KeyCode::Down,
            ) => {
                *selected = (*selected + 1).min(downloads.len().saturating_sub(1));
            }
            (
                Modal::PickDownload {
                    selected,
                    downloads,
                },
                KeyCode::Enter,
            ) => {
                return downloads
                    .get(*selected)
                    .map_or(Action::None, |&(id, _)| Action::KillDownload(id));
            }
            (
                Modal::PickUser {
                    selected, users, ..
                },
                KeyCode::Down,
            ) => {
                *selected = (*selected + 1).min(users.len().saturating_sub(1));
            }
            (Modal::SaveAs { name, input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::DownloadFile {
                    name: std::mem::take(name),
                    save_as: input.take().into(),
                    start: Start::Ask,
                };
            }
            (Modal::UserMenu { selected, .. }, KeyCode::Up) => {
                *selected = selected.saturating_sub(1);
            }
            (
                Modal::UserMenu {
                    selected, items, ..
                },
                KeyCode::Down,
            ) => {
                *selected = (*selected + 1).min(items.len() - 1);
            }
            (
                Modal::UserMenu {
                    user_id,
                    selected,
                    items,
                    ..
                },
                KeyCode::Enter,
            ) if let Some(&item) = items.get(*selected)
                && self.may(item.enabled_by()) =>
            {
                return self.choose(*user_id, item, users);
            }
            (modal, _) => {
                if let Some(input) = modal.input_mut() {
                    input.edit(key);
                }
            }
        }
        self.modal = Some(modal);
        Action::None
    }

    fn confirm_key(&mut self, conflict: Conflict, key: KeyEvent) -> Action {
        match plain_code(key) {
            KeyCode::Char('r') => Action::retry(conflict, Start::Resume),
            KeyCode::Char('o') if conflict.can_replace() => Action::retry(conflict, Start::Replace),
            KeyCode::Esc => Action::None,
            _ => {
                self.modal = Some(Modal::Confirm(conflict));
                Action::None
            }
        }
    }

    fn users_key(&mut self, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        match key.code {
            KeyCode::Tab | KeyCode::Esc => self.focus = Focus::main(self.tab),
            KeyCode::Up => self.move_selection(users, false),
            KeyCode::Down => self.move_selection(users, true),
            KeyCode::Enter => {
                if let Some(user_id) = self.selected_user(users) {
                    return self.choose(user_id, MenuItem::SendMessage, users);
                }
            }
            KeyCode::Char('i') => {
                if let Some(user_id) = self.selected_user(users) {
                    return self.choose(user_id, MenuItem::GetInfo, users);
                }
            }
            KeyCode::Char('k') => {
                if let Some(user_id) = self.selected_user(users) {
                    return self.choose(user_id, MenuItem::Disconnect, users);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn input_key(&mut self, key: KeyEvent) -> Action {
        let tab = self.tab;
        let may_chat = self.may(Access::SEND_CHAT);
        let may_post = self.may(Access::POST_NEWS_ART);
        let Some(input) = self.input_mut() else {
            return Action::None;
        };
        if is_new_line(key) {
            input.insert('\n');
            return Action::None;
        }
        match (key.code, tab) {
            (KeyCode::Enter, Tab::Chat) if may_chat && !input.is_empty() => {
                return Action::SendChat(input.take());
            }
            (KeyCode::Enter, Tab::News) if may_post && !input.is_empty() => {
                return Action::OldPostNews(input.take());
            }
            (KeyCode::Enter, Tab::Room(chat_id)) if may_chat && !input.is_empty() => {
                return Action::SendRoomChat {
                    chat_id,
                    text: input.take(),
                };
            }
            _ if input.edit(key) => {}
            (KeyCode::Tab, _) => self.focus = Focus::Users,
            (KeyCode::Esc, Tab::Chat) => self.modal = Some(Modal::Quit),
            (KeyCode::Esc, _) => return self.set_tab(Tab::Chat),
            (KeyCode::Up, Tab::News) => self.scroll_news(-1),
            (KeyCode::Down, Tab::News) => self.scroll_news(1),
            (KeyCode::PageUp, Tab::News) => self.scroll_news(-10),
            (KeyCode::PageDown, Tab::News) => self.scroll_news(10),
            _ => {}
        }
        Action::None
    }

    fn files_key(&mut self, key: KeyEvent, files: &[FileNameWithInfo]) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.files.select_previous(),
            KeyCode::Down => self.files.select_next(),
            KeyCode::Enter => {
                return self.open_file(selected(self.files.selected(), files.len()), files)
            }
            KeyCode::Backspace | KeyCode::Left => return Action::ParentFolder,
            KeyCode::Char('d') => {
                return self.download(selected(self.files.selected(), files.len()), files)
            }
            KeyCode::Char('u') if self.may(Access::UPLOAD_FILE | Access::UPLOAD_FOLDER) => {
                self.modal = Some(Modal::Upload {
                    input: TextBox::new(),
                });
            }
            KeyCode::Char('n') if self.may(FileEdit::NewFolder.enabled_by(false)) => {
                return Action::AskFileEdit {
                    edit: FileEdit::NewFolder,
                    name: Box::default(),
                };
            }
            KeyCode::Char(c @ ('x' | 'i' | 'r' | 'c' | 'm' | 'a')) => {
                let Some(file) = self.selected_file(files) else {
                    return Action::None;
                };
                let name = file.name.clone();
                let folder = file.is_folder();
                let edit = match c {
                    'x' => {
                        let delete = if folder {
                            Access::DELETE_FOLDER
                        } else {
                            Access::DELETE_FILE
                        };
                        if self.may(delete) {
                            self.modal = Some(Modal::DeleteFile { name });
                        }
                        return Action::None;
                    }
                    'i' => return Action::GetFileInfo(name),
                    'r' => FileEdit::Rename,
                    'c' => FileEdit::Comment,
                    'm' => FileEdit::Move,
                    _ => FileEdit::Alias,
                };
                if !self.may(edit.enabled_by(folder)) {
                    return Action::None;
                }
                return Action::AskFileEdit { edit, name };
            }
            _ => {}
        }
        Action::None
    }

    fn account_form_key(&mut self, mut form: AccountForm, key: KeyEvent) -> Action {
        let last = AccountForm::rows() - 1;
        match key.code {
            KeyCode::Esc => return Action::None,
            KeyCode::Enter if !form.login.is_empty() && self.may(form.saved_by()) => {
                return Action::SaveAccount {
                    old_login: form.old_login.clone(),
                    account: form.account(),
                };
            }
            KeyCode::Up => form.row = form.row.saturating_sub(1),
            KeyCode::Down => form.row = (form.row + 1).min(last),
            KeyCode::PageUp => form.row = form.row.saturating_sub(10),
            KeyCode::PageDown => form.row = (form.row + 10).min(last),
            KeyCode::Char(' ') if form.row >= account_form::TEXT_ROWS => form.toggle(),
            _ if form.text_mut().is_some_and(|text| text.edit(key)) => {}
            _ => {}
        }
        self.modal = Some(Modal::Account(form));
        Action::None
    }

    fn options_form_key(&mut self, mut form: OptionsForm, key: KeyEvent) -> Action {
        let last = OptionsForm::rows() - 1;
        match key.code {
            KeyCode::Esc => return Action::None,
            KeyCode::Enter => {
                if let Some(info) = form.info() {
                    return Action::SetClientUserInfo(info);
                }
            }
            KeyCode::Up => form.row = form.row.saturating_sub(1),
            KeyCode::Down => form.row = (form.row + 1).min(last),
            KeyCode::Char(' ') if form.row >= options_form::TEXT_ROWS => form.toggle(),
            _ if form.text_mut().is_some_and(|text| text.edit(key)) => {}
            _ => {}
        }
        self.modal = Some(Modal::Options(form));
        Action::None
    }

    fn accounts_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.accounts.select_previous(),
            KeyCode::Down => self.accounts.select_next(),
            KeyCode::Enter
                if self.account_count > 0 && self.may(Access::OPEN_USER | Access::CREATE_USER) =>
            {
                return Action::EditAccount(selected(self.accounts.selected(), self.account_count));
            }
            KeyCode::Char('n') if self.may(Access::CREATE_USER) => {
                self.modal = Some(Modal::Account(AccountForm::blank()));
            }
            KeyCode::Char('o') if self.may(Access::OPEN_USER) => {
                self.modal = Some(Modal::OpenAccount {
                    input: TextBox::new(),
                });
            }
            KeyCode::Char('x') if self.account_count > 0 && self.may(Access::DELETE_USER) => {
                return Action::AskDeleteAccount(selected(
                    self.accounts.selected(),
                    self.account_count,
                ));
            }
            _ => {}
        }
        Action::None
    }

    fn news_folders_key(&mut self, key: KeyEvent) -> Action {
        let count = self.news_folder_count;
        let index = selected(self.news_folders.selected(), count);
        match key.code {
            KeyCode::Tab => self.focus = Focus::NewsThreads,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up | KeyCode::Down if count > 0 => {
                let index = step(index, count, key.code == KeyCode::Down);
                self.news_folders.select(Some(index));
                return Action::HighlightNewsFolder(index);
            }
            KeyCode::Enter if count > 0 => return Action::OpenNewsFolder(index),
            KeyCode::Right if count > 0 => return Action::ExpandNewsFolder(index),
            KeyCode::Left | KeyCode::Backspace if count > 0 => {
                return Action::CollapseNewsFolder(index);
            }
            KeyCode::Char('p') if self.may(Access::POST_NEWS_ART) => return Action::NewPost,
            KeyCode::Char('x') if count > 0 && self.may(Access::NEWS_DELETE_CAT) => {
                return Action::AskDeleteNews(NewsTarget::Folder(index));
            }
            KeyCode::Char(c @ ('b' | 'c'))
                if self.may(if c == 'c' {
                    Access::NEWS_CREATE_CAT
                } else {
                    Access::NEWS_CREATE_FLDR
                }) =>
            {
                self.modal = Some(Modal::NewsName {
                    kind: if c == 'c' {
                        NewsFolder::Category
                    } else {
                        NewsFolder::Bundle
                    },
                    input: TextBox::new(),
                });
            }
            _ => {}
        }
        Action::None
    }

    fn news_threads_key(&mut self, key: KeyEvent) -> Action {
        let count = self.news_count;
        let index = selected(self.news.selected(), count);
        match key.code {
            KeyCode::Tab => self.focus = Focus::NewsPreview,
            KeyCode::Left => self.focus = Focus::NewsFolders,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up | KeyCode::Down if count > 0 => {
                let index = step(index, count, key.code == KeyCode::Down);
                self.news.select(Some(index));
                return Action::PreviewArticle(index);
            }
            KeyCode::Enter if count > 0 => return Action::OpenArticle(index),
            KeyCode::Char('p') if self.may(Access::POST_NEWS_ART) => return Action::NewPost,
            KeyCode::Char('x') if count > 0 && self.may(Access::NEWS_DELETE_ART) => {
                return Action::AskDeleteNews(NewsTarget::Article(index));
            }
            _ => {}
        }
        Action::None
    }

    fn news_preview_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::NewsFolders,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.scroll_preview(-1),
            KeyCode::Down => self.scroll_preview(1),
            KeyCode::PageUp => self.scroll_preview(-10),
            KeyCode::PageDown => self.scroll_preview(10),
            KeyCode::Char('p') if self.may(Access::POST_NEWS_ART) => return Action::NewPost,
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
                | Modal::PickDownload { .. }
                | Modal::Palette(_)
                | Modal::Post { .. }
                | Modal::NewsName { .. }
                | Modal::DeleteNews { .. }
                | Modal::Quit
                | Modal::Account(_)
                | Modal::Options(_)
                | Modal::OpenAccount { .. }
                | Modal::DeleteAccount { .. } => {}
            }
            return Action::None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = ui::tab_at(areas.tabs, x, y, self.bar_tabs()) {
                    return self.set_tab(tab);
                }
                if let Some(tab) = ui::tab_at(areas.rooms, x, y, &self.room_row()) {
                    return self.set_tab(tab);
                }
                if let Some(user_id) = self.user_at(areas.users, x, y, users) {
                    return self.click_user(user_id, users, now);
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
                        items: &MenuItem::ALL,
                        selected: 0,
                    });
                }
            }
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
        let [folders, threads, preview] = ui::news_areas(body);
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
        self.tab = tab;
        if self.focus != Focus::Users || !self.shows_users() {
            self.focus = Focus::main(tab);
        }
        if !self.may(tab.enabled_by()) {
            return Action::None;
        }
        match tab {
            Tab::Chat | Tab::Room(_) => Action::None,
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
            (Some(i), true) => (i + 1).min(last),
            (Some(i), false) => i.saturating_sub(1),
        };
        self.selected_user = users.get(next).map(|u| u.user_id);
    }

    /// `true` when `x`, `y` is in the article preview of the Threaded News tab.
    fn in_preview(&self, body: Rect, x: u16, y: u16) -> bool {
        self.tab == Tab::Threaded && contains(ui::news_areas(body)[2], x, y)
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
        let index = self.users_offset + usize::from(y - inner.y);
        users.get(index).map(|u| u.user_id)
    }

    /// The index of the file on the list row at `x`, `y` of the Files tab.
    fn file_at(&self, body: Rect, x: u16, y: u16, files: &[FileNameWithInfo]) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Files || !contains(inner, x, y) {
            return None;
        }
        row_at(self.files.offset(), y - inner.y, files.len())
    }

    /// The index of the account on the list row at `x`, `y` of the Admin tab.
    fn account_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Admin || !contains(inner, x, y) {
            return None;
        }
        row_at(self.accounts.offset(), y - inner.y, self.account_count)
    }

    /// The row of the folder tree at `x`, `y`.
    fn news_folder_at(&self, folders: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(folders);
        if !contains(inner, x, y) {
            return None;
        }
        row_at(
            self.news_folders.offset(),
            y - inner.y,
            self.news_folder_count,
        )
    }

    /// The row of the thread list at `x`, `y`, below the header row.
    fn news_at(&self, threads: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(threads);
        if !contains(inner, x, y) || y == inner.y {
            return None;
        }
        row_at(self.news.offset(), y - inner.y - 1, self.news_count)
    }

    /// `false` on a tab that hides the user list.
    pub fn shows_users(&self) -> bool {
        self.tab != Tab::Threaded
    }
}

/// The selected row of a list of `len` rows, kept in range.
fn selected(selected: Option<usize>, len: usize) -> usize {
    selected.unwrap_or(0).min(len.saturating_sub(1))
}

/// The row after `index` in a list of `len` rows, or the row before it if
/// not `down`, kept in range.
const fn step(index: usize, len: usize, down: bool) -> usize {
    if down {
        if index + 1 < len {
            index + 1
        } else {
            index
        }
    } else {
        index.saturating_sub(1)
    }
}

/// The index of the row on screen line `row` of a list of `len` rows
/// that starts at `offset`.
fn row_at(offset: usize, row: u16, len: usize) -> Option<usize> {
    Some(offset + usize::from(row)).filter(|&index| index < len)
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

/// Ctrl+O, Shift+F10, or the Menu key.
const fn is_menu_key(key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('o') => key.modifiers.contains(KeyModifiers::CONTROL),
        KeyCode::F(10) => key.modifiers.contains(KeyModifiers::SHIFT),
        KeyCode::Menu => true,
        _ => false,
    }
}

/// `Some(true)` for Alt-`.`, the next room, and `Some(false)` for Alt-`,`.
const fn room_step(key: KeyEvent) -> Option<bool> {
    if !key.modifiers.contains(KeyModifiers::ALT) || key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match key.code {
        KeyCode::Char('.') => Some(true),
        KeyCode::Char(',') => Some(false),
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_client::{ClientUserInfo, Password, UserAccount};
    use hotline_proto::types::{HlDate, UserChangeOptions, UserFlags};

    use crate::session::{ArticleText, ChatInvite};

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
        ui::areas(Rect::new(0, 0, 80, 24), Some(""), &[], true, false)
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
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(view.handle_key(ctrl_j, &users(), &files()), Action::None);
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

        let menu = ui::menu_rect((x, y), &MenuItem::ALL, areas().screen);
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
        let alt_r = KeyEvent::new(KeyCode::Char('r'), KeyModifiers::ALT);
        assert_eq!(view.handle_key(alt_r, &users(), &files()), Action::None);
        assert_eq!(view.modal, None);
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
        view.modal = Some(Modal::Options(OptionsForm::edit(&info)));
        type_text(&mut view, "w");
        for _ in 0..3 {
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        }
        type_text(&mut view, " ");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SetClientUserInfo(ClientUserInfo {
                name: "mew".into(),
                options: UserChangeOptions::REFUSE_PM,
                ..ClientUserInfo::default()
            })
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
    fn cancel_download_asks_for_a_download_and_cancels_the_chosen_one() {
        let mut view = View::default();
        assert_eq!(run_command(&mut view, "cancel"), Action::PickDownload);
        view.modal = Some(Modal::PickDownload {
            downloads: vec![(3, "a.txt".into()), (5, "b.txt (queued)".into())],
            selected: 0,
        });
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::KillDownload(5)
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
        assert_eq!(
            view.handle_key(key(KeyCode::F(4)), &users(), &files()),
            Action::None
        );
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
    fn tab_from_the_user_list_returns_to_the_file_list() {
        let mut view = View::default();
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::Users);
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::Files);
    }

    #[test]
    fn a_double_click_on_a_folder_opens_it() {
        let mut view = View::default();
        let f3 = key(KeyCode::F(3));
        let _ = view.handle_key(f3, &users(), &files());
        let areas = ui::areas(Rect::new(0, 0, 80, 24), None, &[], true, false);
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
        let _ = view.handle_key(key(KeyCode::F(5)), &users(), &files());
        assert_eq!(view.tab, Tab::Files);
        let _ = view.handle_key(key(KeyCode::F(1)), &users(), &files());
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn alt_comma_and_dot_step_through_the_room_row_and_wrap() {
        let mut view = view_with_rooms();
        let alt = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::ALT);
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
    fn room_row_lists_public_and_each_room_on_the_chat_tab() {
        let mut view = granted();
        assert!(view.room_row().is_empty());
        assert!(!view.has_room_row());
        assert_eq!(hint_enabled(&view, "Alt-,.", "room"), None);
        view = view_with_rooms();
        assert_eq!(hint_enabled(&view, "Alt-,.", "room"), Some(true));
        let row: Vec<Tab> = view.room_row().into_iter().map(|(tab, _)| tab).collect();
        assert_eq!(row, [Tab::Chat, Tab::Room(7), Tab::Room(9)]);
        assert!(view.has_room_row());
        assert_eq!(view.bar_tabs().len(), Tab::FIXED.len());
        let _ = view.handle_key(key(KeyCode::F(2)), &users(), &files());
        assert!(view.room_row().is_empty());
        assert!(!view.has_room_row());
    }

    #[test]
    fn a_click_on_the_room_row_shows_its_room() {
        let mut view = view_with_rooms();
        let areas = ui::areas(Rect::new(0, 0, 80, 24), Some(""), &[], true, true);
        let row = areas.rooms;
        let x = (row.x..row.right())
            .find(|&x| ui::tab_at(row, x, row.y, &view.room_row()) == Some(Tab::Room(7)))
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
                items: &MenuItem::ALL,
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
        let [folders, threads, preview] = ui::news_areas(body);
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
        let [folders, _, preview] = ui::news_areas(areas.body);
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
    fn ctrl_c_twice_quits() {
        let mut view = View::default();
        assert_eq!(view.handle_key(ctrl('c'), &users(), &files()), Action::None);
        assert_eq!(view.handle_key(ctrl('c'), &users(), &files()), Action::Quit);
    }

    #[test]
    fn f4_opens_threaded_news_and_walks_its_panes() {
        let mut view = granted();
        assert_eq!(
            view.handle_key(key(KeyCode::F(4)), &users(), &files()),
            Action::ListThreadedNews
        );
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
        let _ = view.handle_key(key(KeyCode::F(4)), &users(), &files());
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
        tabs.push((Tab::Admin, Tab::ADMIN_LABEL.to_string()));
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
