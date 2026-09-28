bitflags::bitflags! {
    /// [`myField_Options`] (113) bit meanings when
    /// [`myTran_SetClientUserInfo`] (304) sends a persisted preference bitmap,
    /// distinct from the sequential values carried in
    /// [`myTran_SendInstantMsg`] (108)/[`myTran_ServerMsg`] (104) (see
    /// [`super::connection::MsgOption`]).
    ///
    /// [`myField_Options`]: hotline_proto::FieldId::Options
    /// [`myTran_SetClientUserInfo`]: hotline_proto::Opcode::SetClientUserInfo
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    /// [`myTran_ServerMsg`]: hotline_proto::Opcode::ServerMsg
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct UserChangeOptions: u16 {
        const REFUSE_PM       = 0x01;
        const REFUSE_CHAT     = 0x02;
        const AUTO_RESPONSE   = 0x04;
    }
}
