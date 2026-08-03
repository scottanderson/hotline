//! Transaction opcodes.
//!
//! A reply carries the sentinel [`TASK_REPLY`] (`0x0001_0000`) in the
//! type field, matched by masking the high 16 bits (some servers OR the
//! request's opcode into the low bits, e.g. `0x0001006b`). The specific
//! request a reply answers is identified by transaction ID alone.
//!
//! [`ClientOpcode`]/[`ServerOpcode`] are separate enums: the same
//! numeric value can appear in both tables (`113` is both
//! `HTLC_HDR_CHAT_INVITE` and `HTLS_HDR_CHAT_INVITE`), so
//! [`Opcode::from_client_wire`]/[`Opcode::from_server_wire`] can't be
//! merged; the caller must already know who sent the value.
//!
//! Variant names track the tail of the original wire constant.

/// Sentinel opcode value every reply to a specific request carries in the
/// header's type field, instead of echoing `0` or the original request's
/// opcode. See the module docs for how real (and buggy) servers use this.
pub const TASK_REPLY: u32 = 0x0001_0000;

/// Client -> server transaction opcodes.
#[repr(u32)]
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ClientOpcode {
    /// Read articles from the classic (non-threaded) news (101).
    /// Original: `HTLC_HDR_NEWSFILE_GET`.
    NewsfileGet = 101,
    /// Post a new article to the classic (non-threaded) news (103).
    /// Original: `HTLC_HDR_NEWSFILE_POST`.
    NewsfilePost = 103,
    /// Send a public/private chat message (105).
    /// Original: `HTLC_HDR_CHAT`.
    Chat = 105,
    /// Log in with credentials (107).
    /// Original: `HTLC_HDR_LOGIN`.
    Login = 107,
    /// Send an instant (private) message to another user (108).
    /// Original: `HTLC_HDR_MSG`.
    Msg = 108,
    /// Kick (and optionally ban) a user (110).
    /// Original: `HTLC_HDR_USER_KICK`.
    UserKick = 110,
    /// Open/create a private chat room (112).
    /// Original: `HTLC_HDR_CHAT_CREATE`.
    ChatCreate = 112,
    /// Invite a user to a private chat room (113).
    /// Original: `HTLC_HDR_CHAT_INVITE`.
    ChatInvite = 113,
    /// Decline an invitation to a private chat room (114).
    /// Original: `HTLC_HDR_CHAT_DECLINE`.
    ChatDecline = 114,
    /// Join a private chat room, accepting an invitation (115).
    /// Original: `HTLC_HDR_CHAT_JOIN`.
    ChatJoin = 115,
    /// Leave a private chat room (116).
    /// Original: `HTLC_HDR_CHAT_PART`.
    ChatPart = 116,
    /// Set a chat room's subject (120).
    /// Original: `HTLC_HDR_CHAT_SUBJECT`.
    ChatSubject = 120,
    /// Accept the server's usage agreement, completing login (121).
    /// Original: `HTLC_HDR_AGREED`.
    Agreed = 121,
    /// List files in a folder (200).
    /// Original: `HTLC_HDR_FILE_LIST`.
    FileList = 200,
    /// Begin downloading a file (202).
    /// Original: `HTLC_HDR_FILE_GET`.
    FileGet = 202,
    /// Begin uploading a file (203).
    /// Original: `HTLC_HDR_FILE_PUT`.
    FilePut = 203,
    /// Delete a file or folder (204).
    /// Original: `HTLC_HDR_FILE_DELETE`.
    FileDelete = 204,
    /// Create a folder (205).
    /// Original: `HTLC_HDR_FILE_MKDIR`.
    FileMkdir = 205,
    /// Fetch a file's metadata (206).
    /// Original: `HTLC_HDR_FILE_GETINFO`.
    FileGetinfo = 206,
    /// Rename/comment a file (207).
    /// Original: `HTLC_HDR_FILE_SETINFO`.
    FileSetnfo = 207,
    /// Move a file to another folder (208).
    /// Original: `HTLC_HDR_FILE_MOVE`.
    FileMove = 208,
    /// Create a file alias (209).
    /// Original: `HTLC_HDR_FILE_SYMLINK`.
    FileSymlink = 209,
    /// Fetch the connected-user list (300).
    /// Original: `HTLC_HDR_USER_GETLIST`.
    UserGetlist = 300,
    /// Fetch another user's info text (303).
    /// Original: `HTLC_HDR_USER_GETINFO`.
    UserGetinfo = 303,
    /// Announce this client's nick/icon/options (304).
    /// Original: `HTLC_HDR_USER_CHANGE`.
    UserChange = 304,
    /// Create an account (350).
    /// Original: `HTLC_HDR_ACCOUNT_CREATE`.
    AccountCreate = 350,
    /// Delete an account (351).
    /// Original: `HTLC_HDR_ACCOUNT_DELETE`.
    AccountDelete = 351,
    /// Fetch an account's details (352).
    /// Original: `HTLC_HDR_ACCOUNT_READ`.
    AccountRead = 352,
    /// Update an account's details (353).
    /// Original: `HTLC_HDR_ACCOUNT_MODIFY`.
    AccountModify = 353,
    /// Broadcast a message to every connected user (355).
    /// Original: `HTLC_HDR_MSG_BROADCAST`.
    MessageBroadcast = 355,
    /// List news categories at a path (370).
    /// Original: `HTLC_HDR_NEWS_LISTDIR`.
    NewsListdir = 370,
    /// List news articles at a path (371).
    /// Original: `HTLC_HDR_NEWS_LISTCATEGORY`.
    NewsListcategory = 371,
    /// Delete a news folder/category (380).
    /// Original: `HTLC_HDR_NEWS_DELETE`.
    NewsDelete = 380,
    /// Create a news folder (381).
    /// Original: `HTLC_HDR_NEWS_MKDIR`.
    NewsMkdir = 381,
    /// Create a news category (382).
    /// Original: `HTLC_HDR_NEWS_MKCATEGORY`.
    NewsMkcategory = 382,
    /// Fetch one news article's data (400).
    /// Original: `HTLC_HDR_NEWS_GETTHREAD`.
    NewsGetthread = 400,
    /// Post a threaded news article (410).
    /// Original: `HTLC_HDR_NEWS_POSTTHREAD`.
    NewsPostthread = 410,
    /// Delete a threaded news article (411).
    /// Original: `HTLC_HDR_NEWS_DELETETHREAD`.
    NewsDeletethread = 411,
    /// Idle keep-alive ping (500).
    /// Original: `HTLC_HDR_PING`.
    Ping = 500,
}

impl ClientOpcode {
    /// Map a wire value to a known client opcode, or `None` if this build
    /// doesn't recognize it (the catch-all a `match` should fall through
    /// to, since this enum is `#[non_exhaustive]`).
    pub const fn from_u32(v: u32) -> Option<Self> {
        use ClientOpcode::*;
        Some(match v {
            101 => NewsfileGet,
            103 => NewsfilePost,
            105 => Chat,
            107 => Login,
            108 => Msg,
            110 => UserKick,
            112 => ChatCreate,
            113 => ChatInvite,
            114 => ChatDecline,
            115 => ChatJoin,
            116 => ChatPart,
            120 => ChatSubject,
            121 => Agreed,
            200 => FileList,
            202 => FileGet,
            203 => FilePut,
            204 => FileDelete,
            205 => FileMkdir,
            206 => FileGetinfo,
            207 => FileSetnfo,
            208 => FileMove,
            209 => FileSymlink,
            300 => UserGetlist,
            303 => UserGetinfo,
            304 => UserChange,
            350 => AccountCreate,
            351 => AccountDelete,
            352 => AccountRead,
            353 => AccountModify,
            355 => MessageBroadcast,
            370 => NewsListdir,
            371 => NewsListcategory,
            380 => NewsDelete,
            381 => NewsMkdir,
            382 => NewsMkcategory,
            400 => NewsGetthread,
            410 => NewsPostthread,
            411 => NewsDeletethread,
            500 => Ping,
            _ => return None,
        })
    }

    /// The wire `type` value.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl std::fmt::Debug for ClientOpcode {
    /// Prints the variant name and the original `HTLC_HDR_*` constant
    /// name, e.g. `Login (HTLC_HDR_LOGIN)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = self.as_u32();
        let name = match self {
            Self::NewsfileGet => "HTLC_HDR_NEWSFILE_GET",
            Self::NewsfilePost => "HTLC_HDR_NEWSFILE_POST",
            Self::Chat => "HTLC_HDR_CHAT",
            Self::Login => "HTLC_HDR_LOGIN",
            Self::Msg => "HTLC_HDR_MSG",
            Self::UserKick => "HTLC_HDR_USER_KICK",
            Self::ChatCreate => "HTLC_HDR_CHAT_CREATE",
            Self::ChatInvite => "HTLC_HDR_CHAT_INVITE",
            Self::ChatDecline => "HTLC_HDR_CHAT_DECLINE",
            Self::ChatJoin => "HTLC_HDR_CHAT_JOIN",
            Self::ChatPart => "HTLC_HDR_CHAT_PART",
            Self::ChatSubject => "HTLC_HDR_CHAT_SUBJECT",
            Self::Agreed => "HTLC_HDR_AGREED",
            Self::FileList => "HTLC_HDR_FILE_LIST",
            Self::FileGet => "HTLC_HDR_FILE_GET",
            Self::FilePut => "HTLC_HDR_FILE_PUT",
            Self::FileDelete => "HTLC_HDR_FILE_DELETE",
            Self::FileMkdir => "HTLC_HDR_FILE_MKDIR",
            Self::FileGetinfo => "HTLC_HDR_FILE_GETINFO",
            Self::FileSetnfo => "HTLC_HDR_FILE_SETINFO",
            Self::FileMove => "HTLC_HDR_FILE_MOVE",
            Self::FileSymlink => "HTLC_HDR_FILE_SYMLINK",
            Self::UserGetlist => "HTLC_HDR_USER_GETLIST",
            Self::UserGetinfo => "HTLC_HDR_USER_GETINFO",
            Self::UserChange => "HTLC_HDR_USER_CHANGE",
            Self::AccountCreate => "HTLC_HDR_ACCOUNT_CREATE",
            Self::AccountDelete => "HTLC_HDR_ACCOUNT_DELETE",
            Self::AccountRead => "HTLC_HDR_ACCOUNT_READ",
            Self::AccountModify => "HTLC_HDR_ACCOUNT_MODIFY",
            Self::MessageBroadcast => "HTLC_HDR_MSG_BROADCAST",
            Self::NewsListdir => "HTLC_HDR_NEWS_LISTDIR",
            Self::NewsListcategory => "HTLC_HDR_NEWS_LISTCATEGORY",
            Self::NewsDelete => "HTLC_HDR_NEWS_DELETE",
            Self::NewsMkdir => "HTLC_HDR_NEWS_MKDIR",
            Self::NewsMkcategory => "HTLC_HDR_NEWS_MKCATEGORY",
            Self::NewsGetthread => "HTLC_HDR_NEWS_GETTHREAD",
            Self::NewsPostthread => "HTLC_HDR_NEWS_POSTTHREAD",
            Self::NewsDeletethread => "HTLC_HDR_NEWS_DELETETHREAD",
            Self::Ping => "HTLC_HDR_PING",
        };
        write!(f, "{value} {name}")
    }
}

/// Server -> client transaction opcodes: unsolicited pushes (chat relay,
/// user list changes, news posts, ...). Replies to a specific client
/// request are *not* represented here; see [`TASK_REPLY`] and
/// [`Opcode::Reply`].
#[repr(u32)]
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ServerOpcode {
    /// A new (non-threaded) news post arrived (102).
    /// Original: `HTLS_HDR_NEWSFILE_POST`.
    NewsfilePost = 102,
    /// A message from the server/admin/another user (104).
    /// Original: `HTLS_HDR_MSG`.
    Msg = 104,
    /// A chat message to relay (106).
    /// Original: `HTLS_HDR_CHAT`.
    Chat = 106,
    /// The server's usage agreement text, part of login (109).
    /// Original: `HTLS_HDR_AGREEMENT`.
    Agreement = 109,
    /// The server is disconnecting this client, with a reason (111).
    /// Original: `HTLS_HDR_POLITEQUIT`.
    Politequit = 111,
    /// This client was invited to a private chat room (113).
    /// Original: `HTLS_HDR_CHAT_INVITE`.
    ChatInvite = 113,
    /// A user joined a private chat room (117).
    /// Original: `HTLS_HDR_CHAT_USER_CHANGE`.
    ChatUserChange = 117,
    /// A user left a private chat room (118).
    /// Original: `HTLS_HDR_CHAT_USER_PART`.
    ChatUserPart = 118,
    /// A private chat room's subject changed (119).
    /// Original: `HTLS_HDR_CHAT_SUBJECT`.
    ChatSubject = 119,
    /// A user's info changed, or a user joined (301).
    /// Original: `HTLS_HDR_USER_CHANGE`.
    UserChange = 301,
    /// A user disconnected (302).
    /// Original: `HTLS_HDR_USER_PART`.
    UserPart = 302,
    /// This client's access privileges were (re)set (354).
    /// Original: `HTLS_HDR_USER_SELFINFO`.
    UserSelfinfo = 354,
}

impl ServerOpcode {
    /// Map a wire value to a known server-push opcode, or `None`.
    pub const fn from_u32(v: u32) -> Option<Self> {
        use ServerOpcode::*;
        Some(match v {
            102 => NewsfilePost,
            104 => Msg,
            106 => Chat,
            109 => Agreement,
            111 => Politequit,
            113 => ChatInvite,
            117 => ChatUserChange,
            118 => ChatUserPart,
            119 => ChatSubject,
            301 => UserChange,
            302 => UserPart,
            354 => UserSelfinfo,
            _ => return None,
        })
    }

    /// The wire `type` value.
    pub const fn as_u32(self) -> u32 {
        self as u32
    }
}

impl std::fmt::Debug for ServerOpcode {
    /// Prints the variant name and the original `HTLS_HDR_*` constant
    /// name, e.g. `Chat (HTLS_HDR_CHAT)`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = self.as_u32();
        let name = match self {
            Self::NewsfilePost => "HTLS_HDR_NEWSFILE_POST",
            Self::Msg => "HTLS_HDR_MSG",
            Self::Chat => "HTLS_HDR_CHAT",
            Self::Agreement => "HTLS_HDR_AGREEMENT",
            Self::Politequit => "HTLS_HDR_POLITEQUIT",
            Self::ChatInvite => "HTLS_HDR_CHAT_INVITE",
            Self::ChatUserChange => "HTLS_HDR_CHAT_USER_CHANGE",
            Self::ChatUserPart => "HTLS_HDR_CHAT_USER_PART",
            Self::ChatSubject => "HTLS_HDR_CHAT_SUBJECT",
            Self::UserChange => "HTLS_HDR_USER_CHANGE",
            Self::UserPart => "HTLS_HDR_USER_PART",
            Self::UserSelfinfo => "HTLS_HDR_USER_SELFINFO",
        };
        write!(f, "{value} {name}")
    }
}

/// A decoded header `type` value: either the generic reply sentinel, a
/// recognized client request opcode, a recognized server push opcode, or
/// something this build doesn't know about yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    /// This transaction is a reply to an earlier request (see
    /// [`TASK_REPLY`]); match it back to the request by transaction ID.
    Reply,
    /// A client request opcode.
    Request(ClientOpcode),
    /// An unsolicited server push.
    Push(ServerOpcode),
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    Unknown(u32),
}

impl Opcode {
    /// Classify a raw wire `type` value known to have been sent **by a
    /// client**. Never consults [`ServerOpcode`]'s table; see the
    /// [module docs](self) for why direction can't be guessed.
    pub const fn from_client_wire(v: u32) -> Self {
        match ClientOpcode::from_u32(v) {
            Some(c) => Opcode::Request(c),
            None => Opcode::Unknown(v),
        }
    }

    /// Classify a raw wire `type` value known to have been sent **by a
    /// server**: the reply sentinel first, then a recognized push, else
    /// `Unknown`. Never consults [`ClientOpcode`]'s table.
    pub const fn from_server_wire(v: u32) -> Self {
        if v & 0xFFFF_0000 == TASK_REPLY {
            return Opcode::Reply;
        }
        match ServerOpcode::from_u32(v) {
            Some(s) => Opcode::Push(s),
            None => Opcode::Unknown(v),
        }
    }

    /// The wire `type` value this opcode encodes to.
    pub const fn as_u32(self) -> u32 {
        match self {
            Opcode::Reply => TASK_REPLY,
            Opcode::Request(c) => c.as_u32(),
            Opcode::Push(s) => s.as_u32(),
            Opcode::Unknown(v) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_reply_sentinel_is_recognized_exactly() {
        assert_eq!(Opcode::from_server_wire(TASK_REPLY), Opcode::Reply);
    }

    #[test]
    fn buggy_server_task_reply_with_opcode_in_low_bits_still_matches() {
        let buggy = TASK_REPLY | ClientOpcode::Login.as_u32();
        assert_eq!(Opcode::from_server_wire(buggy), Opcode::Reply);
    }

    #[test]
    fn known_client_and_server_opcodes_round_trip() {
        assert_eq!(
            Opcode::from_client_wire(ClientOpcode::Login.as_u32()),
            Opcode::Request(ClientOpcode::Login)
        );
        assert_eq!(
            Opcode::from_server_wire(ServerOpcode::Chat.as_u32()),
            Opcode::Push(ServerOpcode::Chat)
        );
    }

    #[test]
    fn unrecognized_opcode_is_preserved_not_dropped() {
        assert_eq!(Opcode::from_client_wire(0xDEAD), Opcode::Unknown(0xDEAD));
        assert_eq!(Opcode::from_server_wire(0xDEAD), Opcode::Unknown(0xDEAD));
    }

    /// `113` (`HTLC_HDR_CHAT_INVITE`/`HTLS_HDR_CHAT_INVITE`) exists in
    /// both tables; only the caller knows which one applies.
    #[test]
    fn the_same_wire_value_means_different_things_depending_on_direction() {
        assert_eq!(
            Opcode::from_client_wire(113),
            Opcode::Request(ClientOpcode::ChatInvite)
        );
        assert_eq!(
            Opcode::from_server_wire(113),
            Opcode::Push(ServerOpcode::ChatInvite)
        );
    }
}
