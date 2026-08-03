//! Application state, decoupled from both the terminal (see `ui.rs`) and
//! the connection (see `main.rs`) so it's just data plus pure update
//! logic.

use hotline_client::HotlineClient;
use hotline_proto::field_id::ServerFieldId;
use hotline_proto::opcode::{Opcode, ServerOpcode};
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::{Field, Transaction};

/// One line in the scrollback: a chat message, a system notice (user
/// joined/left, a permission-denied notice, ...), or an error.
pub struct Line {
    pub text: String,
    pub kind: LineKind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Chat,
    System,
    Error,
}

pub struct App {
    pub nick: String,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Vec<Line>,
    pub input: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(nick: String, users: Vec<UserNameWithInfo>) -> Self {
        Self {
            nick,
            users,
            lines: vec![Line {
                text: "connected - type a message and press Enter, Esc to quit".to_string(),
                kind: LineKind::System,
            }],
            input: String::new(),
            should_quit: false,
        }
    }

    fn push(&mut self, text: impl Into<String>, kind: LineKind) {
        self.lines.push(Line {
            text: text.into(),
            kind,
        });
    }

    /// Apply one server-initiated transaction (chat relay, user list
    /// change, permission denial, ...) to the app state.
    pub fn apply_server_event(&mut self, tx: &Transaction) {
        let Opcode::Push(opcode) = tx.opcode() else {
            return;
        };
        match opcode {
            /* 104 */ ServerOpcode::Msg => self.apply_msg(tx),
            /* 106 */ ServerOpcode::Chat => self.apply_chat(tx),
            /* 111 */ ServerOpcode::Politequit => self.apply_politequit(tx),
            /* 113 */ ServerOpcode::ChatInvite => self.apply_chat_invite(tx),
            /* 301 */ ServerOpcode::UserChange => self.apply_user_change(tx),
            /* 302 */ ServerOpcode::UserPart => self.apply_user_part(tx),
            /* 354 */ ServerOpcode::UserSelfinfo => self.apply_user_selfinfo(tx),
            _ => {
                for line in format!("Unrecognized opcode for transaction: {tx:#?}").lines() {
                    self.push(line, LineKind::Error);
                }
            }
        }
    }

    // 104
    fn apply_msg(&mut self, tx: &Transaction) {
        let text = tx.field(ServerFieldId::Text).map(Field::as_string);
        let from = tx.field(ServerFieldId::Name).map(Field::as_string);
        match (from, text) {
            // A private message carries the sender's name; a
            // permission-denied or admin broadcast notice
            // doesn't.
            (Some(from), Some(text)) => {
                self.push(
                    format!("[PM] {from}: {}", display_text(&text)),
                    LineKind::Chat,
                );
            }
            (None, Some(text)) => {
                self.push(display_text(&text), LineKind::Error);
            }
            _ => {}
        }
    }

    // 106
    fn apply_chat(&mut self, tx: &Transaction) {
        if let Some(text) = tx.field(ServerFieldId::Text) {
            self.push(display_text(&text.as_string()), LineKind::Chat);
        }
    }

    // 111
    fn apply_politequit(&mut self, tx: &Transaction) {
        let reason = tx
            .field(ServerFieldId::Text)
            .map(Field::as_string)
            .unwrap_or_else(|| "disconnected by the server".to_string());
        self.push(display_text(&reason), LineKind::Error);
        self.quit();
    }

    // 113
    fn apply_chat_invite(&mut self, tx: &Transaction) {
        let from = tx
            .field(ServerFieldId::Name)
            .map(Field::as_string)
            .unwrap_or_else(|| "someone".to_string());
        let chat_id = tx.field(ServerFieldId::ChatId).and_then(Field::as_u16);
        self.push(
            format!(
                "{from} invited you to a private chat (id {})",
                chat_id.map(|id| id.to_string()).unwrap_or_default()
            ),
            LineKind::System,
        );
    }

    // 301
    fn apply_user_change(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(ServerFieldId::UserId).and_then(Field::as_u16) else {
            return;
        };
        let name = tx
            .field(ServerFieldId::Name)
            .map(Field::as_string)
            .unwrap_or_default();
        let icon = tx
            .field(ServerFieldId::Icon)
            .and_then(Field::as_u16)
            .unwrap_or_default();
        let flags = tx
            .field(ServerFieldId::Color)
            .and_then(Field::as_u16)
            .unwrap_or_default();

        match self.users.iter_mut().find(|u| u.user_id == user_id) {
            Some(existing) => {
                *existing = UserNameWithInfo {
                    user_id,
                    icon,
                    flags,
                    name,
                };
            }
            None => {
                self.push(format!("{name} joined"), LineKind::System);
                self.users.push(UserNameWithInfo {
                    user_id,
                    icon,
                    flags,
                    name,
                });
            }
        }
    }

    // 302
    fn apply_user_part(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(ServerFieldId::UserId).and_then(Field::as_u16) else {
            return;
        };
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
    }

    // 354
    fn apply_user_selfinfo(&mut self, tx: &Transaction) {
        let Some(access) = tx.field(ServerFieldId::Access).and_then(Field::as_u64) else {
            dbg!(tx);
            return;
        };
        todo!("0x{access:08X}");
    }

    /// Handle one input character.
    pub fn on_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn on_backspace(&mut self) {
        self.input.pop();
    }

    /// Take the current input line, clearing it, for the caller to send.
    pub fn take_input(&mut self) -> Option<String> {
        if self.input.is_empty() {
            return None;
        }
        Some(std::mem::take(&mut self.input))
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Send `line` as chat and report the outcome as a local (client-side)
    /// system/error line, since a failed send doesn't necessarily come back
    /// as a server push (a transport error, for instance), so this covers
    /// that case explicitly rather than relying solely on
    /// `apply_server_event`.
    pub async fn send_chat(&mut self, client: &HotlineClient, line: String) {
        if let Err(e) = client.send_chat(&line).await {
            self.push(format!("failed to send: {e}"), LineKind::Error);
        }
    }
}

/// Some servers prepend `\r` (or `\n`) to chat/status text so it starts
/// a fresh line in a client that appends everything into one continuous
/// scrollback. Each `Line` here is already its own row, so that control
/// character would just show up as a stray glyph instead of doing
/// anything.
fn display_text(s: &str) -> String {
    s.trim_start_matches(['\r', '\n']).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_strips_leading_cr() {
        assert_eq!(display_text("\rAlice: hi"), "Alice: hi");
    }

    #[test]
    fn display_text_leaves_normal_text_alone() {
        assert_eq!(display_text("Alice: hi"), "Alice: hi");
    }

    /// `113` is both `ClientOpcode::ChatInvite` and
    /// `ServerOpcode::ChatInvite`; a value-based (rather than
    /// direction-aware) opcode classification would misread this push as
    /// the client-side variant and silently drop it here.
    #[test]
    fn chat_invite_push_is_recognized_despite_sharing_its_wire_value_with_a_client_opcode() {
        use hotline_proto::field_id::ServerFieldId;
        use hotline_proto::opcode::ServerOpcode;

        let mut app = App::new("me".to_string(), Vec::new());
        let invite = Transaction::push(
            ServerOpcode::ChatInvite,
            vec![
                Field::string(ServerFieldId::Name, "Alice"),
                Field::u16(ServerFieldId::ChatId, 7),
            ],
        );
        app.apply_server_event(&invite);
        assert!(app.lines.iter().any(|line| line
            .text
            .contains("Alice invited you to a private chat (id 7)")));
    }

    #[test]
    fn apply_user_change_carries_admin_color_for_new_and_existing_users() {
        use hotline_proto::field_id::ServerFieldId;
        use hotline_proto::opcode::ServerOpcode;
        use hotline_proto::types::UserFlags;

        let mut app = App::new("me".to_string(), Vec::new());
        let admin_change = Transaction::push(
            ServerOpcode::UserChange,
            vec![
                Field::u16(ServerFieldId::UserId, 2),
                Field::u16(ServerFieldId::Icon, 0),
                Field::string(ServerFieldId::Name, "Root"),
                Field::u16(ServerFieldId::Color, UserFlags::ADMIN.bits()),
            ],
        );

        // New user: flags should carry the admin bit from the start.
        app.apply_server_event(&admin_change);
        let user = app.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(UserFlags::from_bits_truncate(user.flags).contains(UserFlags::ADMIN));

        // Existing user: a later update should still reflect current flags.
        let demoted = Transaction::push(
            ServerOpcode::UserChange,
            vec![
                Field::u16(ServerFieldId::UserId, 2),
                Field::u16(ServerFieldId::Icon, 0),
                Field::string(ServerFieldId::Name, "Root"),
                Field::u16(ServerFieldId::Color, 0),
            ],
        );
        app.apply_server_event(&demoted);
        let user = app.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(!UserFlags::from_bits_truncate(user.flags).contains(UserFlags::ADMIN));
    }
}
