use hotline_client::JoinedChat;
use hotline_proto::types::UserNameWithInfo;

use crate::line::{Line, LineKind};

/// A private chat room this client is in.
pub struct ChatRoom {
    pub id: u32,
    pub subject: Box<str>,
    pub users: Vec<UserNameWithInfo>,
    pub lines: Vec<Line>,
}

impl ChatRoom {
    /// The room's tab label: its subject, or `Chat <id>` without one.
    pub fn label(&self) -> String {
        match &*self.subject {
            "" => format!("Chat {}", self.id),
            subject => subject.to_string(),
        }
    }

    pub(super) fn push(&mut self, text: impl Into<Box<str>>, kind: LineKind) {
        self.lines.push(Line {
            text: text.into(),
            kind,
        });
    }
}

impl From<JoinedChat> for ChatRoom {
    fn from(joined: JoinedChat) -> Self {
        Self {
            id: joined.id,
            subject: joined.subject,
            users: joined.users,
            lines: Vec::new(),
        }
    }
}

/// An invitation to a private chat room that waits for an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatInvite {
    pub chat_id: u32,
    /// The name of the user who sent it.
    pub from: Box<str>,
}
