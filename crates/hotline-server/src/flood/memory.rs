use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

use async_trait::async_trait;

use hotline_proto::opcode::Opcode;

use crate::flood::state::FloodState;
use crate::flood::{FloodConfig, FloodDecision, FloodPolicy};

/// A [`FloodPolicy`] that accumulates points per transaction, linearly
/// decaying them over time, and throttles/kicks once accumulated points
/// cross a threshold. Nothing persists across a restart.
#[derive(Debug)]
pub struct InMemoryFloodPolicy {
    config: FloodConfig,
    states: RwLock<HashMap<u16, FloodState>>,
}

impl InMemoryFloodPolicy {
    /// Creates a policy with no accumulated points for any user.
    pub fn new(config: FloodConfig) -> Self {
        Self {
            config,
            states: RwLock::new(HashMap::new()),
        }
    }

    const fn points_for(&self, opcode: Opcode) -> u32 {
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
            decay: std::time::Duration::from_secs(60),
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
            decay: std::time::Duration::from_secs(60),
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
