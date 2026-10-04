//! [`View`]: the tab, focus, selection, and modals. Pure: turns key presses
//! and mouse clicks into an [`Action`] for the caller to run.

mod account_form;
mod action;
mod list_cursor;
mod modal;
mod options_form;
mod tab;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use hotline_client::Ban;
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use ratatui::layout::Rect;

use crate::file_edit::FileEdit;
use crate::session::NewsFolder;
use crate::transfer::{Conflict, Start};
use crate::ui::{self, Areas};

pub use account_form::{privileges, AccountForm};
pub use action::Action;
pub use list_cursor::ListCursor;
pub use modal::{MenuItem, Modal};
pub use options_form::{OptionsForm, OPTIONS};
pub use tab::Tab;

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

/// The pane that takes keys when no modal is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Users,
    /// The file list on the Files tab.
    Files,
    /// The item list on the Threaded News tab.
    NewsList,
    /// The account list on the Admin tab.
    Accounts,
}

impl Focus {
    /// The focus that leaves the user list on `tab`.
    const fn main(tab: Tab) -> Self {
        match tab {
            Tab::Chat | Tab::News | Tab::Room(_) => Self::Input,
            Tab::Files => Self::Files,
            Tab::Threaded => Self::NewsList,
            Tab::Admin => Self::Accounts,
        }
    }
}

/// The row that the first click of a double click hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clicked {
    User(u16),
    File(usize),
    News(usize),
    Account(usize),
}

pub struct View {
    pub tab: Tab,
    pub focus: Focus,
    pub modal: Option<Modal>,
    /// Each tab and its label, in tab bar order.
    pub tabs: Vec<(Tab, String)>,
    pub chat_input: String,
    pub post_input: String,
    /// The input line of each room tab.
    pub room_inputs: HashMap<u32, String>,
    /// Lines of news scrolled past the top.
    pub news_scroll: u16,
    /// The first user list row on screen, as the last draw left it.
    pub users_offset: usize,
    /// The cursor of the file list.
    pub files: ListCursor,
    /// The cursor of the threaded news list.
    pub news: ListCursor,
    /// The number of threaded news items listed.
    pub news_count: usize,
    /// The cursor of the account list.
    pub accounts: ListCursor,
    /// The number of accounts listed.
    pub account_count: usize,
    /// `true` if the user may send a broadcast.
    pub can_broadcast: bool,
    /// `true` if the user may disconnect users.
    pub can_disconnect: bool,
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
            chat_input: String::new(),
            post_input: String::new(),
            room_inputs: HashMap::new(),
            news_scroll: 0,
            users_offset: 0,
            files: ListCursor::default(),
            news: ListCursor::default(),
            news_count: 0,
            accounts: ListCursor::default(),
            account_count: 0,
            can_broadcast: false,
            can_disconnect: false,
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
    pub fn input(&self) -> Option<&str> {
        match self.tab {
            Tab::Chat => Some(&self.chat_input),
            Tab::News => Some(&self.post_input),
            Tab::Files | Tab::Threaded | Tab::Admin => None,
            Tab::Room(id) => Some(self.room_inputs.get(&id).map_or("", String::as_str)),
        }
    }

    fn input_mut(&mut self) -> Option<&mut String> {
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
        self.tabs = tabs;
        if !self.tabs.iter().any(|&(tab, _)| tab == self.tab) {
            let _ = self.set_tab(Tab::Chat);
        }
    }

    /// Show the room tab `chat_id`.
    pub fn show_room(&mut self, chat_id: u32) {
        let _ = self.set_tab(Tab::Room(chat_id));
    }

    /// The room tab after the current tab, or the first room tab.
    fn next_room(&self) -> Option<Tab> {
        let current = self.tabs.iter().position(|&(tab, _)| tab == self.tab);
        let mut rooms = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, (tab, _))| matches!(tab, Tab::Room(_)));
        let first = rooms.clone().next();
        rooms
            .find(|&(index, _)| current.is_some_and(|c| index > c))
            .or(first)
            .map(|(_, &(tab, _))| tab)
    }

    /// The keys that work now, as `(key, what it does)`.
    pub const fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match (&self.modal, self.focus, self.tab) {
            (Some(Modal::InstantMsg { .. } | Modal::Broadcast { .. }), ..) => {
                &[("Enter", "send"), ("⇧Enter", "new line"), ("Esc", "cancel")]
            }
            (Some(Modal::UserMenu { .. }), ..) => {
                &[("↑↓", "choose"), ("Enter", "select"), ("Esc", "close")]
            }
            (Some(Modal::Info { .. } | Modal::FileInfo(_)), ..) => &[("Esc", "close")],
            (Some(Modal::FileEdit { .. }), ..) => &[("Enter", "save"), ("Esc", "cancel")],
            (Some(Modal::Disconnect { .. }), ..) => &[
                ("d", "disconnect"),
                ("t", "ban for a while"),
                ("p", "ban for good"),
                ("Esc", "cancel"),
            ],
            (Some(Modal::Upload { .. }), ..) => &[("Enter", "upload"), ("Esc", "cancel")],
            (Some(Modal::SaveAs { .. }), ..) => &[("Enter", "save"), ("Esc", "cancel")],
            (
                Some(Modal::Confirm(
                    Conflict::PartialDownload { .. } | Conflict::LocalFolderExists { .. },
                )),
                ..,
            ) => &[("r", "resume"), ("o", "replace"), ("Esc", "cancel")],
            (Some(Modal::Confirm(_)), ..) => &[("r", "resume"), ("Esc", "cancel")],
            (Some(Modal::Invite { .. }), ..) => &[("Enter", "join"), ("Esc", "decline")],
            (Some(Modal::Subject { .. }), ..) => &[("Enter", "set"), ("Esc", "cancel")],
            (Some(Modal::PickDownload { .. }), ..) => {
                &[("↑↓", "choose"), ("Enter", "cancel it"), ("Esc", "close")]
            }
            (Some(Modal::PickUser { .. }), ..) => {
                &[("↑↓", "choose"), ("Enter", "invite"), ("Esc", "cancel")]
            }
            (Some(Modal::Article { .. }), ..) => &[("r", "reply"), ("Esc", "close")],
            (Some(Modal::Post { .. }), ..) => &[
                ("Tab", "title or text"),
                ("Enter", "post"),
                ("⇧Enter", "new line"),
                ("Esc", "cancel"),
            ],
            (Some(Modal::NewsName { .. }), ..) => &[("Enter", "create"), ("Esc", "cancel")],
            (
                Some(
                    Modal::DeleteNews { .. }
                    | Modal::DeleteAccount { .. }
                    | Modal::DeleteFile { .. },
                ),
                ..,
            ) => &[("y", "delete"), ("Esc", "cancel")],
            (Some(Modal::Account(_) | Modal::Options(_)), ..) => &[
                ("↑↓", "row"),
                ("Space", "toggle"),
                ("Enter", "save"),
                ("Esc", "cancel"),
            ],
            (Some(Modal::OpenAccount { .. }), ..) => &[("Enter", "open"), ("Esc", "cancel")],
            (None, Focus::Accounts, _) | (None, Focus::Input, Tab::Admin) => &[
                ("↑↓", "select"),
                ("Enter", "edit"),
                ("n", "new"),
                ("o", "open"),
                ("x", "delete"),
                ("Tab", "users"),
                ("Esc", "chat"),
            ],
            (None, Focus::Users, Tab::Admin) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "accounts"),
                ("^U", "chat"),
            ],
            (None, Focus::Users, Tab::Admin) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "accounts"),
                ("^U", "chat"),
            ],
            (None, Focus::NewsList, _) | (None, Focus::Input, Tab::Threaded) => &[
                ("↑↓", "select"),
                ("Enter", "open"),
                ("⌫", "up"),
                ("p", "post"),
                ("x", "delete"),
                ("b", "new bundle"),
                ("c", "new category"),
                ("Esc", "chat"),
            ],
            (None, Focus::Users, Tab::Threaded) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "news"),
                ("^B", "chat"),
            ],
            (None, Focus::Users, Tab::Threaded) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "news"),
                ("^B", "chat"),
            ],
            (None, Focus::Users, Tab::Chat) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "input"),
                ("^N", "news"),
                ("^F", "files"),
            ],
            (None, Focus::Users, Tab::Chat) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "input"),
                ("^N", "news"),
                ("^F", "files"),
            ],
            (None, Focus::Users, Tab::News) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "input"),
                ("^N", "chat"),
            ],
            (None, Focus::Users, Tab::News) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "input"),
                ("^N", "chat"),
            ],
            (None, Focus::Users, Tab::Files) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "files"),
                ("^F", "chat"),
            ],
            (None, Focus::Users, Tab::Files) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "files"),
                ("^F", "chat"),
            ],
            (None, Focus::Input, Tab::Chat) if self.can_broadcast => &[
                ("Enter", "send"),
                ("⇧Enter", "new line"),
                ("Tab", "users"),
                ("^N", "news"),
                ("^F", "files"),
                ("^B", "threaded news"),
                ("^G", "broadcast"),
                ("^K", "cancel download"),
                ("^O", "options"),
                ("Esc", "quit"),
            ],
            (None, Focus::Input, Tab::Chat) => &[
                ("Enter", "send"),
                ("⇧Enter", "new line"),
                ("Tab", "users"),
                ("^N", "news"),
                ("^F", "files"),
                ("^B", "threaded news"),
                ("^K", "cancel download"),
                ("^O", "options"),
                ("Esc", "quit"),
            ],
            (None, Focus::Files, _) | (None, Focus::Input, Tab::Files) => &[
                ("↑↓", "select"),
                ("Enter", "open or download"),
                ("⌫", "up"),
                ("d", "download"),
                ("u", "upload"),
                ("n", "new folder"),
                ("x", "delete"),
                ("i", "info"),
                ("r", "rename"),
                ("c", "comment"),
                ("m", "move"),
                ("a", "alias"),
                ("Tab", "users"),
                ("Esc", "chat"),
            ],
            (None, Focus::Users, Tab::Room(_)) if self.can_disconnect => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("k", "disconnect"),
                ("Tab", "input"),
                ("Esc", "input"),
            ],
            (None, Focus::Users, Tab::Room(_)) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "input"),
                ("Esc", "input"),
            ],
            (None, Focus::Input, Tab::Room(_)) => &[
                ("Enter", "send"),
                ("Tab", "users"),
                ("^A", "invite"),
                ("^T", "subject"),
                ("^W", "leave"),
                ("^R", "next room"),
                ("Esc", "chat"),
            ],
            (None, Focus::Input, Tab::News) => &[
                ("Enter", "post"),
                ("⇧Enter", "new line"),
                ("↑↓", "scroll"),
                ("Tab", "users"),
                ("^N", "chat"),
                ("Esc", "chat"),
            ],
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
            return Action::Quit;
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key, users);
        }
        if ctrl {
            let tab = match (key.code, self.tab) {
                (KeyCode::Char('n'), Tab::News)
                | (KeyCode::Char('f'), Tab::Files)
                | (KeyCode::Char('b'), Tab::Threaded)
                | (KeyCode::Char('u'), Tab::Admin) => Some(Tab::Chat),
                (KeyCode::Char('u'), _) if self.tabs.iter().any(|&(t, _)| t == Tab::Admin) => {
                    Some(Tab::Admin)
                }
                (KeyCode::Char('n'), _) => Some(Tab::News),
                (KeyCode::Char('f'), _) => Some(Tab::Files),
                (KeyCode::Char('b'), _) => Some(Tab::Threaded),
                (KeyCode::Char('r'), _) => self.next_room(),
                _ => None,
            };
            if let Some(tab) = tab {
                return self.set_tab(tab);
            }
            if key.code == KeyCode::Char('o') {
                return Action::EditOptions;
            }
            if key.code == KeyCode::Char('k') {
                return Action::PickDownload;
            }
            if key.code == KeyCode::Char('g') && self.can_broadcast {
                self.modal = Some(Modal::Broadcast {
                    input: String::new(),
                });
                return Action::None;
            }
            if let Tab::Room(chat_id) = self.tab {
                match key.code {
                    KeyCode::Char('w') => return Action::LeaveChat(chat_id),
                    KeyCode::Char('t') => return Action::EditChatSubject(chat_id),
                    KeyCode::Char('a') => return Action::PickInvitee(chat_id),
                    _ => {}
                }
            }
        }
        match self.focus {
            Focus::Users => self.users_key(key, users),
            Focus::Input => self.input_key(key),
            Focus::Files => self.files_key(key, files),
            Focus::NewsList => self.news_key(key),
            Focus::Accounts => self.accounts_key(key),
        }
    }

    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        if let Modal::Confirm(conflict) = modal {
            return self.confirm_key(conflict, key);
        }
        if let Modal::Account(form) = modal {
            return self.account_form_key(form, key);
        }
        if let Modal::Options(form) = modal {
            return self.options_form_key(form, key);
        }
        match (&mut modal, key.code) {
            (Modal::Article { id, title, .. }, KeyCode::Char('r')) => {
                let title = match title.strip_prefix("Re: ") {
                    Some(_) => title.to_string(),
                    None => format!("Re: {title}"),
                };
                self.modal = Some(Modal::Post {
                    parent_id: *id,
                    title,
                    body: String::new(),
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
                    text: std::mem::take(input),
                };
            }
            (Modal::DeleteNews { index, .. }, KeyCode::Char('y')) => {
                return Action::DeleteNewsItem(*index);
            }
            (Modal::DeleteAccount { login }, KeyCode::Char('y')) => {
                return Action::DeleteAccount(std::mem::take(login));
            }
            (Modal::OpenAccount { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::OpenAccount(std::mem::take(input));
            }
            (Modal::Post { body, on_body, .. }, _) if is_new_line(key) => {
                if *on_body {
                    body.push('\n');
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
                    title: std::mem::take(title),
                    text: std::mem::take(body),
                };
            }
            (Modal::NewsName { kind, input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::NewNewsItem {
                    name: std::mem::take(input),
                    kind: *kind,
                };
            }
            (Modal::Invite { chat_id, .. }, KeyCode::Enter) => return Action::JoinChat(*chat_id),
            (Modal::Invite { chat_id, .. }, KeyCode::Esc) => {
                return Action::RejectChatInvite(*chat_id);
            }
            (Modal::InstantMsg { input, .. } | Modal::Broadcast { input }, _)
                if is_new_line(key) =>
            {
                input.push('\n');
            }
            (_, KeyCode::Esc)
            | (Modal::Info { .. } | Modal::FileInfo(_) | Modal::Article { .. }, KeyCode::Enter) => {
                return Action::None;
            }
            (Modal::InstantMsg { user_id, input, .. }, KeyCode::Enter) if !input.is_empty() => {
                return Action::SendInstantMsg {
                    user_id: *user_id,
                    text: std::mem::take(input),
                };
            }
            (Modal::Broadcast { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::UserBroadcast(std::mem::take(input));
            }
            (Modal::Upload { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::Upload {
                    path: std::mem::take(input),
                    start: Start::Ask,
                };
            }
            (Modal::Subject { chat_id, input }, KeyCode::Enter) => {
                return Action::SetChatSubject {
                    chat_id: *chat_id,
                    subject: std::mem::take(input),
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
                    save_as: std::mem::take(input).into(),
                    start: Start::Ask,
                };
            }
            (modal, KeyCode::Backspace) => {
                if let Some(input) = modal.input_mut() {
                    input.pop();
                }
            }
            (modal, KeyCode::Char(c)) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(input) = modal.input_mut() {
                    input.push(c);
                }
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
            ) => {
                return self.choose(*user_id, items[*selected], users);
            }
            _ => {}
        }
        self.modal = Some(modal);
        Action::None
    }

    fn confirm_key(&mut self, conflict: Conflict, key: KeyEvent) -> Action {
        match key.code {
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
            KeyCode::Char('k') if self.can_disconnect => {
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
        let Some(input) = self.input_mut() else {
            return Action::None;
        };
        if is_new_line(key) {
            input.push('\n');
            return Action::None;
        }
        match (key.code, tab) {
            (KeyCode::Enter, Tab::Chat) if !input.is_empty() => {
                return Action::SendChat(std::mem::take(input));
            }
            (KeyCode::Enter, Tab::News) if !input.is_empty() => {
                return Action::OldPostNews(std::mem::take(input));
            }
            (KeyCode::Enter, Tab::Room(chat_id)) if !input.is_empty() => {
                return Action::SendRoomChat {
                    chat_id,
                    text: std::mem::take(input),
                };
            }
            (KeyCode::Backspace, _) => {
                input.pop();
            }
            (KeyCode::Char(c), _) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
            }
            (KeyCode::Tab, _) => self.focus = Focus::Users,
            (KeyCode::Esc, Tab::Chat) => return Action::Quit,
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
            KeyCode::Up => self.files.up(),
            KeyCode::Down => self.files.down(files.len()),
            KeyCode::Enter => return open_file(self.files.selected, files),
            KeyCode::Backspace | KeyCode::Left => return Action::ParentFolder,
            KeyCode::Char('d') => return download(self.files.selected, files),
            KeyCode::Char('u') => {
                self.modal = Some(Modal::Upload {
                    input: String::new(),
                });
            }
            KeyCode::Char('n') => {
                return Action::AskFileEdit {
                    edit: FileEdit::NewFolder,
                    name: Box::default(),
                };
            }
            KeyCode::Char(c @ ('x' | 'i' | 'r' | 'c' | 'm' | 'a')) => {
                let Some(file) = files.get(self.files.selected) else {
                    return Action::None;
                };
                let name = file.name.clone();
                let edit = match c {
                    'x' => {
                        self.modal = Some(Modal::DeleteFile { name });
                        return Action::None;
                    }
                    'i' => return Action::GetFileInfo(name),
                    'r' => FileEdit::Rename,
                    'c' => FileEdit::Comment,
                    'm' => FileEdit::Move,
                    _ => FileEdit::Alias,
                };
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
            KeyCode::Enter if !form.login.is_empty() => {
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
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(text) = form.text_mut() {
                    text.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = form.text_mut() {
                    text.pop();
                }
            }
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
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(text) = form.text_mut() {
                    text.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(text) = form.text_mut() {
                    text.pop();
                }
            }
            _ => {}
        }
        self.modal = Some(Modal::Options(form));
        Action::None
    }

    fn accounts_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.accounts.up(),
            KeyCode::Down => self.accounts.down(self.account_count),
            KeyCode::Enter if self.account_count > 0 => {
                return Action::EditAccount(self.accounts.selected);
            }
            KeyCode::Char('n') => self.modal = Some(Modal::Account(AccountForm::blank())),
            KeyCode::Char('o') => {
                self.modal = Some(Modal::OpenAccount {
                    input: String::new(),
                });
            }
            KeyCode::Char('x') if self.account_count > 0 => {
                return Action::AskDeleteAccount(self.accounts.selected);
            }
            _ => {}
        }
        Action::None
    }

    fn news_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.news.up(),
            KeyCode::Down => self.news.down(self.news_count),
            KeyCode::Enter if self.news_count > 0 => {
                return Action::OpenNewsItem(self.news.selected);
            }
            KeyCode::Backspace | KeyCode::Left => return Action::ParentNews,
            KeyCode::Char('p') => return Action::NewPost,
            KeyCode::Char('x') if self.news_count > 0 => {
                return Action::AskDeleteNews(self.news.selected);
            }
            KeyCode::Char(c @ ('b' | 'c')) => {
                self.modal = Some(Modal::NewsName {
                    kind: if c == 'c' {
                        NewsFolder::Category
                    } else {
                        NewsFolder::Bundle
                    },
                    input: String::new(),
                });
            }
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
                Modal::Info { .. } | Modal::FileInfo(_) => self.modal = None,
                Modal::InstantMsg { .. }
                | Modal::Disconnect { .. }
                | Modal::FileEdit { .. }
                | Modal::DeleteFile { .. }
                | Modal::Broadcast { .. }
                | Modal::Upload { .. }
                | Modal::SaveAs { .. }
                | Modal::Confirm(_)
                | Modal::Invite { .. }
                | Modal::Subject { .. }
                | Modal::PickUser { .. }
                | Modal::PickDownload { .. }
                | Modal::Post { .. }
                | Modal::NewsName { .. }
                | Modal::DeleteNews { .. }
                | Modal::Account(_)
                | Modal::Options(_)
                | Modal::OpenAccount { .. }
                | Modal::DeleteAccount { .. } => {}
                Modal::Article { .. } => self.modal = None,
            }
            return Action::None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(tab) = ui::tab_at(areas.tabs, x, y, &self.tabs) {
                    return self.set_tab(tab);
                }
                if let Some(user_id) = self.user_at(areas.users, x, y, users) {
                    return self.click_user(user_id, users, now);
                }
                if let Some(index) = self.file_at(areas.body, x, y, files) {
                    return self.click_file(index, files, now);
                }
                if let Some(index) = self.news_at(areas.body, x, y) {
                    return self.click_news(index, now);
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
                        items: MenuItem::items(self.can_disconnect),
                        selected: 0,
                    });
                }
            }
            MouseEventKind::ScrollUp if self.tab == Tab::News => self.scroll_news(-3),
            MouseEventKind::ScrollDown if self.tab == Tab::News => self.scroll_news(3),
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

    /// Select the file at `index`, or open it on the second click of a double click.
    fn click_file(&mut self, index: usize, files: &[FileNameWithInfo], now: Instant) -> Action {
        self.files.selected = index;
        self.focus = Focus::Files;
        if self.double_click(Clicked::File(index), now) {
            return open_file(index, files);
        }
        Action::None
    }

    /// Select the threaded news item at `index`, or open it on the second
    /// click of a double click.
    fn click_news(&mut self, index: usize, now: Instant) -> Action {
        self.news.selected = index;
        self.focus = Focus::NewsList;
        if self.double_click(Clicked::News(index), now) {
            return Action::OpenNewsItem(index);
        }
        Action::None
    }

    /// Select the account at `index`, or edit it on the second click of a
    /// double click.
    fn click_account(&mut self, index: usize, now: Instant) -> Action {
        self.accounts.selected = index;
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

    fn choose(&mut self, user_id: u16, item: MenuItem, users: &[UserNameWithInfo]) -> Action {
        match item {
            MenuItem::SendMessage => {
                if let Some(user) = users.iter().find(|u| u.user_id == user_id) {
                    self.modal = Some(Modal::InstantMsg {
                        user_id,
                        name: user.name.clone(),
                        input: String::new(),
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
        if self.focus != Focus::Users {
            self.focus = Focus::main(tab);
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
        self.selected_user = Some(users[next].user_id);
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
        self.files.row_at(y - inner.y, files.len())
    }

    /// The index of the account on the list row at `x`, `y` of the Admin tab.
    fn account_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Admin || !contains(inner, x, y) {
            return None;
        }
        self.accounts.row_at(y - inner.y, self.account_count)
    }

    /// The index of the item on the list row at `x`, `y` of the Threaded News tab.
    fn news_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Threaded || !contains(inner, x, y) {
            return None;
        }
        self.news.row_at(y - inner.y, self.news_count)
    }
}

/// Open the folder at `index`, or download the file there.
fn open_file(index: usize, files: &[FileNameWithInfo]) -> Action {
    match files.get(index) {
        Some(file) if file.is_folder() => Action::OpenFolder(file.name.clone()),
        Some(file) => download_file(file),
        None => Action::None,
    }
}

/// Download the file or folder at `index`.
fn download(index: usize, files: &[FileNameWithInfo]) -> Action {
    match files.get(index) {
        Some(file) if file.is_folder() => Action::DownloadFolder {
            name: file.name.clone(),
            start: Start::Ask,
        },
        Some(file) => download_file(file),
        None => Action::None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_client::{ClientUserInfo, Password, UserAccount};
    use hotline_proto::types::{UserChangeOptions, UserFlags};
    use hotline_proto::Access;

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
        ui::areas(Rect::new(0, 0, 80, 24), Some(""), &[])
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
        let mut view = View::default();
        type_text(&mut view, "/users");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::SendChat("/users".into())
        );
    }

    #[test]
    fn shift_enter_starts_a_new_line() {
        let mut view = View::default();
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
        let mut view = View::default();
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
                input: String::new()
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
        let mut view = View::default();
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
        let mut view = View::default();
        let (x, y) = user_row(0);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), &files(), Instant::now());

        let menu = ui::menu_rect((x, y), MenuItem::items(false), areas().screen);
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
    fn ctrl_g_broadcasts_only_with_the_privilege() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('g'), &users(), &files());
        assert_eq!(view.modal, None);

        view.can_broadcast = true;
        let _ = view.handle_key(ctrl('g'), &users(), &files());
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
        assert_eq!(
            view.handle_key(ctrl('o'), &users(), &files()),
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

        view.can_disconnect = true;
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
    fn the_user_menu_offers_disconnect_only_with_the_privilege() {
        let mut view = View {
            can_disconnect: true,
            ..View::default()
        };
        let (x, y) = user_row(1);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), &files(), Instant::now());
        for _ in 0..3 {
            let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        }
        let _ = view.handle_key(key(KeyCode::Enter), &users(), &files());
        assert!(matches!(
            view.modal,
            Some(Modal::Disconnect { user_id: 2, .. })
        ));
        assert_eq!(MenuItem::items(false).len(), 3);
    }

    #[test]
    fn file_keys_ask_for_text_info_or_a_delete_check() {
        let mut view = View::default();
        let _ = view.handle_key(ctrl('f'), &users(), &files());
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
    fn ctrl_k_asks_for_a_download_and_cancels_the_chosen_one() {
        let mut view = View::default();
        assert_eq!(
            view.handle_key(ctrl('k'), &users(), &files()),
            Action::PickDownload
        );
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
    fn ctrl_n_opens_the_news_tab_and_fetches_the_news() {
        let mut view = View::default();
        let ctrl_n = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL);
        assert_eq!(view.handle_key(ctrl_n, &users(), &files()), Action::GetMsgs);
        assert_eq!(view.tab, Tab::News);

        type_text(&mut view, "hello");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OldPostNews("hello".into())
        );
        assert_eq!(view.handle_key(ctrl_n, &users(), &files()), Action::None);
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn a_click_on_the_news_tab_fetches_the_news() {
        let mut view = View::default();
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

    #[test]
    fn hints_follow_the_focus() {
        let mut view = View::default();
        assert!(view.hints().contains(&("Enter", "send")));
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert!(view.hints().contains(&("Enter", "message")));
    }

    #[test]
    fn hints_show_disconnect_only_with_the_privilege() {
        let mut view = View::default();
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert!(!view.hints().contains(&("k", "disconnect")));
        view.can_disconnect = true;
        assert!(view.hints().contains(&("k", "disconnect")));
    }

    #[test]
    fn ctrl_f_opens_the_files_tab_and_walks_the_folders() {
        let mut view = View::default();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        assert_eq!(
            view.handle_key(ctrl_f, &users(), &files()),
            Action::ListFiles
        );
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

        assert_eq!(view.handle_key(ctrl_f, &users(), &files()), Action::None);
        assert_eq!((view.tab, view.focus), (Tab::Chat, Focus::Input));
    }

    #[test]
    fn tab_from_the_user_list_returns_to_the_file_list() {
        let mut view = View::default();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let _ = view.handle_key(ctrl_f, &users(), &files());
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::Users);
        let _ = view.handle_key(key(KeyCode::Tab), &users(), &files());
        assert_eq!(view.focus, Focus::Files);
    }

    #[test]
    fn a_double_click_on_a_folder_opens_it() {
        let mut view = View::default();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let _ = view.handle_key(ctrl_f, &users(), &files());
        let areas = ui::areas(Rect::new(0, 0, 80, 24), None, &[]);
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
        let mut view = View::default();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let _ = view.handle_key(ctrl_f, &users(), &files());
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
        let mut view = View::default();
        let ctrl_f = KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL);
        let _ = view.handle_key(ctrl_f, &users(), &files());
        let _ = view.handle_key(key(KeyCode::Char('u')), &users(), &files());
        assert_eq!(
            view.modal,
            Some(Modal::Upload {
                input: String::new()
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
        let mut view = View::default();
        let mut tabs = view.tabs.clone();
        tabs.push((Tab::Room(7), "Chat 7".to_string()));
        tabs.push((Tab::Room(9), "plans".to_string()));
        view.set_tabs(tabs);
        view
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
        assert_eq!(view.chat_input, "");
    }

    #[test]
    fn ctrl_r_steps_through_the_room_tabs() {
        let mut view = view_with_rooms();
        let _ = view.handle_key(ctrl('r'), &users(), &files());
        assert_eq!(view.tab, Tab::Room(7));
        let _ = view.handle_key(ctrl('r'), &users(), &files());
        assert_eq!(view.tab, Tab::Room(9));
        let _ = view.handle_key(ctrl('r'), &users(), &files());
        assert_eq!(view.tab, Tab::Room(7));
    }

    #[test]
    fn room_keys_leave_set_the_subject_and_invite() {
        let mut view = view_with_rooms();
        view.show_room(9);
        assert_eq!(
            view.handle_key(ctrl('w'), &users(), &files()),
            Action::LeaveChat(9)
        );
        assert_eq!(
            view.handle_key(ctrl('t'), &users(), &files()),
            Action::EditChatSubject(9)
        );
        assert_eq!(
            view.handle_key(ctrl('a'), &users(), &files()),
            Action::PickInvitee(9)
        );
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
            Some(Modal::Invite {
                chat_id: 7,
                from: "Alice".into(),
            })
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
                items: MenuItem::items(false),
                selected: 0,
            }),
            ..View::default()
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
    fn ctrl_b_opens_threaded_news_and_walks_its_items() {
        let mut view = View::default();
        assert_eq!(
            view.handle_key(ctrl('b'), &users(), &files()),
            Action::ListThreadedNews
        );
        assert_eq!((view.tab, view.focus), (Tab::Threaded, Focus::NewsList));
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::None
        );

        view.news_count = 2;
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users(), &files()),
            Action::OpenNewsItem(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('x')), &users(), &files()),
            Action::AskDeleteNews(1)
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Char('p')), &users(), &files()),
            Action::NewPost
        );
        assert_eq!(
            view.handle_key(key(KeyCode::Backspace), &users(), &files()),
            Action::ParentNews
        );
        assert_eq!(view.handle_key(ctrl('b'), &users(), &files()), Action::None);
        assert_eq!(view.tab, Tab::Chat);
    }

    #[test]
    fn a_reply_to_an_article_posts_its_title_and_text() {
        let mut view = View {
            modal: Some(Modal::Article {
                id: 4,
                title: "Hello".into(),
                poster: "Alice".into(),
                text: "hi".into(),
            }),
            ..View::default()
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
        let mut view = View::default();
        let _ = view.handle_key(ctrl('b'), &users(), &files());
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
            index: 3,
            name: "Gen".into(),
        });
        assert_eq!(
            view.handle_key(key(KeyCode::Char('y')), &users(), &files()),
            Action::DeleteNewsItem(3)
        );
    }

    #[test]
    fn ctrl_u_opens_the_admin_tab_only_when_it_is_shown() {
        let mut view = View::default();
        assert_eq!(view.handle_key(ctrl('u'), &users(), &files()), Action::None);
        assert_eq!(view.tab, Tab::Chat);

        let mut tabs = view.tabs.clone();
        tabs.push((Tab::Admin, Tab::ADMIN_LABEL.to_string()));
        view.set_tabs(tabs);
        assert_eq!(
            view.handle_key(ctrl('u'), &users(), &files()),
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
            ..View::default()
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
