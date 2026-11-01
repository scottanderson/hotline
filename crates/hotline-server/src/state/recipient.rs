use hotline_proto::Transaction;
use tokio::sync::mpsc;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

use super::{AccessUpdate, ConsoleUpdate};

/// How many messages wait for one session before more are dropped.
pub(crate) const QUEUE_LEN: usize = 256;

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
    /// The notice of a kick, with this reason. The session ends after it.
    Kick(Box<str>),
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
    pub(crate) fn into_kick(self) -> Option<Box<str>> {
        match self {
            Self::Kick(reason) => Some(reason),
            _ => None,
        }
    }
}

/// The sending side of one session's [`Inbox`].
#[derive(Clone)]
pub(crate) struct Recipient {
    messages: mpsc::Sender<RecipientMessage>,
    ended: CancellationToken,
}

impl Recipient {
    /// Queue `message`. A full queue drops it and returns `false`.
    pub(crate) fn try_send(&self, message: RecipientMessage) -> bool {
        self.messages.try_send(message).is_ok()
    }

    /// End the session. It writes the queued messages up to a kick notice,
    /// if one is queued, and drops the others.
    pub(crate) fn end(&self) {
        self.ended.cancel();
    }

    /// Resolves once the session drops its [`Inbox`].
    pub(crate) async fn closed(&self) {
        self.messages.closed().await;
    }

    /// Resolves once the server ends the session, or the session drops its
    /// [`Inbox`].
    pub(crate) async fn ended(&self) {
        tokio::select! {
            () = self.ended.cancelled() => {}
            () = self.messages.closed() => {}
        }
    }
}

/// Everything server-initiated for one session.
pub struct Inbox {
    messages: mpsc::Receiver<RecipientMessage>,
    ended: CancellationToken,
}

impl Inbox {
    /// `true` once the server ends the session.
    pub(crate) fn is_ended(&self) -> bool {
        self.ended.is_cancelled()
    }

    /// Resolves once the server ends the session.
    pub(crate) fn ended(&self) -> WaitForCancellationFutureOwned {
        self.ended.clone().cancelled_owned()
    }

    /// The next message, or `None` once the server drops the session.
    pub(crate) async fn recv(&mut self) -> Option<RecipientMessage> {
        self.messages.recv().await
    }

    /// The next message, if one waits.
    pub(crate) fn try_recv(&mut self) -> Result<RecipientMessage, mpsc::error::TryRecvError> {
        self.messages.try_recv()
    }
}

/// A new connection's user ID and its inbound channel.
pub(crate) struct Connection {
    pub(crate) user_id: u16,
    pub(crate) inbox: Inbox,
}

/// A new [`Recipient`] and its [`Inbox`].
pub(crate) fn channel() -> (Recipient, Inbox) {
    let (messages_tx, messages) = mpsc::channel(QUEUE_LEN);
    let ended = CancellationToken::new();
    let recipient = Recipient {
        messages: messages_tx,
        ended: ended.clone(),
    };
    (recipient, Inbox { messages, ended })
}

#[cfg(test)]
mod tests {
    use futures::FutureExt;

    use super::*;

    #[test]
    fn end_reaches_the_inbox_when_the_queue_is_full() {
        let (recipient, inbox) = channel();
        let chat = Transaction::push(hotline_proto::Opcode::ChatMsg, Vec::new());
        for _ in 0..QUEUE_LEN + 1 {
            recipient.try_send(RecipientMessage::Push(chat.clone()));
        }
        recipient.end();
        assert!(inbox.is_ended());
    }

    #[test]
    fn ended_resolves_when_the_server_ends_the_session() {
        let (recipient, _inbox) = channel();
        recipient.end();
        assert!(recipient.ended().now_or_never().is_some());
    }
}
