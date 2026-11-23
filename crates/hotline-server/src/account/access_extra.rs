bitflags::bitflags! {
    /// Per-account settings with no wire representation.
    /// Only resolved server-side.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct AccessExtra: u32 {
        /// Open or create a private chat.
        /// Config key: `chat_private`.
        const CHAT_PRIVATE       = 1 << 0;
        /// Send a private message even without `Access::SEND_MESSAGE`.
        /// Config key: `msg`.
        const MSG_OVERRIDE       = 1 << 1;
        /// List users and see join/part/change traffic.
        /// Config key: `user_getlist`.
        const USER_GETLIST       = 1 << 2;
        /// List files.
        /// Config key: `file_list`.
        const FILE_LIST          = 1 << 3;
        /// Get a file's info.
        /// Config key: `file_getinfo`.
        const FILE_GETINFO       = 1 << 4;
        /// Get a file's hash.
        /// Config key: `file_hash`.
        const FILE_HASH          = 1 << 5;
        /// May process [`myTran_Login`] (107); cleared once logged in.
        /// Config key: `can_login`.
        ///
        /// [`myTran_Login`]: hotline_proto::Opcode::Login
        const CAN_LOGIN          = 1 << 6;
        /// May be hidden from `GETLIST` while invisible.
        /// Config key: `user_visibility`.
        const USER_VISIBILITY    = 1 << 7;
        /// Set a custom color via a console command.
        /// Config key: `user_color`.
        const USER_COLOR         = 1 << 8;
        /// Exempt from spam/flood limits.
        /// Config key: `can_spam`.
        const CAN_SPAM           = 1 << 9;
        /// Set the global chat room's subject.
        /// Config key: `set_subject`.
        const SET_SUBJECT        = 1 << 10;
        /// Use debug console commands.
        /// Config key: `debug`.
        const DEBUG              = 1 << 11;
        /// View another user's access bits via a console command.
        /// Config key: `user_access`.
        const USER_ACCESS        = 1 << 12;
        /// Required for `/access` to modify this account. The change is
        /// live only and is never saved to the account.
        /// Config key: `access_volatile`.
        const ACCESS_VOLATILE    = 1 << 13;
        /// Use the console `/0wn` command on other users.
        /// Config key: `user_0wn`.
        const USER_0WN           = 1 << 14;
        /// Can be targeted by `/0wn`.
        /// Config key: `is_0wn3d`.
        const IS_0WN3D           = 1 << 15;
        /// Manage other users via console commands.
        /// Config key: `manage_users`.
        const MANAGE_USERS       = 1 << 16;
        /// Reveal this account's IP to `GETINFO` callers.
        /// Config key: `info_get_address`.
        const INFO_GET_ADDRESS   = 1 << 17;
        /// Reveal this account's login to `GETINFO` callers.
        /// Config key: `info_get_login`.
        const INFO_GET_LOGIN     = 1 << 18;
        /// Blocks `USE_ANY_NAME` from renaming via chat commands.
        /// Config key: `name_lock`.
        const NAME_LOCK          = 1 << 19;
        /// May (re-)agree to the server agreement.
        /// Config key: `can_agree`.
        const CAN_AGREE          = 1 << 20;
        /// May issue a ping/keepalive request.
        /// Config key: `can_ping`.
        const CAN_PING           = 1 << 21;
        /// Download the server banner image.
        /// Config key: `banner_get`.
        const BANNER_GET         = 1 << 22;
        /// Skip the file transfer queue.
        /// Config key: `ignore_queue`.
        const IGNORE_QUEUE       = 1 << 23;
    }
}

/// Each flag's config key.
const CONSOLE_NAMES: &[(&str, AccessExtra)] = &[
    ("chat_private", AccessExtra::CHAT_PRIVATE),
    ("msg", AccessExtra::MSG_OVERRIDE),
    ("user_getlist", AccessExtra::USER_GETLIST),
    ("file_list", AccessExtra::FILE_LIST),
    ("file_getinfo", AccessExtra::FILE_GETINFO),
    ("file_hash", AccessExtra::FILE_HASH),
    ("user_visibility", AccessExtra::USER_VISIBILITY),
    ("user_color", AccessExtra::USER_COLOR),
    ("can_spam", AccessExtra::CAN_SPAM),
    ("set_subject", AccessExtra::SET_SUBJECT),
    ("debug", AccessExtra::DEBUG),
    ("user_access", AccessExtra::USER_ACCESS),
    ("access_volatile", AccessExtra::ACCESS_VOLATILE),
    ("user_0wn", AccessExtra::USER_0WN),
    ("is_0wn3d", AccessExtra::IS_0WN3D),
    ("info_get_address", AccessExtra::INFO_GET_ADDRESS),
    ("info_get_login", AccessExtra::INFO_GET_LOGIN),
    ("manage_users", AccessExtra::MANAGE_USERS),
    ("name_lock", AccessExtra::NAME_LOCK),
    ("ignore_queue", AccessExtra::IGNORE_QUEUE),
    ("can_login", AccessExtra::CAN_LOGIN),
    ("can_agree", AccessExtra::CAN_AGREE),
    ("can_ping", AccessExtra::CAN_PING),
    ("banner_get", AccessExtra::BANNER_GET),
];

impl AccessExtra {
    /// Looks up a flag by its config key (`chat_private`, `msg`, ...).
    pub fn from_console_name(name: &str) -> Option<Self> {
        CONSOLE_NAMES
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, flag)| *flag)
    }

    /// A single flag's config key, the inverse of
    /// [`AccessExtra::from_console_name`].
    pub fn console_name(self) -> Option<&'static str> {
        CONSOLE_NAMES
            .iter()
            .find(|(_, flag)| *flag == self)
            .map(|(key, _)| *key)
    }
}

impl Default for AccessExtra {
    /// Matches mhxd's own defaults for a fresh account.
    fn default() -> Self {
        Self::CHAT_PRIVATE
            | Self::USER_GETLIST
            | Self::FILE_LIST
            | Self::FILE_GETINFO
            | Self::FILE_HASH
            | Self::ACCESS_VOLATILE
            | Self::IS_0WN3D
            | Self::INFO_GET_ADDRESS
            | Self::INFO_GET_LOGIN
            | Self::BANNER_GET
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever `/access` displays, it must also accept back.
    #[test]
    fn every_flag_round_trips_through_its_console_name() {
        for (_, flag) in AccessExtra::all().iter_names() {
            let key = flag.console_name().expect("every flag has a key");
            assert_eq!(AccessExtra::from_console_name(key), Some(flag));
        }
        assert_eq!(AccessExtra::from_console_name("nope"), None);
    }

    /// The key stays mhxd's own `msg`.
    #[test]
    fn msg_override_keeps_the_msg_key() {
        assert_eq!(
            AccessExtra::from_console_name("msg"),
            Some(AccessExtra::MSG_OVERRIDE)
        );
        assert_eq!(AccessExtra::MSG_OVERRIDE.console_name(), Some("msg"));
        assert_eq!(AccessExtra::from_console_name("msg_override"), None);
    }

    #[test]
    fn access_extra_default_matches_the_reference() {
        let expected = AccessExtra::CHAT_PRIVATE
            | AccessExtra::USER_GETLIST
            | AccessExtra::FILE_LIST
            | AccessExtra::FILE_GETINFO
            | AccessExtra::FILE_HASH
            | AccessExtra::ACCESS_VOLATILE
            | AccessExtra::IS_0WN3D
            | AccessExtra::INFO_GET_ADDRESS
            | AccessExtra::INFO_GET_LOGIN
            | AccessExtra::BANNER_GET;
        assert_eq!(AccessExtra::default(), expected);
    }
}
