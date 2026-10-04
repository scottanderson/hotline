use hotline_proto::Transaction;

use super::{AccessUpdate, ConsoleUpdate};

/// Everything server-initiated that can arrive on a recipient's channel.
#[derive(Debug, Clone)]
pub enum RecipientMessage {
    /// A wire transaction to forward as-is.
    Push(Transaction),
    /// A wire transaction sent to every session, forwarded only after login.
    Broadcast(Transaction),
    /// A live privilege change for this session's login to apply.
    AccessUpdate(AccessUpdate),
    /// A console-command-driven change for this exact session to apply.
    Console(ConsoleUpdate),
    /// Close this connection with no wire-visible notice.
    Disconnect,
}

impl RecipientMessage {
    #[cfg(test)]
    pub(crate) fn into_push(self) -> Option<Transaction> {
        match self {
            Self::Push(transaction) | Self::Broadcast(transaction) => Some(transaction),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_access_update(self) -> Option<AccessUpdate> {
        match self {
            Self::AccessUpdate(access_update) => Some(access_update),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn into_console_update(self) -> Option<ConsoleUpdate> {
        match self {
            Self::Console(update) => Some(update),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn is_disconnect(&self) -> bool {
        matches!(self, Self::Disconnect)
    }
}
