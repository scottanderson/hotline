//! Access privilege bits carried in the 64-bit [`myField_UserAccess`] (110)
//! field.
//!
//! Two big-endian `u32` words concatenated (high word first): the
//! first-declared flag is bit 63, counting down. Reserved bits should
//! always be zero on the wire.
//!
//! [`myField_UserAccess`]: crate::FieldId::UserAccess

use super::RequireFlags;

bitflags::bitflags! {
    /// The 64-bit [`myField_UserAccess`] (110) privilege bitmap. Bits 51, 50, 45, and 44 are
    /// named, but have no use. Bits 22..=0 are reserved.
    ///
    /// [`myField_UserAccess`]: crate::FieldId::UserAccess
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

/// Each flag's mhxd `/access` console config key. [`Access::BROADCAST`]
/// is absent: mhxd's `set_access_bit` has no case for it.
const CONSOLE_NAMES: &[(&str, Access)] = &[
    ("delete_files", Access::DELETE_FILE),
    ("upload_files", Access::UPLOAD_FILE),
    ("download_files", Access::DOWNLOAD_FILE),
    ("rename_files", Access::RENAME_FILE),
    ("move_files", Access::MOVE_FILE),
    ("create_folders", Access::CREATE_FOLDER),
    ("delete_folders", Access::DELETE_FOLDER),
    ("rename_folders", Access::RENAME_FOLDER),
    ("move_folders", Access::MOVE_FOLDER),
    ("read_chat", Access::READ_CHAT),
    ("send_chat", Access::SEND_CHAT),
    ("create_pchats", Access::CREATE_CHAT),
    ("create_users", Access::CREATE_USER),
    ("delete_users", Access::DELETE_USER),
    ("read_users", Access::OPEN_USER),
    ("modify_users", Access::MODIFY_USER),
    ("read_news", Access::NEWS_READ_ART),
    ("post_news", Access::POST_NEWS_ART),
    ("disconnect_users", Access::DISCON_USER),
    ("cant_be_disconnected", Access::CANNOT_BE_DISCON),
    ("get_user_info", Access::GET_CLIENT_INFO),
    ("upload_anywhere", Access::UPLOAD_ANYWHERE),
    ("use_any_name", Access::ANY_NAME),
    ("dont_show_agreement", Access::NO_AGREEMENT),
    ("comment_files", Access::SET_FILE_COMMENT),
    ("comment_folders", Access::SET_FOLDER_COMMENT),
    ("view_drop_boxes", Access::VIEW_DROP_BOXES),
    ("make_aliases", Access::MAKE_ALIAS),
    ("delete_articles", Access::NEWS_DELETE_ART),
    ("create_categories", Access::NEWS_CREATE_CAT),
    ("delete_categories", Access::NEWS_DELETE_CAT),
    ("create_news_bundles", Access::NEWS_CREATE_FLDR),
    ("delete_news_bundles", Access::NEWS_DELETE_FLDR),
    ("upload_folders", Access::UPLOAD_FOLDER),
    ("download_folders", Access::DOWNLOAD_FOLDER),
    ("send_msgs", Access::SEND_MESSAGE),
];

impl Access {
    /// Looks up a flag by its console config key (`delete_files`,
    /// `cant_be_disconnected`, ...). [`Access::BROADCAST`] has no key.
    pub fn from_console_name(name: &str) -> Option<Self> {
        CONSOLE_NAMES
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, flag)| *flag)
    }

    /// A single flag's console config key, the inverse of
    /// [`Access::from_console_name`].
    pub fn console_name(self) -> Option<&'static str> {
        CONSOLE_NAMES
            .iter()
            .find(|(_, flag)| *flag == self)
            .map(|(key, _)| *key)
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

impl RequireFlags for Access {
    fn actions(&self) -> String {
        self.iter_names()
            .map(|(name, this)| match this {
                Access::DELETE_FILE => "delete files",
                Access::UPLOAD_FILE => "upload files",
                Access::DOWNLOAD_FILE => "download files",
                Access::RENAME_FILE => "rename files",
                Access::MOVE_FILE => "move files",
                Access::CREATE_FOLDER => "create folders",
                Access::DELETE_FOLDER => "delete folders",
                Access::RENAME_FOLDER => "rename folders",
                Access::MOVE_FOLDER => "move folders",
                Access::SEND_CHAT => "participate in chat",
                Access::CREATE_CHAT => "create a new chat",
                Access::CREATE_USER => "create new accounts",
                Access::DELETE_USER => "delete accounts",
                Access::OPEN_USER => "get account information",
                Access::MODIFY_USER => "modify account information",
                Access::NEWS_READ_ART => "read articles",
                Access::POST_NEWS_ART => "post articles",
                Access::DISCON_USER => "disconnect users",
                Access::GET_CLIENT_INFO => "get client information",
                Access::UPLOAD_ANYWHERE => "upload anywhere",
                Access::SET_FILE_COMMENT => "set comments for files",
                Access::SET_FOLDER_COMMENT => "set comments for folders",
                Access::MAKE_ALIAS => "make aliases",
                Access::BROADCAST => "broadcast",
                Access::NEWS_DELETE_ART => "delete articles",
                Access::NEWS_CREATE_CAT => "create news categories",
                Access::NEWS_DELETE_CAT => "delete news categories",
                Access::NEWS_CREATE_FLDR => "create news bundles",
                Access::NEWS_DELETE_FLDR => "delete news bundles",
                Access::UPLOAD_FOLDER => "upload folders",
                Access::DOWNLOAD_FOLDER => "download folders",
                Access::SEND_MESSAGE => "send private messages",
                Access::VIEW_DROP_BOXES => "view drop boxes",
                _ => name,
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever `/access` displays, it must also accept back.
    #[test]
    fn every_flag_but_broadcast_round_trips_through_its_console_name() {
        for (_, flag) in Access::all().iter_names() {
            if flag == Access::BROADCAST {
                assert_eq!(flag.console_name(), None);
                continue;
            }
            let key = flag.console_name().expect("every other flag has a key");
            assert_eq!(Access::from_console_name(key), Some(flag));
        }
    }

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
    fn require_describes_the_single_missing_flags_action() {
        let held = Access::READ_CHAT;
        let err = held.require(Access::SEND_CHAT).unwrap_err();
        assert_eq!(err, "You are not allowed to participate in chat.");
    }

    #[test]
    fn require_describes_every_missing_flags_action_joined_by_comma() {
        let held = Access::empty();
        let err = held
            .require(Access::READ_CHAT | Access::SEND_CHAT)
            .unwrap_err();
        assert_eq!(
            err,
            "You are not allowed to READ_CHAT, participate in chat."
        );
    }
}
