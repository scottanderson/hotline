/// What the caller must do after applying one server event.
#[must_use]
pub enum EventOutcome {
    /// State updated; nothing further needed.
    Applied,
    /// Nothing to show for the agreement; the caller should call
    /// [`super::ClientSession::accept_agreement`] right away to complete
    /// login.
    NeedsAgreementAccept,
    /// The server ended the connection. The caller shows why, and quits
    /// when the user closes it.
    Disconnected(Disconnect),
}

/// Why the connection ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disconnect {
    /// [`myTran_DisconnectMsg`] (111) with the server's text.
    ///
    /// [`myTran_DisconnectMsg`]: hotline_proto::Opcode::DisconnectMsg
    Message(Box<str>),
    /// The server closed the connection.
    Closed,
}

impl Disconnect {
    const CLOSED: &str = "The connection with the server has been closed. Try connecting later.";

    /// The title of the alert.
    pub const fn title(&self) -> &'static str {
        match self {
            Self::Message(_) => "Disconnected",
            Self::Closed => "Connection Closed",
        }
    }

    /// The text of the alert.
    pub fn text(&self) -> String {
        match self {
            Self::Message(text) => format!("You have been disconnected:\n{text}"),
            Self::Closed => Self::CLOSED.to_string(),
        }
    }

    /// The line for the chat: the server's text, or the alert text.
    pub fn line(&self) -> &str {
        match self {
            Self::Message(text) => text,
            Self::Closed => Self::CLOSED,
        }
    }
}
