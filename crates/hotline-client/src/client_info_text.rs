/// A user's [`myTran_GetClientInfoText`] (303) reply.
///
/// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
#[derive(Debug, Clone, Default)]
pub struct ClientInfoText {
    /// The user's name.
    pub name: Box<str>,
    /// The info text, with `\r` between lines.
    pub text: Box<str>,
}
