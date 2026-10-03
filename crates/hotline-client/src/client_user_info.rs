use hotline_proto::types::UserChangeOptions;

/// This user's name, icon, and options, as [`myTran_SetClientUserInfo`] (304)
/// sends them.
///
/// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientUserInfo {
    /// The name shown in the user list.
    pub name: Box<str>,
    /// The icon ID.
    pub icon: u16,
    /// The options.
    pub options: UserChangeOptions,
    /// The reply to a private message, sent with
    /// [`UserChangeOptions::AUTO_RESPONSE`].
    pub automatic_response: Box<str>,
}
