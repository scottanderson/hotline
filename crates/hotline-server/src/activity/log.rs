use async_trait::async_trait;

use super::ActivityEntry;

/// Receives [`ActivityEntry`] reports for an operator's logs.
#[async_trait]
pub trait ActivityLog: Send + Sync {
    /// Record one entry.
    async fn record(&self, entry: ActivityEntry);
}
