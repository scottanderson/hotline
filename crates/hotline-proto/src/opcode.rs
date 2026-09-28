//! Transaction opcodes.
//!
//! A reply carries the sentinel `TASK_REPLY` (`0x0001_0000`) in the
//! type field, matched by masking the high 16 bits (some servers OR the
//! request's opcode into the low bits, e.g. `0x0001006b`). The specific
//! request a reply answers is identified by transaction ID alone.
//!
//! [`Opcode`] covers every opcode either direction sends: `HLProtocol.md`
//! numbers opcodes with one flat `myTran_*` list, no client/server split,
//! and a transaction-by-transaction audit against it found no case where
//! the same numeric value means something different depending on
//! direction. [`TransactionType::Request`]/[`TransactionType::Push`]
//! still carry direction, since a raw wire value alone doesn't say
//! whether it came from a client or a server (see
//! [`TransactionType::from_client_wire`]/[`from_server_wire`](TransactionType::from_server_wire)),
//! only which table a decoder should have consulted to get here.
//!
//! Each variant is the exact tail of its `myTran_*` constant, so
//! `myTran_GetMsgs` is [`Opcode::GetMsgs`]. `Debug` prints that name.
//!
//! `HLProtocol.md` does not list [`myTran_KillDownload`] (214),
//! [`myTran_GetUserList`] (348), [`myTran_SetUserList`] (349), or
//! [`myTran_KeepConnectionAlive`] (500). Their names come from openline's
//! `HotlineClientServerCommon.h`.
//!
//! [`myTran_KillDownload`]: crate::Opcode::KillDownload
//! [`myTran_GetUserList`]: crate::Opcode::GetUserList
//! [`myTran_SetUserList`]: crate::Opcode::SetUserList
//! [`myTran_KeepConnectionAlive`]: crate::Opcode::KeepConnectionAlive

use num_enum::{FromPrimitive, IntoPrimitive};

/// Sentinel opcode value every reply to a specific request carries in the
/// header's type field, instead of echoing `0` or the original request's
/// opcode. See the module docs for how real (and buggy) servers use this.
const TASK_REPLY: u32 = 0x0001_0000;

/// Transaction opcode, as sent by either a client or a server.
#[repr(u32)]
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
pub enum Opcode {
    /// Read articles from the classic (non-threaded) news (101).
    /// Original: `myTran_GetMsgs`.
    GetMsgs = 101,
    /// A new (non-threaded) news post arrived (102).
    /// Original: `myTran_NewMsg`.
    NewMsg = 102,
    /// Post a new article to the classic (non-threaded) news (103).
    /// Original: `myTran_OldPostNews`.
    OldPostNews = 103,
    /// A message from the server/admin/another user (104).
    /// Original: `myTran_ServerMsg`.
    ServerMsg = 104,
    /// Send a public/private chat message (105).
    /// Original: `myTran_ChatSend`.
    ChatSend = 105,
    /// A chat message to relay (106).
    /// Original: `myTran_ChatMsg`.
    ChatMsg = 106,
    /// Log in with credentials (107).
    /// Original: `myTran_Login`.
    Login = 107,
    /// Send an instant (private) message to another user (108).
    /// Original: `myTran_SendInstantMsg`.
    SendInstantMsg = 108,
    /// The server's usage agreement text, part of login (109).
    /// Original: `myTran_ShowAgreement`.
    ShowAgreement = 109,
    /// Kick (and optionally ban) a user (110).
    /// Original: `myTran_DisconnectUser`.
    DisconnectUser = 110,
    /// The server is disconnecting this client, with a reason (111).
    /// Original: `myTran_DisconnectMsg`.
    DisconnectMsg = 111,
    /// Open/create a private chat room (112).
    /// Original: `myTran_InviteNewChat`.
    InviteNewChat = 112,
    /// Invite a user to a private chat room, client request and the
    /// server's resulting push to the invitee (113).
    /// Original: `myTran_InviteToChat`.
    InviteToChat = 113,
    /// Decline an invitation to a private chat room (114).
    /// Original: `myTran_RejectChatInvite`.
    RejectChatInvite = 114,
    /// Join a private chat room, accepting an invitation (115).
    /// Original: `myTran_JoinChat`.
    JoinChat = 115,
    /// Leave a private chat room (116).
    /// Original: `myTran_LeaveChat`.
    LeaveChat = 116,
    /// A user joined a private chat room (117).
    /// Original: `myTran_NotifyChatChangeUser`.
    NotifyChatChangeUser = 117,
    /// A user left a private chat room (118).
    /// Original: `myTran_NotifyChatDeleteUser`.
    NotifyChatDeleteUser = 118,
    /// A private chat room's subject changed (119).
    /// Original: `myTran_NotifyChatSubject`.
    NotifyChatSubject = 119,
    /// Set a chat room's subject (120).
    /// Original: `myTran_SetChatSubject`.
    SetChatSubject = 120,
    /// Accept the server's usage agreement, completing login (121).
    /// Original: `myTran_Agreed`.
    Agreed = 121,
    /// The server banner's type and, for a URL banner, its target (122).
    /// Original: `myTran_ServerBanner`.
    ServerBanner = 122,
    /// List files in a folder (200).
    /// Original: `myTran_GetFileNameList`.
    GetFileNameList = 200,
    /// Begin downloading a file (202).
    /// Original: `myTran_DownloadFile`.
    DownloadFile = 202,
    /// Begin uploading a file (203).
    /// Original: `myTran_UploadFile`.
    UploadFile = 203,
    /// Delete a file or folder (204).
    /// Original: `myTran_DeleteFile`.
    DeleteFile = 204,
    /// Create a folder (205).
    /// Original: `myTran_NewFolder`.
    NewFolder = 205,
    /// Fetch a file's metadata (206).
    /// Original: `myTran_GetFileInfo`.
    GetFileInfo = 206,
    /// Rename/comment a file (207).
    /// Original: `myTran_SetFileInfo`.
    SetFileInfo = 207,
    /// Move a file to another folder (208).
    /// Original: `myTran_MoveFile`.
    MoveFile = 208,
    /// Create a file alias (209).
    /// Original: `myTran_MakeFileAlias`.
    MakeFileAlias = 209,
    /// Begin downloading a folder and its contents (210).
    /// Original: `myTran_DownloadFldr`.
    DownloadFldr = 210,
    /// Server push: a queued download's position changed (211). Sent
    /// while [`myField_WaitingCount`] (116) is nonzero.
    /// Original: `myTran_DownloadInfo`.
    ///
    /// [`myField_WaitingCount`]: crate::FieldId::WaitingCount
    DownloadInfo = 211,
    /// Fetch the server banner image over `HTXF` (212).
    /// Original: `myTran_DownloadBanner`.
    DownloadBanner = 212,
    /// Begin uploading a folder and its contents (213).
    /// Original: `myTran_UploadFldr`.
    UploadFldr = 213,
    /// Cancel a download by its reference number (214).
    /// Original: `myTran_KillDownload`.
    KillDownload = 214,
    /// Fetch the connected-user list (300).
    /// Original: `myTran_GetUserNameList`.
    GetUserNameList = 300,
    /// A user's info changed, or a user joined (301).
    /// Original: `myTran_NotifyChangeUser`.
    NotifyChangeUser = 301,
    /// A user disconnected (302).
    /// Original: `myTran_NotifyDeleteUser`.
    NotifyDeleteUser = 302,
    /// Fetch another user's info text (303).
    /// Original: `myTran_GetClientInfoText`.
    GetClientInfoText = 303,
    /// Announce this client's nick/icon/options (304).
    /// Original: `myTran_SetClientUserInfo`.
    SetClientUserInfo = 304,
    /// Fetch every account at once, for bulk editing (348).
    /// Original: `myTran_GetUserList`.
    GetUserList = 348,
    /// Save a bulk-edited account list (349).
    /// Original: `myTran_SetUserList`.
    SetUserList = 349,
    /// Create an account (350).
    /// Original: `myTran_NewUser`.
    NewUser = 350,
    /// Delete an account (351).
    /// Original: `myTran_DeleteUser`.
    DeleteUser = 351,
    /// Fetch an account's details (352).
    /// Original: `myTran_GetUser`.
    GetUser = 352,
    /// Update an account's details (353).
    /// Original: `myTran_SetUser`.
    SetUser = 353,
    /// This client's access privileges were (re)set (354).
    /// Original: `myTran_UserAccess`.
    UserAccess = 354,
    /// Broadcast a message to every connected user, client request and
    /// the resulting server push (355).
    /// Original: `myTran_UserBroadcast`.
    UserBroadcast = 355,
    /// List news categories at a path (370).
    /// Original: `myTran_GetNewsCatNameList`.
    GetNewsCatNameList = 370,
    /// List news articles at a path (371).
    /// Original: `myTran_GetNewsArtNameList`.
    GetNewsArtNameList = 371,
    /// Delete a news folder/category (380).
    /// Original: `myTran_DelNewsItem`.
    DelNewsItem = 380,
    /// Create a news folder (381).
    /// Original: `myTran_NewNewsFldr`.
    NewNewsFldr = 381,
    /// Create a news category (382).
    /// Original: `myTran_NewNewsCat`.
    NewNewsCat = 382,
    /// Fetch one news article's data (400).
    /// Original: `myTran_GetNewsArtData`.
    GetNewsArtData = 400,
    /// Post a threaded news article (410).
    /// Original: `myTran_PostNewsArt`.
    PostNewsArt = 410,
    /// Delete a threaded news article (411).
    /// Original: `myTran_DelNewsArt`.
    DelNewsArt = 411,
    /// Idle keep-alive ping (500); not documented in `HLProtocol.md`.
    /// Original: `myTran_KeepConnectionAlive`.
    KeepConnectionAlive = 500,
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u32),
}

/// Declares `Opcode::original_name`, the `myTran_` constant each listed
/// variant is named for.
macro_rules! opcode_names {
    ($($variant:ident),* $(,)?) => {
        impl Opcode {
            const fn original_name(&self) -> &'static str {
                match self {
                    $(Self::$variant => concat!("myTran_", stringify!($variant)),)*
                    Self::Unknown(_) => "(unrecognized)",
                }
            }

            #[cfg(test)]
            const ALL: &[Self] = &[$(Self::$variant),*];
        }
    };
}

opcode_names!(
    GetMsgs,
    NewMsg,
    OldPostNews,
    ServerMsg,
    ChatSend,
    ChatMsg,
    Login,
    SendInstantMsg,
    ShowAgreement,
    DisconnectUser,
    DisconnectMsg,
    InviteNewChat,
    InviteToChat,
    RejectChatInvite,
    JoinChat,
    LeaveChat,
    NotifyChatChangeUser,
    NotifyChatDeleteUser,
    NotifyChatSubject,
    SetChatSubject,
    Agreed,
    ServerBanner,
    GetFileNameList,
    DownloadFile,
    UploadFile,
    DeleteFile,
    NewFolder,
    GetFileInfo,
    SetFileInfo,
    MoveFile,
    MakeFileAlias,
    DownloadFldr,
    DownloadInfo,
    DownloadBanner,
    UploadFldr,
    KillDownload,
    GetUserNameList,
    NotifyChangeUser,
    NotifyDeleteUser,
    GetClientInfoText,
    SetClientUserInfo,
    GetUserList,
    SetUserList,
    NewUser,
    DeleteUser,
    GetUser,
    SetUser,
    UserAccess,
    UserBroadcast,
    GetNewsCatNameList,
    GetNewsArtNameList,
    DelNewsItem,
    NewNewsFldr,
    NewNewsCat,
    GetNewsArtData,
    PostNewsArt,
    DelNewsArt,
    KeepConnectionAlive,
);

impl std::fmt::Debug for Opcode {
    /// Prints the wire value and the original constant name, e.g.
    /// `107 myTran_Login`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", u32::from(*self), self.original_name())
    }
}

/// A decoded header `type` value: either the generic reply sentinel, a
/// client request opcode, or a server push opcode (unrecognized wire
/// values still resolve via [`Opcode::Unknown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionType {
    /// This transaction is a reply to an earlier request (see
    /// `TASK_REPLY`); match it back to the request by transaction ID.
    Reply,
    /// A client request opcode.
    Request(Opcode),
    /// An unsolicited server push.
    Push(Opcode),
}

impl TransactionType {
    /// Classify a raw wire `type` value known to have been sent **by a
    /// client**.
    pub fn from_client_wire(v: u32) -> Self {
        TransactionType::Request(Opcode::from(v))
    }

    /// Classify a raw wire `type` value known to have been sent **by a
    /// server**: the reply sentinel first, then a push.
    pub fn from_server_wire(v: u32) -> Self {
        if v & 0xFFFF_0000 == TASK_REPLY {
            return TransactionType::Reply;
        }
        TransactionType::Push(Opcode::from(v))
    }

    /// The wire `type` value this opcode encodes to.
    pub fn as_u32(self) -> u32 {
        match self {
            TransactionType::Reply => TASK_REPLY,
            TransactionType::Request(c) => u32::from(c),
            TransactionType::Push(s) => u32::from(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hl_protocol_md;

    #[test]
    fn every_variant_is_named_for_its_hlprotocol_md_constant() {
        let documented = hl_protocol_md::constants("myTran_");
        for &opcode in Opcode::ALL {
            let value = u32::from(opcode);
            let names: Vec<_> = documented.iter().filter(|(v, _)| *v == value).collect();
            assert!(!names.is_empty(), "opcode {value} has no constant");
            for (_, name) in names {
                assert_eq!(opcode.original_name(), name, "opcode {value}");
            }
        }
    }

    #[test]
    fn task_reply_sentinel_is_recognized_exactly() {
        assert_eq!(
            TransactionType::from_server_wire(TASK_REPLY),
            TransactionType::Reply
        );
    }

    #[test]
    fn buggy_server_task_reply_with_opcode_in_low_bits_still_matches() {
        let buggy = TASK_REPLY | u32::from(Opcode::Login);
        assert_eq!(
            TransactionType::from_server_wire(buggy),
            TransactionType::Reply
        );
    }

    #[test]
    fn known_client_and_server_opcodes_round_trip() {
        assert_eq!(
            TransactionType::from_client_wire(u32::from(Opcode::Login)),
            TransactionType::Request(Opcode::Login)
        );
        assert_eq!(
            TransactionType::from_server_wire(u32::from(Opcode::ChatMsg)),
            TransactionType::Push(Opcode::ChatMsg)
        );
    }

    #[test]
    fn unrecognized_opcode_is_preserved_not_dropped() {
        assert_eq!(
            TransactionType::from_client_wire(0xDEAD),
            TransactionType::Request(Opcode::Unknown(0xDEAD))
        );
        assert_eq!(
            TransactionType::from_server_wire(0xDEAD),
            TransactionType::Push(Opcode::Unknown(0xDEAD))
        );
    }

    /// [`myTran_InviteToChat`] (113) is both the client's invite request
    /// and the server's resulting push; direction still comes from which
    /// wire the value was read off of, not from the opcode itself.
    ///
    /// [`myTran_InviteToChat`]: crate::Opcode::InviteToChat
    #[test]
    fn the_same_wire_value_is_a_request_from_a_client_and_a_push_from_a_server() {
        assert_eq!(
            TransactionType::from_client_wire(113),
            TransactionType::Request(Opcode::InviteToChat)
        );
        assert_eq!(
            TransactionType::from_server_wire(113),
            TransactionType::Push(Opcode::InviteToChat)
        );
    }

    /// `Debug`'s reconstructed name must match every opcode's real
    /// original wire constant, not just look plausible.
    #[test]
    fn debug_reconstructs_every_opcodes_original_name() {
        use Opcode::*;
        let cases = [
            ("101 myTran_GetMsgs", GetMsgs),
            ("102 myTran_NewMsg", NewMsg),
            ("103 myTran_OldPostNews", OldPostNews),
            ("104 myTran_ServerMsg", ServerMsg),
            ("105 myTran_ChatSend", ChatSend),
            ("106 myTran_ChatMsg", ChatMsg),
            ("117 myTran_NotifyChatChangeUser", NotifyChatChangeUser),
            ("207 myTran_SetFileInfo", SetFileInfo),
            ("300 myTran_GetUserNameList", GetUserNameList),
            ("301 myTran_NotifyChangeUser", NotifyChangeUser),
            ("304 myTran_SetClientUserInfo", SetClientUserInfo),
            ("348 myTran_GetUserList", GetUserList),
            ("349 myTran_SetUserList", SetUserList),
            ("354 myTran_UserAccess", UserAccess),
            ("355 myTran_UserBroadcast", UserBroadcast),
            ("371 myTran_GetNewsArtNameList", GetNewsArtNameList),
            ("500 myTran_KeepConnectionAlive", KeepConnectionAlive),
            ("57005 (unrecognized)", Unknown(0xDEAD)),
        ];
        for (expected, opcode) in cases {
            assert_eq!(format!("{opcode:?}"), expected);
        }
    }
}
