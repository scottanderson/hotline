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
//! Variant names track the tail of the original wire constant;
//! `opcode_names!`'s invocation pairs each variant with that original
//! name once, and `Debug` reads it back via `Opcode::original_name`
//! rather than storing the string a second time. A few names (`ChatSend`/
//! `ChatMsg` for the two different `Chat` opcodes, `OldPostNews`/`NewMsg`
//! for the two different `NewsfilePost` opcodes, ...) exist only because
//! merging client and server opcodes into one enum forced a rename where
//! the two directions used to share a same-sounding but numerically
//! different Rust name.
//!
//! [`Opcode::Ping`] (500) doesn't appear in `HLProtocol.md`; its name
//! cites openline's `HotlineClientServerCommon.h` constant instead.

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
    NewsfileGet = 101,
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
    Agreement = 109,
    /// Kick (and optionally ban) a user (110).
    /// Original: `myTran_DisconnectUser`.
    UserKick = 110,
    /// The server is disconnecting this client, with a reason (111).
    /// Original: `myTran_DisconnectMsg`.
    Politequit = 111,
    /// Open/create a private chat room (112).
    /// Original: `myTran_InviteNewChat`.
    ChatCreate = 112,
    /// Invite a user to a private chat room, client request and the
    /// server's resulting push to the invitee (113).
    /// Original: `myTran_InviteToChat`.
    ChatInvite = 113,
    /// Decline an invitation to a private chat room (114).
    /// Original: `myTran_RejectChatInvite`.
    ChatDecline = 114,
    /// Join a private chat room, accepting an invitation (115).
    /// Original: `myTran_JoinChat`.
    ChatJoin = 115,
    /// Leave a private chat room (116).
    /// Original: `myTran_LeaveChat`.
    ChatPart = 116,
    /// A user joined a private chat room (117).
    /// Original: `myTran_NotifyChatChangeUser`.
    ChatUserChange = 117,
    /// A user left a private chat room (118).
    /// Original: `myTran_NotifyChatDeleteUser`.
    ChatUserPart = 118,
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
    Banner = 122,
    /// List files in a folder (200).
    /// Original: `myTran_GetFileNameList`.
    FileList = 200,
    /// Begin downloading a file (202).
    /// Original: `myTran_DownloadFile`.
    FileGet = 202,
    /// Begin uploading a file (203).
    /// Original: `myTran_UploadFile`.
    FilePut = 203,
    /// Delete a file or folder (204).
    /// Original: `myTran_DeleteFile`.
    FileDelete = 204,
    /// Create a folder (205).
    /// Original: `myTran_NewFolder`.
    FileMkdir = 205,
    /// Fetch a file's metadata (206).
    /// Original: `myTran_GetFileInfo`.
    FileGetinfo = 206,
    /// Rename/comment a file (207).
    /// Original: `myTran_SetFileInfo`.
    FileSetinfo = 207,
    /// Move a file to another folder (208).
    /// Original: `myTran_MoveFile`.
    FileMove = 208,
    /// Create a file alias (209).
    /// Original: `myTran_MakeFileAlias`.
    FileSymlink = 209,
    /// Server push: a queued download's position changed (211). Sent
    /// while `myField_WaitingCount` (116) is nonzero.
    /// Original: `myTran_DownloadInfo`.
    DownloadInfo = 211,
    /// Fetch the server banner image over `HTXF` (212).
    /// Original: `myTran_DownloadBanner`.
    BannerGet = 212,
    /// Fetch the connected-user list (300).
    /// Original: `myTran_GetUserNameList`.
    UserGetlist = 300,
    /// A user's info changed, or a user joined (301).
    /// Original: `myTran_NotifyChangeUser`.
    NotifyChangeUser = 301,
    /// A user disconnected (302).
    /// Original: `myTran_NotifyDeleteUser`.
    UserPart = 302,
    /// Fetch another user's info text (303).
    /// Original: `myTran_GetClientInfoText`.
    UserGetinfo = 303,
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
    AccountCreate = 350,
    /// Delete an account (351).
    /// Original: `myTran_DeleteUser`.
    AccountDelete = 351,
    /// Fetch an account's details (352).
    /// Original: `myTran_GetUser`.
    AccountRead = 352,
    /// Update an account's details (353).
    /// Original: `myTran_SetUser`.
    AccountModify = 353,
    /// This client's access privileges were (re)set (354).
    /// Original: `myTran_UserAccess`.
    UserSelfinfo = 354,
    /// Broadcast a message to every connected user, client request and
    /// the resulting server push (355).
    /// Original: `myTran_UserBroadcast`.
    MsgBroadcast = 355,
    /// List news categories at a path (370).
    /// Original: `myTran_GetNewsCatNameList`.
    NewsListdir = 370,
    /// List news articles at a path (371).
    /// Original: `myTran_GetNewsArtNameList`.
    NewsListcategory = 371,
    /// Delete a news folder/category (380).
    /// Original: `myTran_DelNewsItem`.
    NewsDelete = 380,
    /// Create a news folder (381).
    /// Original: `myTran_NewNewsFldr`.
    NewsMkdir = 381,
    /// Create a news category (382).
    /// Original: `myTran_NewNewsCat`.
    NewsMkcategory = 382,
    /// Fetch one news article's data (400).
    /// Original: `myTran_GetNewsArtData`.
    NewsGetthread = 400,
    /// Post a threaded news article (410).
    /// Original: `myTran_PostNewsArt`.
    NewsPostthread = 410,
    /// Delete a threaded news article (411).
    /// Original: `myTran_DelNewsArt`.
    NewsDeletethread = 411,
    /// Idle keep-alive ping (500); not documented in `HLProtocol.md`.
    /// Original: `myTran_KeepConnectionAlive`.
    Ping = 500,
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u32),
}

/// Declares a `const fn original_name(&self) -> &'static str` from a list
/// of `(ORIGINAL_WIRE_NAME, Variant)` pairs, so `Debug` doesn't need to
/// store each variant's original wire constant name a second time.
macro_rules! opcode_names {
    ($(($orig:ident, $variant:ident)),* $(,)?) => {
        impl Opcode {
            const fn original_name(&self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($orig),)*
                    Self::Unknown(_) => "(unrecognized)",
                }
            }
        }
    };
}

opcode_names!(
    (myTran_GetMsgs, NewsfileGet),
    (myTran_NewMsg, NewMsg),
    (myTran_OldPostNews, OldPostNews),
    (myTran_ServerMsg, ServerMsg),
    (myTran_ChatSend, ChatSend),
    (myTran_ChatMsg, ChatMsg),
    (myTran_Login, Login),
    (myTran_SendInstantMsg, SendInstantMsg),
    (myTran_ShowAgreement, Agreement),
    (myTran_DisconnectUser, UserKick),
    (myTran_DisconnectMsg, Politequit),
    (myTran_InviteNewChat, ChatCreate),
    (myTran_InviteToChat, ChatInvite),
    (myTran_RejectChatInvite, ChatDecline),
    (myTran_JoinChat, ChatJoin),
    (myTran_LeaveChat, ChatPart),
    (myTran_NotifyChatChangeUser, ChatUserChange),
    (myTran_NotifyChatDeleteUser, ChatUserPart),
    (myTran_NotifyChatSubject, NotifyChatSubject),
    (myTran_SetChatSubject, SetChatSubject),
    (myTran_Agreed, Agreed),
    (myTran_ServerBanner, Banner),
    (myTran_GetFileNameList, FileList),
    (myTran_DownloadFile, FileGet),
    (myTran_UploadFile, FilePut),
    (myTran_DeleteFile, FileDelete),
    (myTran_NewFolder, FileMkdir),
    (myTran_GetFileInfo, FileGetinfo),
    (myTran_SetFileInfo, FileSetinfo),
    (myTran_MoveFile, FileMove),
    (myTran_MakeFileAlias, FileSymlink),
    (myTran_DownloadInfo, DownloadInfo),
    (myTran_DownloadBanner, BannerGet),
    (myTran_GetUserNameList, UserGetlist),
    (myTran_NotifyChangeUser, NotifyChangeUser),
    (myTran_NotifyDeleteUser, UserPart),
    (myTran_GetClientInfoText, UserGetinfo),
    (myTran_SetClientUserInfo, SetClientUserInfo),
    (myTran_GetUserList, GetUserList),
    (myTran_SetUserList, SetUserList),
    (myTran_NewUser, AccountCreate),
    (myTran_DeleteUser, AccountDelete),
    (myTran_GetUser, AccountRead),
    (myTran_SetUser, AccountModify),
    (myTran_UserAccess, UserSelfinfo),
    (myTran_UserBroadcast, MsgBroadcast),
    (myTran_GetNewsCatNameList, NewsListdir),
    (myTran_GetNewsArtNameList, NewsListcategory),
    (myTran_DelNewsItem, NewsDelete),
    (myTran_NewNewsFldr, NewsMkdir),
    (myTran_NewNewsCat, NewsMkcategory),
    (myTran_GetNewsArtData, NewsGetthread),
    (myTran_PostNewsArt, NewsPostthread),
    (myTran_DelNewsArt, NewsDeletethread),
    (myTran_KeepConnectionAlive, Ping),
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

    /// `113` (`myTran_InviteToChat`) is both the client's invite request
    /// and the server's resulting push; direction still comes from which
    /// wire the value was read off of, not from the opcode itself.
    #[test]
    fn the_same_wire_value_is_a_request_from_a_client_and_a_push_from_a_server() {
        assert_eq!(
            TransactionType::from_client_wire(113),
            TransactionType::Request(Opcode::ChatInvite)
        );
        assert_eq!(
            TransactionType::from_server_wire(113),
            TransactionType::Push(Opcode::ChatInvite)
        );
    }

    /// `Debug`'s reconstructed name must match every opcode's real
    /// original wire constant, not just look plausible.
    #[test]
    fn debug_reconstructs_every_opcodes_original_name() {
        use Opcode::*;
        let cases = [
            ("101 myTran_GetMsgs", NewsfileGet),
            ("102 myTran_NewMsg", NewMsg),
            ("103 myTran_OldPostNews", OldPostNews),
            ("104 myTran_ServerMsg", ServerMsg),
            ("105 myTran_ChatSend", ChatSend),
            ("106 myTran_ChatMsg", ChatMsg),
            ("117 myTran_NotifyChatChangeUser", ChatUserChange),
            ("207 myTran_SetFileInfo", FileSetinfo),
            ("300 myTran_GetUserNameList", UserGetlist),
            ("301 myTran_NotifyChangeUser", NotifyChangeUser),
            ("304 myTran_SetClientUserInfo", SetClientUserInfo),
            ("348 myTran_GetUserList", GetUserList),
            ("349 myTran_SetUserList", SetUserList),
            ("354 myTran_UserAccess", UserSelfinfo),
            ("355 myTran_UserBroadcast", MsgBroadcast),
            ("371 myTran_GetNewsArtNameList", NewsListcategory),
            ("500 myTran_KeepConnectionAlive", Ping),
            ("57005 (unrecognized)", Unknown(0xDEAD)),
        ];
        for (expected, opcode) in cases {
            assert_eq!(format!("{opcode:?}"), expected);
        }
    }
}
