bitflags::bitflags! {
    /// The [`myField_Options`] (113) bits of [`myTran_SetClientUserInfo`] (304).
    ///
    /// [`myField_Options`]: crate::FieldId::Options
    /// [`myTran_SetClientUserInfo`]: crate::Opcode::SetClientUserInfo
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct UserChangeOptions: u16 {
        /// Refuse private messages.
        const REFUSE_PM          = 0x01;
        /// Refuse private chat invitations.
        const REFUSE_CHAT        = 0x02;
        /// Answer a private message with [`myField_AutomaticResponse`] (215).
        ///
        /// [`myField_AutomaticResponse`]: crate::FieldId::AutomaticResponse
        const AUTOMATIC_RESPONSE = 0x04;
    }
}
