//! `hls`'s file-backed news providers.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use hotline_server::{
    FlatNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle, NewsItemKind, NewsListKind,
    ThreadedNewsProvider,
};
use tokio::sync::Mutex;

/// Flat news as a plain text file.
///
/// Entries keep the wire's bare `\r` separators, like openline's
/// `MessageBoard.txt`. The text is UTF-8, not the wire encoding.
pub struct FileFlatNewsProvider {
    path: PathBuf,
    cache: ArcSwap<Box<str>>,
    lock: Mutex<()>,
}

impl FileFlatNewsProvider {
    /// Loads `path` if it exists, or starts empty.
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();

        let flat = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };

        Ok(Self {
            path,
            cache: ArcSwap::from_pointee(flat.into_boxed_str()),
            lock: Mutex::new(()),
        })
    }
}

#[async_trait]
impl FlatNewsProvider for FileFlatNewsProvider {
    async fn load(&self) -> Arc<Box<str>> {
        self.cache.load_full()
    }

    async fn post(&self, entry: &str) {
        let _guard = self.lock.lock().await;
        let flat = self.cache.load_full();
        let mut new_flat = String::with_capacity(entry.len() + flat.len());
        new_flat.push_str(entry);
        new_flat.push_str(&flat);

        if let Err(e) = tokio::fs::write(&self.path, new_flat.as_bytes()).await {
            tracing::error!(error = %e, "failed to persist flat news");
        }
        self.cache.store(Arc::new(new_flat.into_boxed_str()));
    }
}

/// Threaded news as a JSON file holding the whole tree.
pub struct FileThreadedNewsProvider {
    path: PathBuf,
    cache: ArcSwap<NewsBundle>,
    lock: Mutex<()>,
}

impl FileThreadedNewsProvider {
    /// Loads `path` if it exists, or starts empty.
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();

        let tree = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NewsBundle::default(),
            Err(e) => return Err(e),
        };

        Ok(Self {
            path,
            cache: ArcSwap::from_pointee(tree),
            lock: Mutex::new(()),
        })
    }

    /// Persists `tree` to disk and stores it as the current cache.
    async fn persist(&self, tree: NewsBundle) {
        match serde_json::to_vec_pretty(&tree) {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::write(&self.path, bytes).await {
                    tracing::error!(error = %e, "failed to persist news tree");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to serialize news tree"),
        }
        self.cache.store(Arc::new(tree));
    }
}

#[async_trait]
impl ThreadedNewsProvider for FileThreadedNewsProvider {
    async fn list_dir(&self, components: &[String]) -> Option<Vec<(String, NewsListKind)>> {
        self.cache.load().list_dir(components)
    }

    async fn list_articles(&self, components: &[String]) -> Option<Vec<(u32, NewsArticle)>> {
        self.cache.load().list_articles(components).map(|articles| {
            articles
                .into_iter()
                .map(|(id, article)| (id, article.clone()))
                .collect()
        })
    }

    async fn article(&self, components: &[String], id: u32) -> Option<NewsArticleDetail> {
        self.cache.load().article(components, id).map(Into::into)
    }

    async fn item_kind(&self, components: &[String]) -> Option<NewsItemKind> {
        self.cache.load().item_kind(components)
    }

    async fn mkdir(&self, components: &[String], name: &str) -> Result<(), &'static str> {
        let _guard = self.lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.cache.load_full());
        tree.mkdir(components, name)?;
        self.persist(tree).await;
        Ok(())
    }

    async fn mkcategory(&self, components: &[String], name: &str) -> Result<(), &'static str> {
        let _guard = self.lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.cache.load_full());
        tree.mkcategory(components, name)?;
        self.persist(tree).await;
        Ok(())
    }

    async fn delete(&self, components: &[String]) -> Result<(), &'static str> {
        let _guard = self.lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.cache.load_full());
        tree.delete(components)?;
        self.persist(tree).await;
        Ok(())
    }

    async fn post_article(
        &self,
        components: &[String],
        parent_id: u32,
        title: String,
        poster: String,
        flags: u32,
        mime_type: String,
        data: Vec<u8>,
    ) -> Result<u32, &'static str> {
        let _guard = self.lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.cache.load_full());
        let id = tree.post_article(components, parent_id, title, poster, flags, mime_type, data)?;
        self.persist(tree).await;
        Ok(id)
    }

    async fn delete_article(
        &self,
        components: &[String],
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str> {
        let _guard = self.lock.lock().await;
        let mut tree = Arc::unwrap_or_clone(self.cache.load_full());
        tree.delete_article(components, id, recursive)?;
        self.persist(tree).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn flat_news_persists_as_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let provider = FileFlatNewsProvider::open(&flat_path).await.unwrap();

        provider.post("hello world").await;

        assert_eq!(
            tokio::fs::read_to_string(&flat_path).await.unwrap(),
            "hello world"
        );
        assert_eq!(provider.load().await.as_ref().as_ref(), "hello world");
    }

    /// A `\n` in a post body survives a restart.
    #[tokio::test]
    async fn flat_news_round_trips_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let provider = FileFlatNewsProvider::open(&flat_path).await.unwrap();

        let entry = "From Alice\r\rline one\nline two\r\r";
        provider.post(entry).await;

        let on_disk = tokio::fs::read(&flat_path).await.unwrap();
        assert_eq!(on_disk, entry.as_bytes());

        let reopened = FileFlatNewsProvider::open(&flat_path).await.unwrap();
        assert_eq!(reopened.load().await.as_ref().as_ref(), entry);
    }

    #[tokio::test]
    async fn flat_and_tree_news_survive_reopening_the_same_files() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let tree_path = dir.path().join("news.json");
        {
            let flat = FileFlatNewsProvider::open(&flat_path).await.unwrap();
            flat.post("hello").await;
            let tree = FileThreadedNewsProvider::open(&tree_path).await.unwrap();
            tree.mkdir(&[], "General").await.unwrap();
        }

        let reopened_flat = FileFlatNewsProvider::open(&flat_path).await.unwrap();
        let reopened_tree = FileThreadedNewsProvider::open(&tree_path).await.unwrap();

        assert_eq!(reopened_flat.load().await.as_ref().as_ref(), "hello");
        assert_eq!(reopened_tree.list_dir(&[]).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn opening_missing_files_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let flat = FileFlatNewsProvider::open(dir.path().join("missing.txt"))
            .await
            .unwrap();
        let tree = FileThreadedNewsProvider::open(dir.path().join("missing.json"))
            .await
            .unwrap();

        assert_eq!(flat.load().await.as_ref().as_ref(), "");
        assert_eq!(tree.list_dir(&[]).await.unwrap().len(), 0);
    }
}
