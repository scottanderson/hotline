use async_trait::async_trait;

use hotline_proto::opcode::Opcode;

use crate::flood::{FloodDecision, FloodPolicy};

/// A [`FloodPolicy`] that never throttles or kicks.
#[derive(Debug, Default)]
pub struct NoFloodPolicy;

#[async_trait]
impl FloodPolicy for NoFloodPolicy {
    async fn check(&self, _user_id: u16, _opcode: Opcode) -> FloodDecision {
        FloodDecision::Allow
    }

    async fn reset(&self, _user_id: u16) {}
}
