use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;

use crate::news::{NewsBundle, NewsProvider};

/// A [`NewsProvider`] that keeps news in memory only; nothing survives a restart.
#[derive(Debug, Default)]
pub struct InMemoryNewsProvider {
    flat: ArcSwap<Box<str>>,
    tree: ArcSwap<NewsBundle>,
}

#[async_trait]
impl NewsProvider for InMemoryNewsProvider {
    async fn load_flat(&self) -> Arc<Box<str>> {
        self.flat.load_full()
    }

    async fn save_flat(&self, flat: Arc<Box<str>>) {
        self.flat.store(flat);
    }

    async fn load_tree(&self) -> Arc<NewsBundle> {
        self.tree.load_full()
    }

    async fn save_tree(&self, tree: Arc<NewsBundle>) {
        self.tree.store(tree);
    }
}
