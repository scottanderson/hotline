use std::sync::{Arc, RwLock};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::news::{
    FlatNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle, NewsItemKind, NewsListKind,
    ThreadedNewsProvider,
};
use crate::sync::{read, write};

/// A [`FlatNewsProvider`]/[`ThreadedNewsProvider`] that keeps news in
/// memory only; nothing survives a restart.
#[derive(Debug, Default)]
pub struct InMemoryNewsProvider {
    flat: RwLock<Arc<str>>,
    flat_lock: Mutex<()>,
    tree: ArcSwap<NewsBundle>,
    tree_lock: Mutex<()>,
}

#[async_trait]
impl FlatNewsProvider for InMemoryNewsProvider {
    async fn load(&self) -> Arc<str> {
        read(&self.flat).clone()
    }

    async fn post(&self, entry: &str) {
        let _guard = self.flat_lock.lock().await;
        let flat = self.load().await;
        let mut new_flat = String::with_capacity(entry.len() + flat.len());
        new_flat.push_str(entry);
        new_flat.push_str(&flat);
        *write(&self.flat) = new_flat.into();
    }
}

#[async_trait]
impl ThreadedNewsProvider for InMemoryNewsProvider {
    async fn list_dir(&self, components: &[String]) -> Option<Vec<(String, NewsListKind)>> {
        self.tree.load().list_dir(components)
    }

    async fn list_articles(&self, components: &[String]) -> Option<Vec<(u32, NewsArticle)>> {
        self.tree.load().list_articles(components).map(|articles| {
            articles
                .into_iter()
                .map(|(id, article)| (id, article.clone()))
                .collect()
        })
    }

    async fn article(&self, components: &[String], id: u32) -> Option<NewsArticleDetail> {
        self.tree.load().article(components, id).map(Into::into)
    }

    async fn item_kind(&self, components: &[String]) -> Option<NewsItemKind> {
        self.tree.load().item_kind(components)
    }

    async fn mkdir(&self, components: &[String], name: &str) -> Result<(), &'static str> {
        let _guard = self.tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.tree.load_full());
        tree.mkdir(components, name)?;
        self.tree.store(Arc::new(tree));
        Ok(())
    }

    async fn mkcategory(&self, components: &[String], name: &str) -> Result<(), &'static str> {
        let _guard = self.tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.tree.load_full());
        tree.mkcategory(components, name)?;
        self.tree.store(Arc::new(tree));
        Ok(())
    }

    async fn delete(&self, components: &[String]) -> Result<(), &'static str> {
        let _guard = self.tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.tree.load_full());
        tree.delete(components)?;
        self.tree.store(Arc::new(tree));
        Ok(())
    }

    async fn post_article(
        &self,
        components: &[String],
        parent_id: u32,
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
    ) -> Result<u32, &'static str> {
        let _guard = self.tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.tree.load_full());
        let id = tree.post_article(components, parent_id, title, poster, flags, mime_type, data)?;
        self.tree.store(Arc::new(tree));
        Ok(id)
    }

    async fn delete_article(
        &self,
        components: &[String],
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str> {
        let _guard = self.tree_lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.tree.load_full());
        tree.delete_article(components, id, recursive)?;
        self.tree.store(Arc::new(tree));
        Ok(())
    }
}
