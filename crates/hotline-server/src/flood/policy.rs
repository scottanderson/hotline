use std::net::IpAddr;

use async_trait::async_trait;

use super::{FloodDecision, FloodEvent};

/// Flood/spam policy. `AccessExtra::CAN_SPAM` exempts an account from
/// [`FloodPolicy::check`]; see `Session::flood_banned`.
#[async_trait]
pub trait FloodPolicy: Send + Sync {
    /// Record one `event` from `user_id`.
    async fn check(&self, user_id: u16, event: FloodEvent<'_>) -> FloodDecision;
    /// Record one new connection from `ip`, before its session starts.
    async fn check_connection(&self, ip: IpAddr) -> FloodDecision;
    /// Forget any accumulated state for `user_id`, called on disconnect.
    async fn reset(&self, user_id: u16);
}
