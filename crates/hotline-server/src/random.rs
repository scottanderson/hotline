//! Random numbers that make an ID hard to guess.

use tracing::warn;

/// A random `u32` from the system, or 0 when the system has none.
pub fn random_u32() -> u32 {
    getrandom::u32().unwrap_or_else(|e| {
        warn!("no random bytes: {e}");
        0
    })
}
