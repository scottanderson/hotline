use std::net::IpAddr;

use async_trait::async_trait;

use crate::flood::{FloodDecision, FloodEvent, FloodPolicy};

/// A [`FloodPolicy`] that never bans.
#[derive(Debug, Default)]
pub struct NoFloodPolicy;

#[async_trait]
impl FloodPolicy for NoFloodPolicy {
    async fn check(&self, _user_id: u16, _event: FloodEvent<'_>) -> FloodDecision {
        FloodDecision::Allow
    }

    async fn check_connection(&self, _ip: IpAddr) -> FloodDecision {
        FloodDecision::Allow
    }

    async fn reset(&self, _user_id: u16) {}
}
