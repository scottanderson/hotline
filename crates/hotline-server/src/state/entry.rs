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
    pub login: Box<str>,
    /// The account's own registered display name, which can differ from
    /// `name` if `USE_ANY_NAME` lets this session pick its own.
    pub account_name: Box<str>,
    /// Display name, as last set by [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    pub name: Box<str>,
    /// Icon ID.
    pub icon: u16,
    /// Client-reported protocol version, [`myField_Vers`] (160). `None` if
    /// the login request never sent one.
    ///
    /// [`myField_Vers`]: hotline_proto::FieldId::Vers
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
    pub automatic_response: Option<Box<str>>,
}

impl UserEntry {
    /// `true` if this user hasn't sent a request in over
    /// `IDLE_THRESHOLD`.
    pub fn is_idle(&self) -> bool {
        self.last_active.elapsed() > IDLE_THRESHOLD
    }

    /// Current `UserFlags`: admin (from `DISCONNECT_USERS`), idle, and
    /// whether this user holds each privilege but `NO_AGREEMENT`, or none.
    pub fn flags(&self) -> UserFlags {
        let mut flags = UserFlags::empty();
        flags.set(UserFlags::ADMIN, self.access.contains(Access::DISCON_USER));
        flags.set(UserFlags::AWAY, self.is_idle());
        flags.set(
            UserFlags::ALL_PRIVS,
            self.access
                .contains(Access::all().difference(Access::NO_AGREEMENT)),
        );
        flags.set(UserFlags::NO_PRIVS, self.access.is_empty());
        flags.set(UserFlags::REFUSES_PM, self.refuses_pm);
        flags.set(UserFlags::REFUSES_CHAT, self.refuses_chat);
        flags
    }
}
