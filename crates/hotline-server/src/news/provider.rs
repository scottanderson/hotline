use std::sync::Arc;

use async_trait::async_trait;

use super::NewsBundle;

/// News persistence, supplied by the embedding application like
/// [`crate::AccountProvider`]/[`crate::FileProvider`]. Flat text and the
/// threaded tree are independent load/save pairs, not one snapshot.
#[async_trait]
pub trait NewsProvider: Send + Sync {
    async fn load_flat(&self) -> Arc<Box<str>>;
    async fn save_flat(&self, flat: Arc<Box<str>>);
    async fn load_tree(&self) -> Arc<NewsBundle>;
    async fn save_tree(&self, tree: Arc<NewsBundle>);
}
