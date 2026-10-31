use std::borrow::Cow;
use std::io;

use super::TimeZone;

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
    /// The comment write in [`myTran_SetFileInfo`] (207) failed. openline
    /// still renames.
    ///
    /// [`myTran_SetFileInfo`]: hotline_proto::Opcode::SetFileInfo
    CommentWrite {
        /// The write error.
        error: io::Error,
    },
}

/// What [`myTran_SetUserList`] (349) does with a rename onto a taken login.
///
/// [`myTran_SetUserList`]: hotline_proto::Opcode::SetUserList
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum TakenLogin {
    /// Write the edit over the account with that login, keep the old
    /// account, and move the old login's sessions to the new login. The
    /// sessions of the taken login keep their access and name. The reply
    /// is an error.
    #[default]
    Overwrite,
    /// Change nothing, send a [`myTran_ServerMsg`] (104) notice that names
    /// the taken login, and reply with an error.
    ///
    /// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
    Refuse,
}

/// The choices where a server differs from openline.
pub trait ServerPolicy: Send + Sync {
    /// The error reply for `case`, which stops the request. `None` sends no
    /// error and goes on, as openline does.
    fn error(&self, _case: Silent) -> Option<Cow<'static, str>> {
        None
    }

    /// The time zone of each date that the server sends.
    fn time_zone(&self) -> TimeZone {
        TimeZone::default()
    }

    /// What a rename onto a taken login does.
    fn taken_login(&self) -> TakenLogin {
        TakenLogin::default()
    }
}
