//! Flood/spam rate limiting, checked once per inbound client transaction.
//!
//! [`FloodPolicy`] is the decision interface; `hotline-server` only defines
//! it and provides [`NoFloodPolicy`] and [`InMemoryFloodPolicy`] as
//! defaults. The embedding application supplies the real policy to
//! [`crate::Server::new`], the same way it supplies a [`crate::BanProvider`].

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use async_trait::async_trait;

use hotline_proto::opcode::Opcode;

/// What a session should do after [`FloodPolicy::check`] records one more
/// transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloodDecision {
    /// Under the limit; proceed as normal.
    Allow,
    /// Over the limit; drop this transaction and warn the sender instead.
    Throttle,
    /// Far enough over the limit to disconnect this session.
    Kick,
}

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

/// Tunables for [`InMemoryFloodPolicy`].
#[derive(Debug, Clone, Copy)]
pub struct FloodConfig {
    /// Points added per transaction, absent a more specific override.
    pub default_points: u32,
    /// Points added for `myTran_ChatSend` (105) and `myTran_SendInstantMsg` (108),
    /// the two opcodes most likely to be used to spam other users.
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

/// One user's accumulated flood points and when they were last updated.
#[derive(Debug, Clone, Copy)]
struct FloodState {
    points: f64,
    last_update: Instant,
}

/// A [`FloodPolicy`] that accumulates points per transaction, linearly
/// decaying them over time, and throttles/kicks once accumulated points
/// cross a threshold. Nothing persists across a restart.
#[derive(Debug)]
pub struct InMemoryFloodPolicy {
    config: FloodConfig,
    states: RwLock<HashMap<u16, FloodState>>,
}

impl InMemoryFloodPolicy {
    pub fn new(config: FloodConfig) -> Self {
        Self {
            config,
            states: RwLock::new(HashMap::new()),
        }
    }

    fn points_for(&self, opcode: Opcode) -> u32 {
        match opcode {
            Opcode::ChatSend | Opcode::SendInstantMsg => self.config.chat_points,
            _ => self.config.default_points,
        }
    }
}

impl Default for InMemoryFloodPolicy {
    fn default() -> Self {
        Self::new(FloodConfig::default())
    }
}

#[async_trait]
impl FloodPolicy for InMemoryFloodPolicy {
    async fn check(&self, user_id: u16, opcode: Opcode) -> FloodDecision {
        let decay_per_sec = 1.0 / self.config.decay.as_secs_f64();
        let now = Instant::now();
        let mut states = self.states.write().unwrap();
        let state = states.entry(user_id).or_insert(FloodState {
            points: 0.0,
            last_update: now,
        });
        let elapsed = now.duration_since(state.last_update).as_secs_f64();
        state.points = (state.points - elapsed * decay_per_sec).max(0.0);
        state.points += self.points_for(opcode) as f64;
        state.last_update = now;

        if state.points >= self.config.kick_threshold as f64 {
            FloodDecision::Kick
        } else if state.points >= self.config.throttle_threshold as f64 {
            FloodDecision::Throttle
        } else {
            FloodDecision::Allow
        }
    }

    async fn reset(&self, user_id: u16) {
        self.states.write().unwrap().remove(&user_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn points_accumulate_and_kick_once_over_threshold() {
        let policy = InMemoryFloodPolicy::new(FloodConfig {
            default_points: 10,
            chat_points: 10,
            decay: Duration::from_secs(60),
            throttle_threshold: 15,
            kick_threshold: 25,
        });

        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Allow);
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Throttle);
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Kick);
    }

    #[tokio::test]
    async fn reset_clears_accumulated_points() {
        let policy = InMemoryFloodPolicy::new(FloodConfig {
            default_points: 10,
            chat_points: 10,
            decay: Duration::from_secs(60),
            throttle_threshold: 15,
            kick_threshold: 25,
        });
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Allow);
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Throttle);
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Kick);

        policy.reset(1).await;
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Allow);
    }

    #[tokio::test]
    async fn users_are_tracked_independently() {
        let policy = InMemoryFloodPolicy::new(FloodConfig {
            default_points: 30,
            ..FloodConfig::default()
        });
        assert_eq!(policy.check(1, Opcode::Ping).await, FloodDecision::Kick);
        assert_eq!(policy.check(2, Opcode::Ping).await, FloodDecision::Kick);
    }
}
