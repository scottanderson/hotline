//! [`ClientSession`] state: what the client knows about the connection,
//! decoupled from the terminal. Data plus pure update logic.

mod event_outcome;
mod mode;

use hotline_client::HotlineClient;
use hotline_proto::opcode::{Opcode, TransactionType};
use hotline_proto::text_encoding::EncodingHandle;
use hotline_proto::types::{UserFlags, UserNameWithInfo};
use hotline_proto::FieldId;
use hotline_proto::{Access, Field, Transaction};

use crate::line::{Line, LineKind};

pub use event_outcome::EventOutcome;
pub use mode::Mode;

pub struct ClientSession {
    pub nick: Box<str>,
    pub icon: u16,
    pub mode: Mode,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Vec<Line>,
    pub access: Access,
    /// This connection's active text encoding, shared with the client's codec.
    encoding: EncodingHandle,
}

impl ClientSession {
    pub fn new(nick: String, icon: u16, encoding: EncodingHandle) -> Self {
        Self {
            nick: nick.into(),
            icon,
            mode: Mode::Joining,
            users: Vec::new(),
            lines: vec![Line {
                text: "connected - type a message and press Enter, Esc to quit".into(),
                kind: LineKind::System,
            }],
            access: Access::empty(),
            encoding,
        }
    }

    /// Show the server's agreement text and wait for the user to accept it.
    pub fn show_agreement(&mut self, text: String) {
        self.mode = Mode::Agreement(text);
    }

    /// Accept the agreement, [`myTran_Agreed`] (121), and switch to chat.
    ///
    /// [`myTran_Agreed`]: hotline_proto::Opcode::Agreed
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

    fn push(&mut self, text: impl Into<Box<str>>, kind: LineKind) {
        self.lines.push(Line {
            text: text.into(),
            kind,
        });
    }

    /// Push one line for each `\r` in `text`.
    fn push_lines(&mut self, text: &str, kind: LineKind) {
        for line in text.split('\r') {
            self.push(line, kind);
        }
    }

    /// Apply one server-initiated transaction (chat relay, user list
    /// change, permission denial, ...) to the session state.
    pub fn apply_server_event(&mut self, tx: &Transaction) -> EventOutcome {
        let TransactionType::Push(opcode) = tx.r#type() else {
            return EventOutcome::Applied;
        };
        match opcode {
            /* 104 */ Opcode::ServerMsg => self.apply_server_msg(tx),
            /* 106 */ Opcode::ChatMsg => self.apply_chat_msg(tx),
            /* 109 */ Opcode::ShowAgreement => return self.apply_show_agreement(tx),
            /* 111 */ Opcode::DisconnectMsg => return self.apply_disconnect_msg(tx),
            /* 113 */ Opcode::InviteToChat => self.apply_invite_to_chat(tx),
            /* 301 */ Opcode::NotifyChangeUser => self.apply_notify_change_user(tx),
            /* 302 */ Opcode::NotifyDeleteUser => self.apply_notify_delete_user(tx),
            /* 354 */ Opcode::UserAccess => self.apply_user_access(tx),
            _ => {
                for line in format!("Unrecognized opcode for transaction: {tx:#?}").lines() {
                    self.push(line, LineKind::Error);
                }
            }
        }
        EventOutcome::Applied
    }

    /// [`myTran_ShowAgreement`] (109): show the text and wait for acceptance.
    /// With no text, the caller accepts right away.
    ///
    /// [`myTran_ShowAgreement`]: hotline_proto::Opcode::ShowAgreement
    fn apply_show_agreement(&mut self, tx: &Transaction) -> EventOutcome {
        let encoding = self.encoding.get();
        match tx.string(FieldId::Data, encoding) {
            Some(text) => {
                self.show_agreement(text);
                EventOutcome::Applied
            }
            None => EventOutcome::NeedsAgreementAccept,
        }
    }

    /// [`myTran_ServerMsg`] (104).
    ///
    /// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
    fn apply_server_msg(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let text = tx.string(FieldId::Data, encoding);
        let from = tx.string(FieldId::UserName, encoding);
        match (from, text) {
            // A private message carries the sender's name; a
            // permission-denied or admin broadcast notice
            // doesn't.
            (Some(from), Some(text)) => {
                self.push_lines(
                    &format!("[PM] {from}: {}", display_text(&text)),
                    LineKind::Chat,
                );
            }
            (None, Some(text)) => {
                self.push_lines(&display_text(&text), LineKind::Error);
            }
            _ => {}
        }
    }

    /// [`myTran_ChatMsg`] (106).
    ///
    /// [`myTran_ChatMsg`]: hotline_proto::Opcode::ChatMsg
    fn apply_chat_msg(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        if let Some(text) = tx.string(FieldId::Data, encoding) {
            self.push_lines(&display_text(&text), LineKind::Chat);
        }
    }

    /// [`myTran_DisconnectMsg`] (111).
    ///
    /// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
    fn apply_disconnect_msg(&mut self, tx: &Transaction) -> EventOutcome {
        let encoding = self.encoding.get();
        let reason = tx
            .string(FieldId::Data, encoding)
            .unwrap_or_else(|| "disconnected by the server".to_string());
        self.push(display_text(&reason), LineKind::Error);
        EventOutcome::Quit
    }

    /// [`myTran_InviteToChat`] (113).
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    fn apply_invite_to_chat(&mut self, tx: &Transaction) {
        let encoding = self.encoding.get();
        let from = tx
            .string(FieldId::UserName, encoding)
            .unwrap_or_else(|| "someone".to_string());
        let chat_id = tx.field(FieldId::ChatID).and_then(Field::as_u32);
        self.push(
            format!(
                "{from} invited you to a private chat (id {})",
                chat_id.map(|id| id.to_string()).unwrap_or_default()
            ),
            LineKind::System,
        );
    }

    /// [`myTran_NotifyChangeUser`] (301).
    ///
    /// [`myTran_NotifyChangeUser`]: hotline_proto::Opcode::NotifyChangeUser
    fn apply_notify_change_user(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return;
        };
        let encoding = self.encoding.get();
        let name = tx.string(FieldId::UserName, encoding).unwrap_or_default();
        let icon = tx
            .field(FieldId::UserIconID)
            .and_then(Field::as_u16)
            .unwrap_or_default();
        let flags = tx
            .field(FieldId::UserFlags)
            .and_then(Field::as_u16)
            .map(UserFlags::from_bits_retain)
            .unwrap_or_else(UserFlags::empty);

        match self.users.iter().position(|u| u.user_id == user_id) {
            Some(pos) => {
                let old_name = self.users[pos].name.clone();
                if !old_name.is_empty() && !name.is_empty() && *old_name != *name {
                    self.push(
                        format!(" <<< {old_name} is now known as {name} >>>"),
                        LineKind::System,
                    );
                }
                self.users[pos] = UserNameWithInfo {
                    user_id,
                    icon,
                    flags,
                    name: name.into(),
                };
            }
            None => {
                self.push(format!("{name} joined"), LineKind::System);
                self.users.push(UserNameWithInfo {
                    user_id,
                    icon,
                    flags,
                    name: name.into(),
                });
            }
        }
    }

    /// [`myTran_NotifyDeleteUser`] (302).
    ///
    /// [`myTran_NotifyDeleteUser`]: hotline_proto::Opcode::NotifyDeleteUser
    fn apply_notify_delete_user(&mut self, tx: &Transaction) {
        let Some(user_id) = tx.field(FieldId::UserID).and_then(Field::as_u16) else {
            return;
        };
        if let Some(pos) = self.users.iter().position(|u| u.user_id == user_id) {
            let user = self.users.remove(pos);
            self.push(format!("{} left", user.name), LineKind::System);
        }
    }

    /// [`myTran_UserAccess`] (354).
    ///
    /// [`myTran_UserAccess`]: hotline_proto::Opcode::UserAccess
    fn apply_user_access(&mut self, tx: &Transaction) {
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

    /// Send `line` as chat, reporting a failed send as a local error line
    /// (a transport error may never come back as a server push).
    pub async fn send_chat(&mut self, client: &HotlineClient, line: &str) {
        if let Err(e) = client.send_chat(&wire_text(line)).await {
            self.push(format!("failed to send: {e}"), LineKind::Error);
        }
    }

    pub async fn send_instant_msg(&mut self, client: &HotlineClient, user_id: u16, text: &str) {
        let text = wire_text(text);
        match client.send_instant_msg(user_id, &text).await {
            Ok(()) => {
                let to = match self.users.iter().find(|u| u.user_id == user_id) {
                    Some(user) => user.name.to_string(),
                    None => format!("#{user_id}"),
                };
                self.push_lines(&format!("[PM to {to}] {text}"), LineKind::Chat);
            }
            Err(e) => self.push(format!("failed to send: {e}"), LineKind::Error),
        }
    }
}

/// `text` with each line break as `\r`, the Hotline line separator.
fn wire_text(text: &str) -> String {
    text.replace('\n', "\r")
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
    fn wire_text_ends_each_line_with_cr() {
        assert_eq!(wire_text("one\ntwo"), "one\rtwo");
    }

    #[test]
    fn chat_msg_shows_each_line_on_its_own() {
        use hotline_proto::opcode::Opcode;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let chat = Transaction::push(
            Opcode::ChatMsg,
            vec![Field::string(FieldId::Data, "\rAlice: one\rtwo")],
        );
        let _ = session.apply_server_event(&chat);
        let texts: Vec<&str> = session.lines.iter().map(|l| &*l.text).collect();
        assert!(texts.ends_with(&["Alice: one", "two"]));
    }

    #[test]
    fn display_text_strips_leading_cr() {
        assert_eq!(display_text("\rAlice: hi"), "Alice: hi");
    }

    #[test]
    fn display_text_leaves_normal_text_alone() {
        assert_eq!(display_text("Alice: hi"), "Alice: hi");
    }

    /// [`myTran_InviteToChat`] (113) is both the client's invite request
    /// and the server's resulting push; a value-based classification
    /// would misread this push and silently drop it here.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    #[test]
    fn chat_invite_push_is_recognized_despite_sharing_its_wire_value_with_a_client_opcode() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let invite = Transaction::push(
            Opcode::InviteToChat,
            vec![
                Field::string(FieldId::UserName, "Alice"),
                Field::u32(FieldId::ChatID, 7),
            ],
        );
        let _ = session.apply_server_event(&invite);
        assert!(session.lines.iter().any(|line| line
            .text
            .contains("Alice invited you to a private chat (id 7)")));
    }

    #[test]
    fn notify_change_user_carries_admin_color_for_new_and_existing_users() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let admin_change = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, UserFlags::ADMIN.bits()),
            ],
        );

        // New user: flags should carry the admin bit from the start.
        let _ = session.apply_server_event(&admin_change);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(user.flags.contains(UserFlags::ADMIN));

        // Existing user: a later update should still reflect current flags.
        let demoted = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Root"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&demoted);
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert!(!user.flags.contains(UserFlags::ADMIN));
    }

    #[test]
    fn notify_change_user_notes_a_rename() {
        use hotline_proto::opcode::Opcode;
        use hotline_proto::FieldId;

        let mut session = ClientSession::new("me".to_string(), 0, EncodingHandle::default());
        let joined = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alice"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&joined);

        let renamed = Transaction::push(
            Opcode::NotifyChangeUser,
            vec![
                Field::u16(FieldId::UserID, 2),
                Field::u16(FieldId::UserIconID, 0),
                Field::string(FieldId::UserName, "Alicia"),
                Field::u16(FieldId::UserFlags, 0),
            ],
        );
        let _ = session.apply_server_event(&renamed);

        assert_eq!(
            &*session.lines.last().unwrap().text,
            " <<< Alice is now known as Alicia >>>"
        );
        let user = session.users.iter().find(|u| u.user_id == 2).unwrap();
        assert_eq!(&*user.name, "Alicia");
    }
}
