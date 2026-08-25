use async_trait::async_trait;
use hotline_proto::opcode::Opcode;

use super::FloodDecision;

/// Flood/spam policy, checked once per inbound client transaction before
/// it's dispatched. `AccessExtra::CAN_SPAM` exempts an account from this
/// check entirely; see `Session::handle`.
#[async_trait]
pub trait FloodPolicy: Send + Sync {
    /// Record one `opcode` transaction from `user_id` and decide what
    /// happens to it.
    async fn check(&self, user_id: u16, opcode: Opcode) -> FloodDecision;
    /// Forget any accumulated state for `user_id`, called on disconnect.
    async fn reset(&self, user_id: u16);
}
