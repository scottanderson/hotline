use hotline_proto::types::UserNameWithInfo;

/// A private chat room this client just joined or created.
#[derive(Debug, Clone, Default)]
pub struct JoinedChat {
    /// The room's [`myField_ChatID`] (114).
    ///
    /// [`myField_ChatID`]: hotline_proto::FieldId::ChatID
    pub id: u32,
    /// The room's subject.
    pub subject: Box<str>,
    /// The users in the room, this client too.
    pub users: Vec<UserNameWithInfo>,
}
