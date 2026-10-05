use std::borrow::Cow;

/// A case where openline sends no error and goes on.
#[derive(Debug)]
#[non_exhaustive]
pub enum Silent {
    /// [`myTran_SendInstantMsg`] (108) or [`myTran_UserBroadcast`] (355)
    /// has no [`myField_Data`] (101). openline sends nothing and replies with
    /// success.
    ///
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    /// [`myTran_UserBroadcast`]: hotline_proto::Opcode::UserBroadcast
    /// [`myField_Data`]: hotline_proto::FieldId::Data
    MessageWithoutData,
    /// [`myTran_InviteToChat`] (113) or [`myTran_SetChatSubject`] (120)
    /// failed. openline sends no reply.
    ///
    /// [`myTran_InviteToChat`]: hotline_proto::Opcode::InviteToChat
    /// [`myTran_SetChatSubject`]: hotline_proto::Opcode::SetChatSubject
    ChatRequest {
        /// Why the request failed.
        reason: Cow<'static, str>,
    },
    /// The source of [`myTran_MoveFile`] (208) or [`myTran_MakeFileAlias`]
    /// (209) is in a drop box that the user cannot view. openline moves or
    /// aliases it.
    ///
    /// [`myTran_MoveFile`]: hotline_proto::Opcode::MoveFile
    /// [`myTran_MakeFileAlias`]: hotline_proto::Opcode::MakeFileAlias
    DropBoxSource {
        /// The missing privilege.
        reason: Cow<'static, str>,
    },
}

/// The error replies that a server sends where openline sends none.
pub trait ServerPolicy: Send + Sync {
    /// The error reply for `case`, which stops the request. `None` sends no
    /// error and goes on, as openline does.
    fn error(&self, _case: Silent) -> Option<Cow<'static, str>> {
        None
    }
}
