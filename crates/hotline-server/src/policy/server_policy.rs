use std::borrow::Cow;
use std::io;

use super::TimeZone;

use crate::state::{TransferReport, UserEntry};

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
    /// The category gave out article ID `u32::MAX`, so
    /// [`myTran_PostNewsArt`] (410) has no ID for the article. openline
    /// sends no reply.
    ///
    /// [`myTran_PostNewsArt`]: hotline_proto::Opcode::PostNewsArt
    NoArticleId,
    /// The comment write in [`myTran_SetFileInfo`] (207) failed. openline
    /// still renames.
    ///
    /// [`myTran_SetFileInfo`]: hotline_proto::Opcode::SetFileInfo
    CommentWrite {
        /// The name of the file or folder.
        name: String,
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

/// Who joins a private chat room, and who sets its subject.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ChatRooms {
    /// Each user joins each open private room, and sets its subject. A
    /// request with no subject clears it. A request on the public chat
    /// fails. A room password has no effect.
    #[default]
    Open,
    /// A user joins with a pending invite, or with the room's password.
    /// Only a member sets the subject or the password of a room. A user
    /// with `AccessExtra::SET_SUBJECT` sets the subject of the public
    /// chat.
    Invited,
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

    /// Who joins a private chat room, and who sets its subject.
    fn chat_rooms(&self) -> ChatRooms {
        ChatRooms::default()
    }

    /// The [`myTran_GetClientInfoText`] (303) text about `user`, who has
    /// `transfers`. The default has openline's lines.
    ///
    /// [`myTran_GetClientInfoText`]: hotline_proto::Opcode::GetClientInfoText
    fn client_info_text(&self, user: &UserEntry, transfers: &TransferReport) -> String {
        format!(
            "Nickname:   {}\r\
             Name:       {}\r\
             Account:    {}\r\
             Address:    {}\r\r{}",
            user.name,
            user.account_name,
            user.login,
            user.remote_addr.ip(),
            transfers.text(),
        )
    }
}
