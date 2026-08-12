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
    pub struct Access: u64 {
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

impl Default for Access {
    /// A reasonable "guest" default: read/send public chat, browse,
    /// upload, and download files, read and post news, send and receive
    /// private messages. No admin or destructive privileges.
    fn default() -> Self {
        Self::empty()
            | Self::UPLOAD_FILE
            | Self::DOWNLOAD_FILE
            | Self::READ_CHAT
            | Self::SEND_CHAT
            | Self::CREATE_CHAT
            | Self::NEWS_READ_ART
            | Self::POST_NEWS_ART
            | Self::ANY_NAME
            | Self::UPLOAD_FOLDER
            | Self::DOWNLOAD_FOLDER
            | Self::SEND_MESSAGE
    }
}

/// Extension trait for any `bitflags` type: a privilege check that names
/// the missing flags via the macro's own generated metadata rather than a
/// hand-maintained table.
pub trait RequireFlags: bitflags::Flags + Copy {
    /// `Err` names every flag `required` has that `self` doesn't, joined
    /// by `", "`.
    fn require(self, required: Self) -> Result<(), String> {
        let missing = required.difference(self);
        if missing.is_empty() {
            return Ok(());
        }
        let names = missing
            .iter_names()
            .map(|(name, _)| name)
            .collect::<Vec<_>>()
            .join(", ");
        let names = match names.is_empty() {
            true => "this privilege",
            false => &names,
        };
        Err(format!("You are not allowed to do that ({names})."))
    }
}

impl<T: bitflags::Flags + Copy> RequireFlags for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_privileges_round_trip_bytes() {
        let privs = Access::default();
        let bytes = privs.bits().to_be_bytes();
        assert_eq!(Access::from_bits(u64::from_be_bytes(bytes)), Some(privs));
    }

    /// A known "everything enabled" wire value decodes to exactly the
    /// named base flags (excludes the voice-chat/chat-history extensions).
    #[test]
    fn everything_enabled_constant_decodes_correctly() {
        let mut bytes = [0u8; 8];
        bytes[0..4].copy_from_slice(&0xfff3cfefu32.to_be_bytes());
        bytes[4..8].copy_from_slice(&0xff800000u32.to_be_bytes());

        let expected = Access::all() - Access::NO_AGREEMENT;
        assert_eq!(Access::from_bits(u64::from_be_bytes(bytes)), Some(expected));
    }

    #[test]
    fn reserved_bit_set_is_rejected_not_silently_dropped() {
        // Bit 0 is reserved.
        let bytes = 1u64.to_be_bytes();
        assert_eq!(Access::from_bits(u64::from_be_bytes(bytes)), None);
    }

    #[test]
    fn require_succeeds_when_every_required_flag_is_held() {
        let held = Access::READ_CHAT | Access::SEND_CHAT;
        assert!(held.require(Access::READ_CHAT).is_ok());
        assert!(held.require(held).is_ok());
    }

    #[test]
    fn require_names_the_single_missing_flag() {
        let held = Access::READ_CHAT;
        let err = held.require(Access::SEND_CHAT).unwrap_err();
        assert_eq!(err, "You are not allowed to do that (SEND_CHAT).");
    }

    #[test]
    fn require_names_every_missing_flag_joined_by_comma() {
        let held = Access::empty();
        let err = held
            .require(Access::READ_CHAT | Access::SEND_CHAT)
            .unwrap_err();
        assert_eq!(
            err,
            "You are not allowed to do that (READ_CHAT, SEND_CHAT)."
        );
    }
}
