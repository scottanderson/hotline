/// The ban that goes with a [`myTran_DisconnectUser`] (110).
///
/// [`myTran_DisconnectUser`]: hotline_proto::Opcode::DisconnectUser
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ban {
    /// Disconnect only.
    None,
    /// Ban the user's address for a while.
    Temporary,
    /// Ban the user's address until an administrator lifts the ban.
    Permanent,
}

impl Ban {
    /// The [`myField_Options`] (113) value, `None` for no ban.
    ///
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    pub(crate) const fn options(self) -> Option<u16> {
        match self {
            Self::None => None,
            Self::Temporary => Some(1),
            Self::Permanent => Some(2),
        }
    }
}
