//! [`UserFlags`]: bit meanings for the `myField_UserFlags` (112) field.

bitflags::bitflags! {
    /// Bit meanings for [`crate::types::UserNameWithInfo::flags`] / the [`myField_UserFlags`] (112) field.
    ///
    /// [`myField_UserFlags`]: crate::FieldId::UserFlags
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UserFlags: u16 {
        /// User is away/idle.
        const AWAY               = 1 << 0;
        /// User is an admin (`HLProtocol.md` says "disconnected?").
        const ADMIN               = 1 << 1;
        /// User refuses private messages.
        const REFUSES_PM          = 1 << 2;
        /// User refuses private chat invites.
        const REFUSES_CHAT        = 1 << 3;
        /// User holds every currently-defined [`myField_UserAccess`] (110) privilege.
        ///
        /// [`myField_UserAccess`]: crate::FieldId::UserAccess
        const ALL_PRIVS           = 1 << 4;
        /// User holds no [`myField_UserAccess`] (110) privileges at all.
        ///
        /// [`myField_UserAccess`]: crate::FieldId::UserAccess
        const NO_PRIVS            = 1 << 5;
    }
}
