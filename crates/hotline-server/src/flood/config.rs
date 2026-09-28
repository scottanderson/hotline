use std::time::Duration;

/// Tunables for [`crate::InMemoryFloodPolicy`].
#[derive(Debug, Clone, Copy)]
pub struct FloodConfig {
    /// Points added per transaction, absent a more specific override.
    pub default_points: u32,
    /// Points added for [`myTran_ChatSend`] (105) and [`myTran_SendInstantMsg`] (108),
    /// the two opcodes most likely to be used to spam other users.
    ///
    /// [`myTran_ChatSend`]: hotline_proto::Opcode::ChatSend
    /// [`myTran_SendInstantMsg`]: hotline_proto::Opcode::SendInstantMsg
    pub chat_points: u32,
    /// How long it takes one accumulated point to fully decay.
    pub decay: Duration,
    /// Accumulated points at or above which a transaction is throttled.
    pub throttle_threshold: u32,
    /// Accumulated points at or above which a session is kicked.
    pub kick_threshold: u32,
}

impl Default for FloodConfig {
    fn default() -> Self {
        Self {
            default_points: 1,
            chat_points: 3,
            decay: Duration::from_secs(2),
            throttle_threshold: 15,
            kick_threshold: 25,
        }
    }
}
