/// What a console command sends back to the session that ran it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsoleReply {
    /// Nothing.
    Silent,
    /// A chat line, in the command's room if it has one.
    Chat(String),
    /// A private message from [`ConsoleReply::SENDER`].
    Message(String),
}

impl ConsoleReply {
    /// Sender name on a [`ConsoleReply::Message`], mhxd's own.
    pub const SENDER: &'static str = "hxd";
}
