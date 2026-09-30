//! [`View`]: the focus, selection, and modals. Pure: turns key presses and
//! mouse clicks into an [`Action`] for the caller to run.

mod action;
mod modal;

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use hotline_proto::types::UserNameWithInfo;
use ratatui::layout::Rect;

use crate::ui::{self, Areas};

pub use action::Action;
pub use modal::{MenuItem, Modal};

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
}

pub struct View {
    pub focus: Focus,
    pub modal: Option<Modal>,
    pub input: String,
    /// The first user list row on screen, as the last draw left it.
    pub users_offset: usize,
    selected_user: Option<u16>,
    last_click: Option<(u16, Instant)>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            focus: Focus::Input,
            modal: None,
            input: String::new(),
            users_offset: 0,
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

    /// The keys that work now, as `(key, what it does)`.
    pub const fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match (&self.modal, self.focus) {
            (Some(Modal::InstantMsg { .. }), _) => {
                &[("Enter", "send"), ("⇧Enter", "new line"), ("Esc", "cancel")]
            }
            (Some(Modal::UserMenu { .. }), _) => {
                &[("↑↓", "choose"), ("Enter", "select"), ("Esc", "close")]
            }
            (None, Focus::Users) => &[("↑↓", "select"), ("Enter", "message"), ("Tab", "input")],
            (None, Focus::Input) => &[
                ("Enter", "send"),
                ("⇧Enter", "new line"),
                ("Tab", "users"),
                ("Esc", "quit"),
            ],
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key, users);
        }
        match self.focus {
            Focus::Users => self.users_key(key, users),
            Focus::Input => self.input_key(key),
        }
    }

    fn modal_key(&mut self, mut modal: Modal, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        match (&mut modal, key.code) {
            (Modal::InstantMsg { input, .. }, _) if is_new_line(key) => input.push('\n'),
            (_, KeyCode::Esc) => return Action::None,
            (Modal::InstantMsg { user_id, input, .. }, KeyCode::Enter) if !input.is_empty() => {
                return Action::SendInstantMsg {
                    user_id: *user_id,
                    text: std::mem::take(input),
                };
            }
            (Modal::InstantMsg { input, .. }, KeyCode::Backspace) => {
                input.pop();
            }
            (Modal::InstantMsg { input, .. }, KeyCode::Char(c))
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
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

    fn users_key(&mut self, key: KeyEvent, users: &[UserNameWithInfo]) -> Action {
        match key.code {
            KeyCode::Tab | KeyCode::Esc => self.focus = Focus::Input,
            KeyCode::Up => self.move_selection(users, false),
            KeyCode::Down => self.move_selection(users, true),
            KeyCode::Enter => {
                if let Some(user_id) = self.selected_user(users) {
                    return self.choose(user_id, MenuItem::SendMessage, users);
                }
            }
            _ => {}
        }
        Action::None
    }

    fn input_key(&mut self, key: KeyEvent) -> Action {
        if is_new_line(key) {
            self.input.push('\n');
            return Action::None;
        }
        match key.code {
            KeyCode::Tab => self.focus = Focus::Users,
            KeyCode::Esc => return Action::Quit,
            KeyCode::Enter if !self.input.is_empty() => {
                return Action::SendChat(std::mem::take(&mut self.input));
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(c);
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
                Modal::InstantMsg { .. } => {}
            }
            return Action::None;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(user_id) = self.user_at(areas.users, x, y, users) {
                    return self.click_user(user_id, users, now);
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
            _ => {}
        }
        Action::None
    }

    /// Select `user_id`, or open a message to it on the second click of a double click.
    fn click_user(&mut self, user_id: u16, users: &[UserNameWithInfo], now: Instant) -> Action {
        self.selected_user = Some(user_id);
        self.focus = Focus::Users;
        let double = matches!(
            self.last_click,
            Some((last, at)) if last == user_id && now.duration_since(at) <= DOUBLE_CLICK
        );
        if double {
            self.last_click = None;
            return self.choose(user_id, MenuItem::SendMessage, users);
        }
        self.last_click = Some((user_id, now));
        Action::None
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

    /// The user on the list row at `x`, `y`.
    fn user_at(&self, list: Rect, x: u16, y: u16, users: &[UserNameWithInfo]) -> Option<u16> {
        let inner = ui::inner(list);
        if !contains(inner, x, y) {
            return None;
        }
        let index = self.users_offset + usize::from(y - inner.y);
        users.get(index).map(|u| u.user_id)
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

    fn areas() -> Areas {
        ui::areas(Rect::new(0, 0, 80, 24), "")
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
            let _ = view.handle_key(key(KeyCode::Char(c)), &users());
        }
    }

    #[test]
    fn a_line_starting_with_a_slash_is_sent_as_chat() {
        let mut view = View::default();
        type_text(&mut view, "/users");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users()),
            Action::SendChat("/users".into())
        );
    }

    #[test]
    fn shift_enter_starts_a_new_line() {
        let mut view = View::default();
        type_text(&mut view, "one");
        let shift_enter = KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT);
        assert_eq!(view.handle_key(shift_enter, &users()), Action::None);
        type_text(&mut view, "two");
        let ctrl_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL);
        assert_eq!(view.handle_key(ctrl_j, &users()), Action::None);
        type_text(&mut view, "three");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users()),
            Action::SendChat("one\ntwo\nthree".into())
        );
    }

    #[test]
    fn a_single_click_only_selects_a_user() {
        let mut view = View::default();
        let (x, y) = user_row(1);
        let left = click(MouseEventKind::Down(MouseButton::Left), x, y);
        let action = view.handle_mouse(left, &areas(), &users(), Instant::now());
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
        let _ = view.handle_mouse(left, &areas(), &users(), now);
        let _ = view.handle_mouse(left, &areas(), &users(), now + Duration::from_millis(200));
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
        let _ = view.handle_key(alt_enter, &users());
        type_text(&mut view, "there");
        assert_eq!(
            view.handle_key(key(KeyCode::Enter), &users()),
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
        let _ = view.handle_mouse(left, &areas(), &users(), now);
        let _ = view.handle_mouse(left, &areas(), &users(), now + Duration::from_secs(1));
        assert_eq!(view.modal, None);
    }

    #[test]
    fn a_click_on_a_menu_item_chooses_it() {
        let mut view = View::default();
        let (x, y) = user_row(0);
        let right = click(MouseEventKind::Down(MouseButton::Right), x, y);
        let _ = view.handle_mouse(right, &areas(), &users(), Instant::now());
        assert!(matches!(
            view.modal,
            Some(Modal::UserMenu { user_id: 1, .. })
        ));

        let menu = ui::menu_rect((x, y), areas().screen);
        let inner = ui::inner(menu);
        let left = click(MouseEventKind::Down(MouseButton::Left), inner.x, inner.y);
        let _ = view.handle_mouse(left, &areas(), &users(), Instant::now());
        assert!(matches!(
            view.modal,
            Some(Modal::InstantMsg { user_id: 1, .. })
        ));
    }

    #[test]
    fn hints_follow_the_focus() {
        let mut view = View::default();
        assert!(view.hints().contains(&("Enter", "send")));
        let _ = view.handle_key(key(KeyCode::Tab), &users());
        assert!(view.hints().contains(&("Enter", "message")));
    }
}
