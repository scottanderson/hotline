//! Deadlines for tokio timers.

use std::time::Duration;

use tokio::time::Instant;

/// The longest wait that [`deadline`] keeps.
const MAX_WAIT: Duration = Duration::from_secs(365 * 24 * 60 * 60);

/// The time `wait` after `now`. A wait longer than a year counts as a year.
pub(crate) fn deadline(now: Instant, wait: Duration) -> Instant {
    now.checked_add(wait.min(MAX_WAIT)).unwrap_or(now)
}
