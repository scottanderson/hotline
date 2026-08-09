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
    /// The 64-bit `USER_ACCESS` privilege bitmap. Bits 51, 50, 45, and 44 are
    /// named, but have no use. Bits 22..=0 are reserved.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
    #[derive(serde::Serialize, serde::Deserialize)]
    pub struct AccessPrivileges: u64 {
        /// Delete a file.
        const DELETE_FILE           = 1 << 63;
        /// Upload a file.
        const UPLOAD_FILE           = 1 << 62;
        /// Download a file.
        const DOWNLOAD_FILE         = 1 << 61;
        /// Rename a file.
        const RENAME_FILE           = 1 << 60;
        /// Move a file between folders.
        const MOVE_FILE             = 1 << 59;
        /// Create a folder.
        const CREATE_FOLDER         = 1 << 58;
        /// Delete a folder.
        const DELETE_FOLDER         = 1 << 57;
        /// Rename a folder.
        const RENAME_FOLDER         = 1 << 56;
        /// Move a folder.
        const MOVE_FOLDER           = 1 << 55;
        /// Read public chat.
        const READ_CHAT             = 1 << 54;
        /// Send public chat.
        const SEND_CHAT             = 1 << 53;
        /// Create a private chat.
        const CREATE_CHAT           = 1 << 52;
        // Unused: Close chat.
        // const CLOSE_CHAT            = 1 << 51;
        // Unused: Show in list.
        // const SHOW_IN_LIST          = 1 << 50;
        /// Create a user account.
        const CREATE_USER           = 1 << 49;
        /// Delete a user account.
        const DELETE_USER           = 1 << 48;
        /// Read a user account's details.
        const OPEN_USER             = 1 << 47;
        /// Modify a user account's details.
        const MODIFY_USER           = 1 << 46;
        // Unused: Change own passowrd.
        // const CHANGE_OWN_PASS       = 1 << 45;
        // Unused: Send private message.
        // const SEND_PRIV_MSG         = 1 << 44;
        /// Read news articles.
        const NEWS_READ_ART         = 1 << 43;
        /// Post news articles.
        const POST_NEWS_ART         = 1 << 42;
        /// Disconnect (kick) another user.
        const DISCON_USER           = 1 << 41;
        /// This user cannot be disconnected/kicked.
        const CANNOT_BE_DISCON      = 1 << 40;
        /// Fetch another client's info text.
        const GET_CLIENT_INFO       = 1 << 39;
        /// Upload outside the normal upload folder.
        const UPLOAD_ANYWHERE       = 1 << 38;
        /// Use any nickname, bypassing name restrictions.
        const ANY_NAME              = 1 << 37;
        /// Skip the server agreement on login.
        const NO_AGREEMENT          = 1 << 36;
        /// Set a file's comment.
        const SET_FILE_COMMENT      = 1 << 35;
        /// Set a folder's comment.
        const SET_FOLDER_COMMENT    = 1 << 34;
        /// View drop boxes (upload-only, no listing, folders).
        const VIEW_DROP_BOXES       = 1 << 33;
        /// Create a file alias.
        const MAKE_ALIAS            = 1 << 32;
        /// Broadcast a message to every connected user.
        const BROADCAST             = 1 << 31;
        /// Delete a news article.
        const NEWS_DELETE_ART       = 1 << 30;
        /// Create a news category.
        const NEWS_CREATE_CAT       = 1 << 29;
        /// Delete a news category.
        const NEWS_DELETE_CAT       = 1 << 28;
        /// Create a news bundle (folder).
        const NEWS_CREATE_FLDR      = 1 << 27;
        /// Delete a news bundle (folder).
        const NEWS_DELETE_FLDR      = 1 << 26;
        /// Upload a whole folder.
        const UPLOAD_FOLDER         = 1 << 25;
        /// Download a whole folder.
        const DOWNLOAD_FOLDER       = 1 << 24;
        /// Send a private instant message.
        const SEND_MESSAGE          = 1 << 23;
        // bits 22..=0 reserved.
    }
}

impl Default for AccessPrivileges {
    /// A reasonable "guest" default: read/send public chat, browse,
    /// upload, and download files, read and post news, send and receive
    /// private messages. No admin or destructive privileges.
    fn default() -> Self {
        Self::READ_CHAT
            | Self::SEND_CHAT
            | Self::DOWNLOAD_FILE
            | Self::UPLOAD_FILE
            | Self::NEWS_READ_ART
            | Self::POST_NEWS_ART
            | Self::ANY_NAME
            | Self::SEND_MESSAGE
    }
}

impl AccessPrivileges {
    /// The constant name for a single flag, or `None` if `self` isn't
    /// exactly one named flag.
    pub const fn flag_name(self) -> Option<&'static str> {
        match self {
            Self::DELETE_FILE => Some("DELETE_FILE"),
            Self::UPLOAD_FILE => Some("UPLOAD_FILE"),
            Self::DOWNLOAD_FILE => Some("DOWNLOAD_FILE"),
            Self::RENAME_FILE => Some("RENAME_FILE"),
            Self::MOVE_FILE => Some("MOVE_FILE"),
            Self::CREATE_FOLDER => Some("CREATE_FOLDER"),
            Self::DELETE_FOLDER => Some("DELETE_FOLDER"),
            Self::RENAME_FOLDER => Some("RENAME_FOLDER"),
            Self::MOVE_FOLDER => Some("MOVE_FOLDER"),
            Self::READ_CHAT => Some("READ_CHAT"),
            Self::SEND_CHAT => Some("SEND_CHAT"),
            Self::CREATE_CHAT => Some("CREATE_CHAT"),
            Self::CREATE_USER => Some("CREATE_USER"),
            Self::DELETE_USER => Some("DELETE_USER"),
            Self::OPEN_USER => Some("OPEN_USER"),
            Self::MODIFY_USER => Some("MODIFY_USER"),
            Self::NEWS_READ_ART => Some("NEWS_READ_ART"),
            Self::POST_NEWS_ART => Some("POST_NEWS_ART"),
            Self::DISCON_USER => Some("DISCON_USER"),
            Self::CANNOT_BE_DISCON => Some("CANNOT_BE_DISCON"),
            Self::GET_CLIENT_INFO => Some("GET_CLIENT_INFO"),
            Self::UPLOAD_ANYWHERE => Some("UPLOAD_ANYWHERE"),
            Self::ANY_NAME => Some("ANY_NAME"),
            Self::NO_AGREEMENT => Some("NO_AGREEMENT"),
            Self::SET_FILE_COMMENT => Some("SET_FILE_COMMENT"),
            Self::SET_FOLDER_COMMENT => Some("SET_FOLDER_COMMENT"),
            Self::VIEW_DROP_BOXES => Some("VIEW_DROP_BOXES"),
            Self::MAKE_ALIAS => Some("MAKE_ALIAS"),
            Self::BROADCAST => Some("BROADCAST"),
            Self::NEWS_DELETE_ART => Some("NEWS_DELETE_ART"),
            Self::NEWS_CREATE_CAT => Some("NEWS_CREATE_CAT"),
            Self::NEWS_DELETE_CAT => Some("NEWS_DELETE_CAT"),
            Self::NEWS_CREATE_FLDR => Some("NEWS_CREATE_FLDR"),
            Self::NEWS_DELETE_FLDR => Some("NEWS_DELETE_FLDR"),
            Self::UPLOAD_FOLDER => Some("UPLOAD_FOLDER"),
            Self::DOWNLOAD_FOLDER => Some("DOWNLOAD_FOLDER"),
            Self::SEND_MESSAGE => Some("SEND_MESSAGE"),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_privileges_round_trip_bytes() {
        let privs = AccessPrivileges::default();
        let bytes = privs.bits().to_be_bytes();
        assert_eq!(
            AccessPrivileges::from_bits(u64::from_be_bytes(bytes)),
            Some(privs)
        );
    }

    /// A known "everything enabled" wire value decodes to exactly the
    /// named base flags (excludes the voice-chat/chat-history extensions).
    #[test]
    fn everything_enabled_constant_decodes_correctly() {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&0xfff3cfefu32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0xff800000u32.to_be_bytes());

        let expected = AccessPrivileges::all() - AccessPrivileges::NO_AGREEMENT;
        assert_eq!(
            AccessPrivileges::from_bits(u64::from_be_bytes(bytes)),
            Some(expected)
        );
    }

    #[test]
    fn reserved_bit_set_is_rejected_not_silently_dropped() {
        // Bit 0 is reserved.
        let bytes = 1u64.to_be_bytes();
        assert_eq!(AccessPrivileges::from_bits(u64::from_be_bytes(bytes)), None);
    }

    #[test]
    fn flag_name_identifies_every_single_flag() {
        for (p, n) in [
            (AccessPrivileges::DELETE_FILE, "DELETE_FILE"),
            (AccessPrivileges::UPLOAD_FILE, "UPLOAD_FILE"),
            (AccessPrivileges::DOWNLOAD_FILE, "DOWNLOAD_FILE"),
            (AccessPrivileges::RENAME_FILE, "RENAME_FILE"),
            (AccessPrivileges::MOVE_FILE, "MOVE_FILE"),
            (AccessPrivileges::CREATE_FOLDER, "CREATE_FOLDER"),
            (AccessPrivileges::DELETE_FOLDER, "DELETE_FOLDER"),
            (AccessPrivileges::RENAME_FOLDER, "RENAME_FOLDER"),
            (AccessPrivileges::MOVE_FOLDER, "MOVE_FOLDER"),
            (AccessPrivileges::READ_CHAT, "READ_CHAT"),
            (AccessPrivileges::SEND_CHAT, "SEND_CHAT"),
            (AccessPrivileges::CREATE_CHAT, "CREATE_CHAT"),
            (AccessPrivileges::CREATE_USER, "CREATE_USER"),
            (AccessPrivileges::DELETE_USER, "DELETE_USER"),
            (AccessPrivileges::OPEN_USER, "OPEN_USER"),
            (AccessPrivileges::MODIFY_USER, "MODIFY_USER"),
            (AccessPrivileges::NEWS_READ_ART, "NEWS_READ_ART"),
            (AccessPrivileges::POST_NEWS_ART, "POST_NEWS_ART"),
            (AccessPrivileges::DISCON_USER, "DISCON_USER"),
            (AccessPrivileges::CANNOT_BE_DISCON, "CANNOT_BE_DISCON"),
            (AccessPrivileges::GET_CLIENT_INFO, "GET_CLIENT_INFO"),
            (AccessPrivileges::UPLOAD_ANYWHERE, "UPLOAD_ANYWHERE"),
            (AccessPrivileges::ANY_NAME, "ANY_NAME"),
            (AccessPrivileges::NO_AGREEMENT, "NO_AGREEMENT"),
            (AccessPrivileges::SET_FILE_COMMENT, "SET_FILE_COMMENT"),
            (AccessPrivileges::SET_FOLDER_COMMENT, "SET_FOLDER_COMMENT"),
            (AccessPrivileges::VIEW_DROP_BOXES, "VIEW_DROP_BOXES"),
            (AccessPrivileges::MAKE_ALIAS, "MAKE_ALIAS"),
            (AccessPrivileges::BROADCAST, "BROADCAST"),
            (AccessPrivileges::NEWS_DELETE_ART, "NEWS_DELETE_ART"),
            (AccessPrivileges::NEWS_CREATE_CAT, "NEWS_CREATE_CAT"),
            (AccessPrivileges::NEWS_DELETE_CAT, "NEWS_DELETE_CAT"),
            (AccessPrivileges::NEWS_CREATE_FLDR, "NEWS_CREATE_FLDR"),
            (AccessPrivileges::NEWS_DELETE_FLDR, "NEWS_DELETE_FLDR"),
            (AccessPrivileges::UPLOAD_FOLDER, "UPLOAD_FOLDER"),
            (AccessPrivileges::DOWNLOAD_FOLDER, "DOWNLOAD_FOLDER"),
            (AccessPrivileges::SEND_MESSAGE, "SEND_MESSAGE"),
        ] {
            assert_eq!(p.flag_name(), Some(n));
        }
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
