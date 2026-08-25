use std::time::Instant;

/// One user's accumulated flood points and when they were last updated.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FloodState {
    pub(crate) points: f64,
    pub(crate) last_update: Instant,
}
