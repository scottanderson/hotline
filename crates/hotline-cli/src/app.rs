//! Application state, decoupled from both the terminal (see `ui.rs`) and
//! the connection (see `main.rs`) so it's just data plus pure update
//! logic.

use hotline_client::HotlineClient;
use hotline_proto::field_id::FieldId;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::EncodingHandle;
use hotline_proto::types::UserNameWithInfo;
use hotline_proto::{Access, Field, Transaction};

/// Where the UI is: still showing the server's agreement, or fully
/// logged in and chatting.
pub enum Mode {
    Joining,
    Agreement(String),
    Chat,
}

/// What the caller must do after applying one server event.
#[must_use]
pub enum EventOutcome {
    /// State updated; nothing further needed.
    Applied,
    /// Nothing to show for the agreement; the caller should call
    /// [`App::accept_agreement`] right away to complete login.
    NeedsAgreementAccept,
}

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
    pub icon: u16,
    pub mode: Mode,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Vec<Line>,
    pub input: String,
    pub should_quit: bool,
    pub access: Access,
    /// This connection's active text encoding, shared with the client's codec.
    encoding: EncodingHandle,
}

impl App {
    pub fn new(nick: String, icon: u16, encoding: EncodingHandle) -> Self {
        Self {
            nick,
            icon,
            mode: Mode::Joining,
            users: Vec::new(),
            lines: vec![Line {
                text: "connected - type a message and press Enter, Esc to quit".to_string(),
                kind: LineKind::System,
            }],
            input: String::new(),
            should_quit: false,
            access: Access::empty(),
            encoding,
        }
    }

    /// Show the server's agreement text and wait for the user to accept it.
    pub fn show_agreement(&mut self, text: String) {
        self.mode = Mode::Agreement(text);
    }

    /// Accept the agreement (`myTran_Agreed`, 121) and switch to chat.
    pub async fn accept_agreement(&mut self, client: &HotlineClient) {
        if let Err(e) = client.agree(&self.nick, self.icon).await {
            self.push(format!("failed to complete login: {e}"), LineKind::Error);
        }
        match client.get_user_name_list().await {
            Ok(users) => self.users = users,
            Err(e) => self.push(format!("failed to fetch user list: {e}"), LineKind::Error),
        }
        self.mode = Mode::Chat;
    }

    fn push(&mut self, text: impl Into<String>, kind: LineKind) {
        self.lines.push(Line {
            text: text.into(),
            kind,
        });
    }

    /// Apply one server-initiated transaction (chat relay, user list
    /// change, permission denial, ...) to the app state.
    pub fn apply_server_event(&mut self, tx: &Transaction) -> EventOutcome {
        let TransactionType::Push(opcode) = tx.r#type() else {
            return EventOutcome::Applied;
        };
        match opcode {
            /* 104 */ Opcode::ServerMsg => self.apply_msg(tx),
            /* 106 */ Opcode::ChatMsg => self.apply_chat(tx),
            /* 109 */ Opcode::Agreement => return self.apply_agreement(tx),
            /* 111 */ Opcode::Politequit => self.apply_politequit(tx),
            /* 113 */ Opcode::ChatInvite => self.apply_chat_invite(tx),
            /* 301 */ Opcode::NotifyChangeUser => self.apply_user_change(tx),
            /* 302 */ Opcode::UserPart => self.apply_user_part(tx),
            /* 354 */ Opcode::UserSelfinfo => self.apply_user_selfinfo(tx),
            _ => {
                for line in format!("Unrecognized opcode for transaction: {tx:#?}").lines() {
                    self.push(line, LineKind::Error);
                }
            }
        }
        EventOutcome::Applied
    }

    // 109
    //
    // Text means show it and wait for acceptance; no text means there's
    // nothing to accept, so the caller should accept right away.
    fn apply_agreement(&mut self, tx: &Transaction) -> EventOutcome {
        let encoding = self.encoding.get();
        match tx.string(FieldId::Text, encoding) {
            Some(text) => {
                self.show_agreement(text);
                EventOutcome::Applied
            }
            None => EventOutcome::NeedsAgreementAccept,
        }
    }

    // 104
    fn apply_msg(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let text = tx.string(FieldId::Text, encoding);
        let from = tx.string(FieldId::Name, encoding);
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
        let encoding = self.encoding.get();
        if let Some(text) = tx.string(FieldId::Text, encoding) {
            self.push(display_text(&text), LineKind::Chat);
        }
    }

    // 111
    fn apply_politequit(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let reason = tx
            .string(FieldId::Text, encoding)
            .unwrap_or_else(|| "disconnected by the server".to_string());
        self.push(display_text(&reason), LineKind::Error);
        self.quit();
    }

    // 113
    fn apply_chat_invite(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let from = tx
            .string(FieldId::Name, encoding)
            .unwrap_or_else(|| "someone".to_string());
        let chat_id = tx.field(FieldId::ChatId).and_then(Field::as_u32);
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
        let Some(user_id) = tx.field(FieldId::UserId).and_then(Field::as_u16) else {
            return;
        };
        let encoding = self.encoding.get();
        let name = tx.string(FieldId::Name, encoding).unwrap_or_default();
        let icon = tx
            .field(FieldId::Icon)
            .and_then(Field::as_u16)
            .unwrap_or_default();
        let flags = tx
            .field(FieldId::Color)
            .and_then(Field::as_u16)
            .unwrap_or_default();

        match self.users.iter().position(|u| u.user_id == user_id) {
            Some(pos) => {
                let old_name = self.users[pos].name.clone();
                if !old_name.is_empty() && !name.is_empty() && old_name != name {
                    self.push(
                        format!(" <<< {old_name} is now known as {name} >>>"),
                        LineKind::System,
                    );
                }
                self.users[pos] = UserNameWithInfo {
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
        let Some(user_id) = tx.field(FieldId::UserId).and_then(Field::as_u16) else {
            return;
        };
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
    }

    // 354
    fn apply_user_selfinfo(&mut self, tx: &Transaction) {
        self.access = tx.access().unwrap_or(self.access);

        // This session's own uid/icon/name.
        let encoding = self.encoding.get();
        if let Some(result) = tx.user_list_entry(encoding) {
            match result {
                Ok(entry) => match self.users.iter_mut().find(|u| u.user_id == entry.user_id) {
                    Some(existing) => *existing = entry,
                    None => self.users.push(entry),
                },
                Err(e) => self.push(
                    format!("failed to parse own user info: {e}"),
                    LineKind::Error,
                ),
            }
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

    /// Send `line` as chat, reporting a failed send as a local error line
    /// (a transport error may never come back as a server push).
    pub async fn send_chat(&mut self, client: &HotlineClient, line: String) {
        if let Err(e) = client.send_chat(&line).await {
            self.push(format!("failed to send: {e}"), LineKind::Error);
        }
    }
}

/// Strip a leading `\r`/`\n` some servers prepend to chat/status text;
/// each `Line` here is already its own row.
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

    /// `113` (`myTran_InviteToChat`) is both the client's invite request
    /// and the server's resulting push; a value-based classification
    /// would misread this push and silently drop it here.
    #[test]
    fn chat_invite_push_is_recognized_despite_sharing_its_wire_value_with_a_client_opcode() {
        use hotline_proto::field_id::FieldId;
        use hotline_proto::opcode::Opcode;

        let mut app = App::new("me".to_string(), 0, EncodingHandle::default());
        let invite = Transaction::push(
            Opcode::ChatInvite,
            vec![
                Field::string(FieldId::Name, "Alice"),
                Field::u32(FieldId::ChatId, 7),
            ],
        );
        let _ = app.apply_server_event(&invite);
        assert!(app.lines.iter().any(|line| line
            .text
            .contains("Alice invited you to a private chat (id 7)")));
    }

    #[test]
    fn apply_user_change_carries_admin_color_for_new_and_existing_users() {
        use hotline_proto::field_id::FieldId;
        use hotline_proto::opcode::Opcode;
        use hotline_proto::types::UserFlags;

        let mut app = App::new("me".to_string(), 0, EncodingHandle::default());
        let admin_change = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserId, 2),
                Field::u16(FieldId::Icon, 0),
                Field::string(FieldId::Name, "Root"),
                Field::u16(FieldId::Color, UserFlags::ADMIN.bits()),
            ],
        );

        // New user: flags should carry the admin bit from the start.
        let _ = app.apply_server_event(&admin_change);
        let user = app.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(UserFlags::from_bits_truncate(user.flags).contains(UserFlags::ADMIN));

        // Existing user: a later update should still reflect current flags.
        let demoted = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserId, 2),
                Field::u16(FieldId::Icon, 0),
                Field::string(FieldId::Name, "Root"),
                Field::u16(FieldId::Color, 0),
            ],
        );
        let _ = app.apply_server_event(&demoted);
        let user = app.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(!UserFlags::from_bits_truncate(user.flags).contains(UserFlags::ADMIN));
    }

    #[test]
    fn apply_user_change_notes_a_rename() {
        use hotline_proto::field_id::FieldId;
        use hotline_proto::opcode::Opcode;

        let mut app = App::new("me".to_string(), 0, EncodingHandle::default());
        let joined = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserId, 2),
                Field::u16(FieldId::Icon, 0),
                Field::string(FieldId::Name, "Alice"),
                Field::u16(FieldId::Color, 0),
            ],
        );
        let _ = app.apply_server_event(&joined);

        let renamed = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserId, 2),
                Field::u16(FieldId::Icon, 0),
                Field::string(FieldId::Name, "Alicia"),
                Field::u16(FieldId::Color, 0),
            ],
        );
        let _ = app.apply_server_event(&renamed);

        assert_eq!(
            app.lines.last().unwrap().text,
            " <<< Alice is now known as Alicia >>>"
        );
        let user = app.users.iter().find(|u| u.user_id == 2).unwrap();
        assert_eq!(user.name, "Alicia");
    }
}
