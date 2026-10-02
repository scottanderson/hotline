//! [`View`]: the tab, focus, selection, and modals. Pure: turns key presses
//! and mouse clicks into an [`Action`] for the caller to run.

mod action;
mod modal;
mod tab;

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use hotline_proto::types::{FileNameWithInfo, UserNameWithInfo};
use ratatui::layout::Rect;

use crate::transfer::{Conflict, Start};
use crate::ui::{self, Areas};

pub use action::Action;
pub use modal::{MenuItem, Modal};
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
}

impl Focus {
    /// The focus that leaves the user list on `tab`.
    const fn main(tab: Tab) -> Self {
        match tab {
            Tab::Chat | Tab::News | Tab::Room(_) => Self::Input,
            Tab::Files => Self::Files,
            Tab::Threaded => Self::NewsList,
        }
    }
}

/// The row that the first click of a double click hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clicked {
    User(u16),
    File(usize),
    News(usize),
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
    /// The first file list row on screen, as the last draw left it.
    pub files_offset: usize,
    /// The index of the selected file in the listing.
    pub selected_file: usize,
    /// The first threaded news row on screen, as the last draw left it.
    pub news_offset: usize,
    /// The index of the selected threaded news item.
    pub selected_news: usize,
    /// The number of threaded news items listed.
    pub news_count: usize,
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
            files_offset: 0,
            selected_file: 0,
            news_offset: 0,
            selected_news: 0,
            news_count: 0,
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
            Tab::Files | Tab::Threaded => None,
            Tab::Room(id) => Some(self.room_inputs.get(&id).map_or("", String::as_str)),
        }
    }

    fn input_mut(&mut self) -> Option<&mut String> {
        match self.tab {
            Tab::Chat => Some(&mut self.chat_input),
            Tab::News => Some(&mut self.post_input),
            Tab::Files | Tab::Threaded => None,
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

    /// Select the first file and scroll to the top of the list.
    pub const fn reset_file_selection(&mut self) {
        self.selected_file = 0;
        self.files_offset = 0;
    }

    /// Select the first threaded news item and scroll to the top of the list.
    pub const fn reset_news_selection(&mut self) {
        self.selected_news = 0;
        self.news_offset = 0;
    }

    /// The keys that work now, as `(key, what it does)`.
    pub const fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match (&self.modal, self.focus, self.tab) {
            (Some(Modal::InstantMsg { .. }), ..) => {
                &[("Enter", "send"), ("⇧Enter", "new line"), ("Esc", "cancel")]
            }
            (Some(Modal::UserMenu { .. }), ..) => {
                &[("↑↓", "choose"), ("Enter", "select"), ("Esc", "close")]
            }
            (Some(Modal::Info { .. }), ..) => &[("Esc", "close")],
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
            (Some(Modal::DeleteNews { .. }), ..) => &[("y", "delete"), ("Esc", "cancel")],
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
            (None, Focus::Users, Tab::Threaded) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "news"),
                ("^B", "chat"),
            ],
            (None, Focus::Users, Tab::Chat) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "input"),
                ("^N", "news"),
                ("^F", "files"),
            ],
            (None, Focus::Users, Tab::News) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "input"),
                ("^N", "chat"),
            ],
            (None, Focus::Users, Tab::Files) => &[
                ("↑↓", "select"),
                ("Enter", "message"),
                ("i", "info"),
                ("Tab", "files"),
                ("^F", "chat"),
            ],
            (None, Focus::Input, Tab::Chat) => &[
                ("Enter", "send"),
                ("⇧Enter", "new line"),
                ("Tab", "users"),
                ("^N", "news"),
                ("^F", "files"),
                ("^B", "threaded news"),
                ("Esc", "quit"),
            ],
            (None, Focus::Files, _) | (None, Focus::Input, Tab::Files) => &[
                ("↑↓", "select"),
                ("Enter", "open or download"),
                ("⌫", "up"),
                ("d", "download"),
                ("u", "upload"),
                ("Tab", "users"),
                ("Esc", "chat"),
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
                | (KeyCode::Char('b'), Tab::Threaded) => Some(Tab::Chat),
                (KeyCode::Char('n'), _) => Some(Tab::News),
                (KeyCode::Char('f'), _) => Some(Tab::Files),
                (KeyCode::Char('b'), _) => Some(Tab::Threaded),
                (KeyCode::Char('r'), _) => self.next_room(),
                _ => None,
            };
            if let Some(tab) = tab {
                return self.set_tab(tab);
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
        }
    }

    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        if let Modal::Confirm(conflict) = modal {
            return self.confirm_key(conflict, key);
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
            (Modal::DeleteNews { index, .. }, KeyCode::Char('y')) => {
                return Action::DeleteNewsItem(*index);
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
            (
                Modal::Post {
                    title,
                    body,
                    on_body,
                    ..
                },
                KeyCode::Backspace,
            ) => {
                if *on_body { body } else { title }.pop();
            }
            (
                Modal::Post {
                    title,
                    body,
                    on_body,
                    ..
                },
                KeyCode::Char(c),
            ) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if *on_body { body } else { title }.push(c);
            }
            (Modal::NewsName { category, input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::NewNewsItem {
                    name: std::mem::take(input),
                    category: *category,
                };
            }
            (Modal::Invite { chat_id, .. }, KeyCode::Enter) => return Action::JoinChat(*chat_id),
            (Modal::Invite { chat_id, .. }, KeyCode::Esc) => {
                return Action::RejectChatInvite(*chat_id);
            }
            (Modal::InstantMsg { input, .. }, _) if is_new_line(key) => input.push('\n'),
            (_, KeyCode::Esc) | (Modal::Info { .. } | Modal::Article { .. }, KeyCode::Enter) => {
                return Action::None;
            }
            (Modal::InstantMsg { user_id, input, .. }, KeyCode::Enter) if !input.is_empty() => {
                return Action::SendInstantMsg {
                    user_id: *user_id,
                    text: std::mem::take(input),
                };
            }
            (Modal::Upload { input }, KeyCode::Enter) if !input.is_empty() => {
                return Action::Upload {
                    path: std::mem::take(input),
                    start: Start::New,
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
            (Modal::PickUser { selected, .. }, KeyCode::Up) => {
                *selected = selected.saturating_sub(1);
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
                    start: Start::New,
                };
            }
            (
                Modal::InstantMsg { input, .. }
                | Modal::Upload { input }
                | Modal::SaveAs { input, .. }
                | Modal::Subject { input, .. }
                | Modal::NewsName { input, .. },
                KeyCode::Backspace,
            ) => {
                input.pop();
            }
            (
                Modal::InstantMsg { input, .. }
                | Modal::Upload { input }
                | Modal::SaveAs { input, .. }
                | Modal::Subject { input, .. }
                | Modal::NewsName { input, .. },
                KeyCode::Char(c),
            ) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
            }
            (Modal::UserMenu { selected, .. }, KeyCode::Up) => {
                *selected = selected.saturating_sub(1);
            }
            (Modal::UserMenu { selected, .. }, KeyCode::Down) => {
                *selected = (*selected + 1).min(MenuItem::ALL.len() - 1);
            }
            (
                Modal::UserMenu {
                    user_id, selected, ..
                },
                KeyCode::Enter,
            ) => {
                return self.choose(*user_id, MenuItem::ALL[*selected], users);
            }
            _ => {}
        }
        self.modal = Some(modal);
        Action::None
    }

    fn confirm_key(&mut self, conflict: Conflict, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('r') => conflict.action(Start::Resume),
            KeyCode::Char('o') if conflict.can_replace() => conflict.action(Start::Replace),
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
            KeyCode::Up => self.selected_file = self.selected_file.saturating_sub(1),
            KeyCode::Down => {
                self.selected_file = (self.selected_file + 1).min(files.len().saturating_sub(1));
            }
            KeyCode::Enter => return open_file(self.selected_file, files),
            KeyCode::Backspace | KeyCode::Left => return Action::ParentFolder,
            KeyCode::Char('d') => return download(self.selected_file, files),
            KeyCode::Char('u') => {
                self.modal = Some(Modal::Upload {
                    input: String::new(),
                });
            }
            _ => {}
        }
        Action::None
    }

    fn news_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return self.set_tab(Tab::Chat),
            KeyCode::Up => self.selected_news = self.selected_news.saturating_sub(1),
            KeyCode::Down => {
                self.selected_news =
                    (self.selected_news + 1).min(self.news_count.saturating_sub(1));
            }
            KeyCode::Enter if self.news_count > 0 => {
                return Action::OpenNewsItem(self.selected_news);
            }
            KeyCode::Backspace | KeyCode::Left => return Action::ParentNews,
            KeyCode::Char('p') => return Action::NewPost,
            KeyCode::Char('x') if self.news_count > 0 => {
                return Action::AskDeleteNews(self.selected_news);
            }
            KeyCode::Char(c @ ('b' | 'c')) => {
                self.modal = Some(Modal::NewsName {
                    category: c == 'c',
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
                Modal::UserMenu { user_id, at, .. } => {
                    let user_id = *user_id;
                    let item = ui::menu_item_at(ui::menu_rect(*at, areas.screen), x, y);
                    self.modal = None;
                    if let (MouseButton::Left, Some(item)) = (button, item) {
                        return self.choose(user_id, item, users);
                    }
                }
                Modal::Info { .. } => self.modal = None,
                Modal::InstantMsg { .. }
                | Modal::Upload { .. }
                | Modal::SaveAs { .. }
                | Modal::Confirm(_)
                | Modal::Invite { .. }
                | Modal::Subject { .. }
                | Modal::PickUser { .. }
                | Modal::Post { .. }
                | Modal::NewsName { .. }
                | Modal::DeleteNews { .. } => {}
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
        self.selected_file = index;
        self.focus = Focus::Files;
        if self.double_click(Clicked::File(index), now) {
            return open_file(index, files);
        }
        Action::None
    }

    /// Select the threaded news item at `index`, or open it on the second
    /// click of a double click.
    fn click_news(&mut self, index: usize, now: Instant) -> Action {
        self.selected_news = index;
        self.focus = Focus::NewsList;
        if self.double_click(Clicked::News(index), now) {
            return Action::OpenNewsItem(index);
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
        let index = self.files_offset + usize::from(y - inner.y);
        (index < files.len()).then_some(index)
    }

    /// The index of the item on the list row at `x`, `y` of the Threaded News tab.
    fn news_at(&self, body: Rect, x: u16, y: u16) -> Option<usize> {
        let inner = ui::inner(body);
        if self.tab != Tab::Threaded || !contains(inner, x, y) {
            return None;
        }
        let index = self.news_offset + usize::from(y - inner.y);
        (index < self.news_count).then_some(index)
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
            start: Start::New,
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
        start: Start::New,
    }
}

pub fn contains(rect: Rect, x: u16, y: u16) -> bool {
    (rect.left()..rect.right()).contains(&x) && (rect.top()..rect.bottom()).contains(&y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::types::UserFlags;

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
        ui::areas(Rect::new(0, 0, 80, 24), Some(""))
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

        let menu = ui::menu_rect((x, y), areas().screen);
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
                start: Start::New,
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
        let areas = ui::areas(Rect::new(0, 0, 80, 24), None);
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
                start: Start::New,
            }
        );
        let _ = view.handle_key(key(KeyCode::Down), &users(), &files());
        assert_eq!(
            view.handle_key(key(KeyCode::Char('d')), &users(), &files()),
            Action::DownloadFile {
                name: "readme.txt".into(),
                save_as: "readme.txt".into(),
                start: Start::New,
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
                start: Start::New,
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
            conflict.clone().action(Start::Replace)
        );

        view.modal = Some(Modal::ask(conflict.clone()));
        assert_eq!(
            view.handle_key(key(KeyCode::Char('r')), &users(), &files()),
            conflict.action(Start::Resume)
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
                start: Start::New,
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
                category: true,
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
}
