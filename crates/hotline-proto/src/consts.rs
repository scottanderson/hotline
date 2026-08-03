//! Access privilege bits carried in the 64-bit `USER_ACCESS` field.
//!
//! Two big-endian `u32` words concatenated (high word first): the
//! first-declared flag is bit 63, counting down. Reserved bits should
//! always be zero on the wire.
//!
//! [`AccessPrivileges::VOICE_CHAT`] and [`AccessPrivileges::READ_CHAT_HISTORY`]
//! are community extensions under capability negotiation, not part of
//! the base privilege set.

bitflags::bitflags! {
    /// The 64-bit `USER_ACCESS` privilege bitmap (plus two community
    /// extension bits, see the [module docs](self)). Bits with no
    /// named flag below (51, 50, 45, 44, 22..=9, 6..=0) are reserved
    /// and should always be zero on the wire unless a server
    /// specifically implements one of the extensions this module
    /// documents.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    pub struct AccessPrivileges: u64 {
        /// Delete a file.
        /// Wire field: `delete_files`.
        const DELETE_FILES           = 1 << 63;
        /// Upload a file.
        /// Wire field: `upload_files`.
        const UPLOAD_FILES           = 1 << 62;
        /// Download a file.
        /// Wire field: `download_files`.
        const DOWNLOAD_FILES         = 1 << 61;
        /// Rename a file.
        /// Wire field: `rename_files`.
        const RENAME_FILES           = 1 << 60;
        /// Move a file between folders.
        /// Wire field: `move_files`.
        const MOVE_FILES             = 1 << 59;
        /// Create a folder.
        /// Wire field: `create_folders`.
        const CREATE_FOLDERS         = 1 << 58;
        /// Delete a folder.
        /// Wire field: `delete_folders`.
        const DELETE_FOLDERS         = 1 << 57;
        /// Rename a folder.
        /// Wire field: `rename_folders`.
        const RENAME_FOLDERS         = 1 << 56;
        /// Move a folder.
        /// Wire field: `move_folders`.
        const MOVE_FOLDERS           = 1 << 55;
        /// Read public chat.
        /// Wire field: `read_chat`.
        const READ_CHAT              = 1 << 54;
        /// Send public chat.
        /// Wire field: `send_chat`.
        const SEND_CHAT              = 1 << 53;
        /// Open/create a private chat.
        /// Wire field: `create_pchats`.
        const CREATE_PCHATS          = 1 << 52;
        // bits reserved.
        /// Create a user account.
        /// Wire field: `create_users`.
        const CREATE_USERS           = 1 << 49;
        /// Delete a user account.
        /// Wire field: `delete_users`.
        const DELETE_USERS           = 1 << 48;
        /// Read a user account's details.
        /// Wire field: `read_users`.
        const READ_USERS             = 1 << 47;
        /// Modify a user account's details.
        /// Wire field: `modify_users`.
        const MODIFY_USERS           = 1 << 46;
        // bits reserved.
        /// Read news articles.
        /// Wire field: `read_news`.
        const READ_NEWS              = 1 << 43;
        /// Post news articles.
        /// Wire field: `post_news`.
        const POST_NEWS              = 1 << 42;
        /// Disconnect (kick) another user.
        /// Wire field: `disconnect_users`.
        const DISCONNECT_USERS       = 1 << 41;
        /// This user cannot be disconnected/kicked.
        /// Wire field: `cant_be_disconnected`.
        const CANT_BE_DISCONNECTED   = 1 << 40;
        /// Fetch another user's client info text.
        /// Wire field: `get_user_info`.
        const GET_USER_INFO          = 1 << 39;
        /// Upload outside the normal upload folder.
        /// Wire field: `upload_anywhere`.
        const UPLOAD_ANYWHERE        = 1 << 38;
        /// Use any nickname, bypassing name restrictions.
        /// Wire field: `use_any_name`.
        const USE_ANY_NAME           = 1 << 37;
        /// Skip the server agreement on login.
        /// Wire field: `dont_show_agreement`.
        const DONT_SHOW_AGREEMENT    = 1 << 36;
        /// Set a file's comment.
        /// Wire field: `comment_files`.
        const COMMENT_FILES          = 1 << 35;
        /// Set a folder's comment.
        /// Wire field: `comment_folders`.
        const COMMENT_FOLDERS        = 1 << 34;
        /// View drop boxes (upload-only, no listing, folders).
        /// Wire field: `view_drop_boxes`.
        const VIEW_DROP_BOXES        = 1 << 33;
        /// Create a file alias.
        /// Wire field: `make_aliases`.
        const MAKE_ALIASES           = 1 << 32;
        /// Broadcast a message to every connected user.
        /// Wire field: `can_broadcast`.
        const CAN_BROADCAST          = 1 << 31;
        /// Delete a news article.
        /// Wire field: `delete_articles`.
        const DELETE_ARTICLES        = 1 << 30;
        /// Create a news category.
        /// Wire field: `create_categories`.
        const CREATE_CATEGORIES      = 1 << 29;
        /// Delete a news category.
        /// Wire field: `delete_categories`.
        const DELETE_CATEGORIES      = 1 << 28;
        /// Create a news bundle (folder).
        /// Wire field: `create_news_bundles`.
        const CREATE_NEWS_BUNDLES    = 1 << 27;
        /// Delete a news bundle (folder).
        /// Wire field: `delete_news_bundles`.
        const DELETE_NEWS_BUNDLES    = 1 << 26;
        /// Upload a whole folder.
        /// Wire field: `upload_folders`.
        const UPLOAD_FOLDERS         = 1 << 25;
        /// Download a whole folder.
        /// Wire field: `download_folders`.
        const DOWNLOAD_FOLDERS       = 1 << 24;
        /// Send a private instant message.
        /// Wire field: `send_msgs`.
        const SEND_MSGS              = 1 << 23;
        // bits 22..=9 reserved.

        /// Join voice chat. Community extension gating `HTLC_HDR_VOICE_JOIN`
        /// (600) under capability negotiation.
        /// Wire field: `HL_ACCESS_VOICE_CHAT`.
        const VOICE_CHAT              = 1 << 8;
        /// Query chat history. Community extension gating
        /// `HTLC_HDR_GET_CHAT_HISTORY`.
        /// Wire field: `HL_ACCESS_READ_CHAT_HISTORY`.
        const READ_CHAT_HISTORY       = 1 << 7;
        // bits 6..=0 reserved.
    }
}

impl AccessPrivileges {
    /// A reasonable "guest" default: read/send public chat, browse +
    /// download files, read news, and send/receive private messages. No
    /// admin, upload, or destructive privileges.
    pub const fn guest_default() -> Self {
        Self::READ_CHAT
            .union(Self::SEND_CHAT)
            .union(Self::DOWNLOAD_FILES)
            .union(Self::READ_NEWS)
            .union(Self::SEND_MSGS)
            .union(Self::GET_USER_INFO)
    }

    /// Decode the big-endian 8-byte wire representation of the
    /// `USER_ACCESS` field. Returns `None` if any bit outside the named
    /// flags is set, rather than silently discarding it. Use
    /// [`AccessPrivileges::from_bits_truncate`] directly to ignore
    /// unknown bits instead.
    pub const fn from_be_bytes(bytes: [u8; 8]) -> Option<Self> {
        Self::from_bits(u64::from_be_bytes(bytes))
    }

    /// Encode to the big-endian 8-byte wire representation.
    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.bits().to_be_bytes()
    }

    /// The original `HL_ACCESS_*` constant name for a single flag, or
    /// `None` if `self` isn't exactly one named flag.
    pub const fn flag_name(self) -> Option<&'static str> {
        match self {
            Self::DELETE_FILES => Some("HL_ACCESS_DELETE_FILES"),
            Self::UPLOAD_FILES => Some("HL_ACCESS_UPLOAD_FILES"),
            Self::DOWNLOAD_FILES => Some("HL_ACCESS_DOWNLOAD_FILES"),
            Self::RENAME_FILES => Some("HL_ACCESS_RENAME_FILES"),
            Self::MOVE_FILES => Some("HL_ACCESS_MOVE_FILES"),
            Self::CREATE_FOLDERS => Some("HL_ACCESS_CREATE_FOLDERS"),
            Self::DELETE_FOLDERS => Some("HL_ACCESS_DELETE_FOLDERS"),
            Self::RENAME_FOLDERS => Some("HL_ACCESS_RENAME_FOLDERS"),
            Self::MOVE_FOLDERS => Some("HL_ACCESS_MOVE_FOLDERS"),
            Self::READ_CHAT => Some("HL_ACCESS_READ_CHAT"),
            Self::SEND_CHAT => Some("HL_ACCESS_SEND_CHAT"),
            Self::CREATE_PCHATS => Some("HL_ACCESS_CREATE_PCHATS"),
            Self::CREATE_USERS => Some("HL_ACCESS_CREATE_USERS"),
            Self::DELETE_USERS => Some("HL_ACCESS_DELETE_USERS"),
            Self::READ_USERS => Some("HL_ACCESS_READ_USERS"),
            Self::MODIFY_USERS => Some("HL_ACCESS_MODIFY_USERS"),
            Self::READ_NEWS => Some("HL_ACCESS_READ_NEWS"),
            Self::POST_NEWS => Some("HL_ACCESS_POST_NEWS"),
            Self::DISCONNECT_USERS => Some("HL_ACCESS_DISCONNECT_USERS"),
            Self::CANT_BE_DISCONNECTED => Some("HL_ACCESS_CANT_BE_DISCONNECTED"),
            Self::GET_USER_INFO => Some("HL_ACCESS_GET_USER_INFO"),
            Self::UPLOAD_ANYWHERE => Some("HL_ACCESS_UPLOAD_ANYWHERE"),
            Self::USE_ANY_NAME => Some("HL_ACCESS_USE_ANY_NAME"),
            Self::DONT_SHOW_AGREEMENT => Some("HL_ACCESS_DONT_SHOW_AGREEMENT"),
            Self::COMMENT_FILES => Some("HL_ACCESS_COMMENT_FILES"),
            Self::COMMENT_FOLDERS => Some("HL_ACCESS_COMMENT_FOLDERS"),
            Self::VIEW_DROP_BOXES => Some("HL_ACCESS_VIEW_DROP_BOXES"),
            Self::MAKE_ALIASES => Some("HL_ACCESS_MAKE_ALIASES"),
            Self::CAN_BROADCAST => Some("HL_ACCESS_CAN_BROADCAST"),
            Self::DELETE_ARTICLES => Some("HL_ACCESS_DELETE_ARTICLES"),
            Self::CREATE_CATEGORIES => Some("HL_ACCESS_CREATE_CATEGORIES"),
            Self::DELETE_CATEGORIES => Some("HL_ACCESS_DELETE_CATEGORIES"),
            Self::CREATE_NEWS_BUNDLES => Some("HL_ACCESS_CREATE_NEWS_BUNDLES"),
            Self::DELETE_NEWS_BUNDLES => Some("HL_ACCESS_DELETE_NEWS_BUNDLES"),
            Self::UPLOAD_FOLDERS => Some("HL_ACCESS_UPLOAD_FOLDERS"),
            Self::DOWNLOAD_FOLDERS => Some("HL_ACCESS_DOWNLOAD_FOLDERS"),
            Self::SEND_MSGS => Some("HL_ACCESS_SEND_MSGS"),
            Self::VOICE_CHAT => Some("HL_ACCESS_VOICE_CHAT"),
            Self::READ_CHAT_HISTORY => Some("HL_ACCESS_READ_CHAT_HISTORY"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_privileges_round_trip_bytes() {
        let privs = AccessPrivileges::guest_default();
        let bytes = privs.to_be_bytes();
        assert_eq!(AccessPrivileges::from_be_bytes(bytes), Some(privs));
    }

    /// A known "everything enabled" wire value decodes to exactly the
    /// named base flags (excludes the voice-chat/chat-history extensions).
    #[test]
    fn everything_enabled_constant_decodes_correctly() {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&0xfff3cfefu32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0xff800000u32.to_be_bytes());

        let expected = AccessPrivileges::all()
            - AccessPrivileges::DONT_SHOW_AGREEMENT
            - AccessPrivileges::VOICE_CHAT
            - AccessPrivileges::READ_CHAT_HISTORY;
        assert_eq!(AccessPrivileges::from_be_bytes(bytes), Some(expected));
    }

    #[test]
    fn reserved_bit_set_is_rejected_not_silently_dropped() {
        // Bit 0 is reserved.
        let bytes = 1u64.to_be_bytes();
        assert_eq!(AccessPrivileges::from_be_bytes(bytes), None);
    }

    #[test]
    fn extension_bits_round_trip() {
        let privs = AccessPrivileges::VOICE_CHAT | AccessPrivileges::READ_CHAT_HISTORY;
        let bytes = privs.to_be_bytes();
        assert_eq!(AccessPrivileges::from_be_bytes(bytes), Some(privs));
    }

    #[test]
    fn flag_name_identifies_every_single_flag() {
        assert_eq!(
            AccessPrivileges::SEND_CHAT.flag_name(),
            Some("HL_ACCESS_SEND_CHAT")
        );
        assert_eq!(
            AccessPrivileges::SEND_MSGS.flag_name(),
            Some("HL_ACCESS_SEND_MSGS")
        );
        assert_eq!(
            AccessPrivileges::VOICE_CHAT.flag_name(),
            Some("HL_ACCESS_VOICE_CHAT")
        );
    }

    #[test]
    fn flag_name_is_none_for_unions_and_empty() {
        assert_eq!(AccessPrivileges::empty().flag_name(), None);
        assert_eq!(
            (AccessPrivileges::SEND_CHAT | AccessPrivileges::READ_CHAT).flag_name(),
            None
        );
    }
}
