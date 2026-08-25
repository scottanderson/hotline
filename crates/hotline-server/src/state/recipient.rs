use hotline_proto::Transaction;

use super::AccessUpdate;

/// Everything server-initiated that can arrive on a recipient's channel.
#[derive(Debug, Clone)]
pub enum RecipientMessage {
    /// A wire transaction to forward as-is.
    Push(Transaction),
    /// A live privilege change for this session's login to apply.
    AccessUpdate(AccessUpdate),
    /// Close this connection with no wire-visible notice.
    Disconnect,
}

impl RecipientMessage {
    #[cfg(test)]
    pub(crate) fn into_push(self) -> Option<Transaction> {
        match self {
            Self::Push(transaction) => Some(transaction),
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
    pub(crate) fn is_disconnect(&self) -> bool {
        matches!(self, Self::Disconnect)
    }
}
