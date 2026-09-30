use async_trait::async_trait;

use crate::activity::{ActivityEntry, ActivityLog};

/// An [`ActivityLog`] that records nothing.
#[derive(Debug, Default)]
pub struct NoActivityLog;

#[async_trait]
impl ActivityLog for NoActivityLog {
    async fn record(&self, _entry: ActivityEntry) {}
}
