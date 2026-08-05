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
//! Variant names track the tail of the original wire constant; each
//! enum's `opcode_names!` invocation pairs a variant with that original
//! name once, and `Debug` reads it back via [`ClientOpcode::original_name`]/
//! [`ServerOpcode::original_name`] rather than storing the string a
//! second time.

use num_enum::{FromPrimitive, IntoPrimitive};

/// Sentinel opcode value every reply to a specific request carries in the
/// header's type field, instead of echoing `0` or the original request's
/// opcode. See the module docs for how real (and buggy) servers use this.
const TASK_REPLY: u32 = 0x0001_0000;

/// Declares a `const fn original_name(&self) -> &'static str` from a list
/// of `(ORIGINAL_WIRE_NAME, Variant)` pairs, so `Debug` impls don't need
/// to store each variant's original wire constant name a second time.
macro_rules! opcode_names {
    ($name:ident, $(($orig:ident, $variant:ident)),* $(,)?) => {
        impl $name {
            const fn original_name(&self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($orig),)*
                    Self::Unknown(_) => "(unrecognized)",
                }
            }
        }
    };
}

/// Client -> server transaction opcodes.
#[repr(u32)]
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
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
    FileSetinfo = 207,
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
    MsgBroadcast = 355,
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
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u32),
}

opcode_names!(
    ClientOpcode,
    (HTLC_HDR_NEWSFILE_GET, NewsfileGet),
    (HTLC_HDR_NEWSFILE_POST, NewsfilePost),
    (HTLC_HDR_CHAT, Chat),
    (HTLC_HDR_LOGIN, Login),
    (HTLC_HDR_MSG, Msg),
    (HTLC_HDR_USER_KICK, UserKick),
    (HTLC_HDR_CHAT_CREATE, ChatCreate),
    (HTLC_HDR_CHAT_INVITE, ChatInvite),
    (HTLC_HDR_CHAT_DECLINE, ChatDecline),
    (HTLC_HDR_CHAT_JOIN, ChatJoin),
    (HTLC_HDR_CHAT_PART, ChatPart),
    (HTLC_HDR_CHAT_SUBJECT, ChatSubject),
    (HTLC_HDR_AGREED, Agreed),
    (HTLC_HDR_FILE_LIST, FileList),
    (HTLC_HDR_FILE_GET, FileGet),
    (HTLC_HDR_FILE_PUT, FilePut),
    (HTLC_HDR_FILE_DELETE, FileDelete),
    (HTLC_HDR_FILE_MKDIR, FileMkdir),
    (HTLC_HDR_FILE_GETINFO, FileGetinfo),
    (HTLC_HDR_FILE_SETINFO, FileSetinfo),
    (HTLC_HDR_FILE_MOVE, FileMove),
    (HTLC_HDR_FILE_SYMLINK, FileSymlink),
    (HTLC_HDR_USER_GETLIST, UserGetlist),
    (HTLC_HDR_USER_GETINFO, UserGetinfo),
    (HTLC_HDR_USER_CHANGE, UserChange),
    (HTLC_HDR_ACCOUNT_CREATE, AccountCreate),
    (HTLC_HDR_ACCOUNT_DELETE, AccountDelete),
    (HTLC_HDR_ACCOUNT_READ, AccountRead),
    (HTLC_HDR_ACCOUNT_MODIFY, AccountModify),
    (HTLC_HDR_MSG_BROADCAST, MsgBroadcast),
    (HTLC_HDR_NEWS_LISTDIR, NewsListdir),
    (HTLC_HDR_NEWS_LISTCATEGORY, NewsListcategory),
    (HTLC_HDR_NEWS_DELETE, NewsDelete),
    (HTLC_HDR_NEWS_MKDIR, NewsMkdir),
    (HTLC_HDR_NEWS_MKCATEGORY, NewsMkcategory),
    (HTLC_HDR_NEWS_GETTHREAD, NewsGetthread),
    (HTLC_HDR_NEWS_POSTTHREAD, NewsPostthread),
    (HTLC_HDR_NEWS_DELETETHREAD, NewsDeletethread),
    (HTLC_HDR_PING, Ping),
);

impl std::fmt::Debug for ClientOpcode {
    /// Prints the wire value and the original constant name, e.g.
    /// `107 HTLC_HDR_LOGIN`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", u32::from(*self), self.original_name())
    }
}

/// Server -> client transaction opcodes: unsolicited pushes (chat relay,
/// user list changes, news posts, ...). Replies to a specific client
/// request are *not* represented here; see [`TASK_REPLY`] and
/// [`Opcode::Reply`].
#[non_exhaustive]
#[derive(Clone, Copy, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u32)]
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
    /// A message broadcast to every connected user (355).
    /// Original: `HTLS_HDR_MSG_BROADCAST`.
    MsgBroadcast = 355,
    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u32),
}

opcode_names!(
    ServerOpcode,
    (HTLS_HDR_NEWSFILE_POST, NewsfilePost),
    (HTLS_HDR_MSG, Msg),
    (HTLS_HDR_CHAT, Chat),
    (HTLS_HDR_AGREEMENT, Agreement),
    (HTLS_HDR_POLITEQUIT, Politequit),
    (HTLS_HDR_CHAT_INVITE, ChatInvite),
    (HTLS_HDR_CHAT_USER_CHANGE, ChatUserChange),
    (HTLS_HDR_CHAT_USER_PART, ChatUserPart),
    (HTLS_HDR_CHAT_SUBJECT, ChatSubject),
    (HTLS_HDR_USER_CHANGE, UserChange),
    (HTLS_HDR_USER_PART, UserPart),
    (HTLS_HDR_USER_SELFINFO, UserSelfinfo),
    (HTLS_HDR_MSG_BROADCAST, MsgBroadcast),
);

impl std::fmt::Debug for ServerOpcode {
    /// Prints the wire value and the original constant name, e.g.
    /// `106 HTLS_HDR_CHAT`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", u32::from(*self), self.original_name())
    }
}

/// A decoded header `type` value: either the generic reply sentinel, a
/// client request opcode, or a server push opcode (unrecognized wire
/// values still resolve to a direction via
/// [`ClientOpcode::Unknown`]/[`ServerOpcode::Unknown`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opcode {
    /// This transaction is a reply to an earlier request (see
    /// [`TASK_REPLY`]); match it back to the request by transaction ID.
    Reply,
    /// A client request opcode.
    Request(ClientOpcode),
    /// An unsolicited server push.
    Push(ServerOpcode),
}

impl Opcode {
    /// Classify a raw wire `type` value known to have been sent **by a
    /// client**. Never consults [`ServerOpcode`]'s table; see the
    /// [module docs](self) for why direction can't be guessed.
    pub fn from_client_wire(v: u32) -> Self {
        Opcode::Request(ClientOpcode::from(v))
    }

    /// Classify a raw wire `type` value known to have been sent **by a
    /// server**: the reply sentinel first, then a push. Never consults
    /// [`ClientOpcode`]'s table.
    pub fn from_server_wire(v: u32) -> Self {
        if v & 0xFFFF_0000 == TASK_REPLY {
            return Opcode::Reply;
        }
        Opcode::Push(ServerOpcode::from(v))
    }

    /// The wire `type` value this opcode encodes to.
    pub fn as_u32(self) -> u32 {
        match self {
            Opcode::Reply => TASK_REPLY,
            Opcode::Request(c) => u32::from(c),
            Opcode::Push(s) => u32::from(s),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_reply_sentinel_is_recognized_exactly() {
        assert_eq!(
            Opcode::from_server_wire(TASK_REPLY),
            Opcode::Reply
        );
    }

    #[test]
    fn buggy_server_task_reply_with_opcode_in_low_bits_still_matches() {
        let buggy = TASK_REPLY | u32::from(ClientOpcode::Login);
        assert_eq!(
            Opcode::from_server_wire(buggy),
            Opcode::Reply
        );
    }

    #[test]
    fn known_client_and_server_opcodes_round_trip() {
        assert_eq!(
            Opcode::from_client_wire(u32::from(ClientOpcode::Login)),
            Opcode::Request(ClientOpcode::Login)
        );
        assert_eq!(
            Opcode::from_server_wire(u32::from(ServerOpcode::Chat)),
            Opcode::Push(ServerOpcode::Chat)
        );
    }

    #[test]
    fn unrecognized_opcode_is_preserved_not_dropped() {
        assert_eq!(
            Opcode::from_client_wire(0xDEAD),
            Opcode::Request(ClientOpcode::Unknown(0xDEAD))
        );
        assert_eq!(
            Opcode::from_server_wire(0xDEAD),
            Opcode::Push(ServerOpcode::Unknown(0xDEAD))
        );
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

    /// `Debug`'s reconstructed name must match every opcode's real
    /// original wire constant, not just look plausible.
    #[test]
    fn debug_reconstructs_every_client_opcodes_original_name() {
        let cases = [
            (ClientOpcode::NewsfileGet, "101 HTLC_HDR_NEWSFILE_GET"),
            (ClientOpcode::FileSetinfo, "207 HTLC_HDR_FILE_SETINFO"),
            (ClientOpcode::UserGetlist, "300 HTLC_HDR_USER_GETLIST"),
            (
                ClientOpcode::NewsListcategory,
                "371 HTLC_HDR_NEWS_LISTCATEGORY",
            ),
            (ClientOpcode::Ping, "500 HTLC_HDR_PING"),
        ];
        for (opcode, expected) in cases {
            assert_eq!(format!("{opcode:?}"), expected);
        }
        assert_eq!(
            format!("{:?}", ClientOpcode::Unknown(0xDEAD)),
            "57005 (unrecognized)"
        );
    }

    #[test]
    fn debug_reconstructs_every_server_opcodes_original_name() {
        let cases = [
            (
                ServerOpcode::ChatUserChange,
                "117 HTLS_HDR_CHAT_USER_CHANGE",
            ),
            (ServerOpcode::UserSelfinfo, "354 HTLS_HDR_USER_SELFINFO"),
            (ServerOpcode::MsgBroadcast, "355 HTLS_HDR_MSG_BROADCAST"),
        ];
        for (opcode, expected) in cases {
            assert_eq!(format!("{opcode:?}"), expected);
        }
    }
}
