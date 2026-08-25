use std::net::SocketAddr;
use std::time::{Duration, Instant};

use hotline_proto::consts::Access;
use hotline_proto::types::UserFlags;

use crate::account::AccessExtra;

/// How long without any request before a user is considered idle.
pub const IDLE_THRESHOLD: Duration = Duration::from_secs(10 * 60);

/// One connected user, as tracked by the server.
#[derive(Debug, Clone)]
pub struct UserEntry {
    /// Socket/user ID assigned at connect time.
    pub user_id: u16,
    /// Account login this session authenticated as. Also kept in
    /// [`crate::session::Session`].
    pub login: String,
    /// The account's own registered display name, which can differ from
    /// `name` if `USE_ANY_NAME` lets this session pick its own.
    pub account_name: String,
    /// Display name (post `SET_CLIENT_USER_INFO`).
    pub name: String,
    /// Icon ID.
    pub icon: u16,
    /// Client-reported protocol version (`myField_Vers`, field
    /// 160), `None` if the login request never sent one.
    pub client_version: Option<u16>,
    /// Remote socket address this connection was accepted from.
    pub remote_addr: SocketAddr,
    /// When this connection was accepted.
    pub connected_at: Instant,
    /// Privileges granted at login. Also kept in
    /// [`crate::session::Session`], which is what handlers check against.
    pub access: Access,
    /// Server-side-only account settings granted at login, also kept in
    /// [`crate::session::Session`].
    pub access_extra: AccessExtra,
    /// Last time this user sent any request. Compared against
    /// `IDLE_THRESHOLD` by [`UserEntry::is_idle`].
    pub last_active: Instant,
    /// Refuses incoming private messages. Also kept in
    /// [`crate::session::Session`].
    pub refuses_pm: bool,
    /// Refuses incoming private chat invites. Also kept in
    /// [`crate::session::Session`].
    pub refuses_chat: bool,
    /// Custom automatic-response text sent to private-message senders,
    /// `None` when the feature is off. Also kept in
    /// [`crate::session::Session`].
    pub automatic_response: Option<String>,
}

impl UserEntry {
    /// `true` if this user hasn't sent a request in over
    /// `IDLE_THRESHOLD`.
    pub fn is_idle(&self) -> bool {
        self.last_active.elapsed() > IDLE_THRESHOLD
    }

    /// Current `UserFlags` bits: admin (from `DISCONNECT_USERS`), idle, and
    /// whether this user holds every/no currently-defined privilege.
    pub fn flags(&self) -> u16 {
        let mut bits = if self.access.contains(Access::DISCON_USER) {
            UserFlags::ADMIN.bits()
        } else {
            0
        };
        if self.is_idle() {
            bits |= UserFlags::AWAY.bits();
        }
        if self.access == Access::all() - Access::NO_AGREEMENT {
            bits |= UserFlags::ALL_PRIVS.bits();
        }
        if self.access.is_empty() {
            bits |= UserFlags::NO_PRIVS.bits();
        }
        if self.refuses_pm {
            bits |= UserFlags::REFUSES_PM.bits();
        }
        if self.refuses_chat {
            bits |= UserFlags::REFUSES_CHAT.bits();
        }
        bits
    }
}
