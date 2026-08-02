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
            ServerOpcode::Chat => {
                if let Some(text) = tx.field(ServerFieldId::Text) {
                    self.push(display_text(&text.as_string()), LineKind::Chat);
                }
            }
            ServerOpcode::Msg => {
                let text = tx.field(ServerFieldId::Text).map(|f| f.as_string());
                let from = tx.field(ServerFieldId::Name).map(|f| f.as_string());
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
            ServerOpcode::Politequit => {
                let reason = tx
                    .field(ServerFieldId::Text)
                    .map(|f| f.as_string())
                    .unwrap_or_else(|| "disconnected by the server".to_string());
                self.push(display_text(&reason), LineKind::Error);
                self.quit();
            }
            ServerOpcode::ChatInvite => {
                let from = tx
                    .field(ServerFieldId::Name)
                    .map(|f| f.as_string())
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
            ServerOpcode::UserChange => self.apply_user_change(tx),
            ServerOpcode::UserPart => self.apply_user_delete(tx),
            _ => {}
        }
    }

    fn apply_user_change(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(ServerFieldId::UserId).and_then(|f| f.as_integer()) else {
            return;
        };
        let user_id = user_id as u16;
        let name = tx
            .field(ServerFieldId::Name)
            .map(|f| f.as_string())
            .unwrap_or_default();
        let icon_id = tx
            .field(ServerFieldId::Icon)
            .and_then(|f| f.as_integer())
            .unwrap_or(0) as u16;

        let is_new = !self.users.iter().any(|u| u.user_id == user_id);
        if let Some(existing) = self.users.iter_mut().find(|u| u.user_id == user_id) {
            existing.name = name.clone();
            existing.icon_id = icon_id;
        } else {
            self.users.push(UserNameWithInfo {
                user_id,
                icon_id,
                flags: 0,
                name: name.clone(),
            });
        }
        if is_new {
            self.push(format!("{name} joined"), LineKind::System);
        }
    }

    fn apply_user_delete(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(ServerFieldId::UserId).and_then(|f| f.as_integer()) else {
            return;
        };
        let user_id = user_id as u16;
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
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
}

/// Send `line` as chat and report the outcome as a local (client-side)
/// system/error line - a failed send doesn't necessarily come back as a
/// server push (a transport error, for instance), so this covers that
/// case explicitly rather than relying solely on `apply_server_event`.
pub async fn send_chat(app: &mut App, client: &HotlineClient, line: String) {
    if let Err(e) = client.send_chat(&line).await {
        app.push(format!("failed to send: {e}"), LineKind::Error);
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
}
