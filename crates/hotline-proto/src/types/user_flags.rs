//! [`UserFlags`]: bit meanings for the `USER_FLAGS` field.

bitflags::bitflags! {
    /// Bit meanings for [`crate::types::UserNameWithInfo::flags`] / the `USER_FLAGS` field.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UserFlags: u16 {
        /// User is away/idle.
        const AWAY               = 1 << 0;
        /// User is an admin (or, per the reference's own uncertainty, disconnected).
        const ADMIN               = 1 << 1;
        /// User refuses private messages.
        const REFUSES_PM          = 1 << 2;
        /// User refuses private chat invites.
        const REFUSES_CHAT        = 1 << 3;
        /// User holds every currently-defined `USER_ACCESS` privilege.
        const ALL_PRIVS           = 1 << 4;
        /// User holds no `USER_ACCESS` privileges at all.
        const NO_PRIVS            = 1 << 5;
    }
}
