//! `hls`'s file-backed `NewsProvider`.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use hotline_server::{NewsBundle, NewsProvider};
use tokio::sync::RwLock;

/// Flat news as a plain text file, the threaded tree as a separate JSON file.
pub struct FileNewsProvider {
    flat_path: PathBuf,
    tree_path: PathBuf,
    flat_cache: RwLock<Arc<str>>,
    tree_cache: RwLock<Arc<NewsBundle>>,
}

impl FileNewsProvider {
    /// Loads `flat_path`/`tree_path` if they exist, or starts empty.
    pub async fn open(
        flat_path: impl Into<PathBuf>,
        tree_path: impl Into<PathBuf>,
    ) -> std::io::Result<Self> {
        let flat_path = flat_path.into();
        let tree_path = tree_path.into();

        let flat = match tokio::fs::read_to_string(&flat_path).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let tree = match tokio::fs::read(&tree_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => NewsBundle::default(),
            Err(e) => return Err(e),
        };

        Ok(Self {
            flat_path,
            tree_path,
            flat_cache: RwLock::new(flat.into()),
            tree_cache: RwLock::new(Arc::new(tree)),
        })
    }
}

#[async_trait]
impl NewsProvider for FileNewsProvider {
    async fn load_flat(&self) -> Arc<str> {
        self.flat_cache.read().await.clone()
    }

    async fn save_flat(&self, flat: Arc<str>) {
        if let Err(e) = tokio::fs::write(&self.flat_path, flat.as_bytes()).await {
            tracing::error!(error = %e, "failed to persist flat news");
        }
        *self.flat_cache.write().await = flat;
    }

    async fn load_tree(&self) -> Arc<NewsBundle> {
        self.tree_cache.read().await.clone()
    }

    async fn save_tree(&self, tree: Arc<NewsBundle>) {
        match serde_json::to_vec_pretty(tree.as_ref()) {
            Ok(bytes) => {
                if let Err(e) = tokio::fs::write(&self.tree_path, bytes).await {
                    tracing::error!(error = %e, "failed to persist news tree");
                }
            }
            Err(e) => tracing::error!(error = %e, "failed to serialize news tree"),
        }
        *self.tree_cache.write().await = tree;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn flat_news_persists_as_plain_text() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let tree_path = dir.path().join("news.json");
        let provider = FileNewsProvider::open(&flat_path, &tree_path)
            .await
            .unwrap();

        provider.save_flat("hello world".into()).await;

        assert_eq!(
            tokio::fs::read_to_string(&flat_path).await.unwrap(),
            "hello world"
        );
        assert_eq!(provider.load_flat().await.as_ref(), "hello world");
    }

    #[tokio::test]
    async fn flat_and_tree_news_survive_reopening_the_same_files() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let tree_path = dir.path().join("news.json");
        {
            let provider = FileNewsProvider::open(&flat_path, &tree_path)
                .await
                .unwrap();
            provider.save_flat("hello".into()).await;
            let mut tree = NewsBundle::default();
            tree.mkdir(&[], "General").unwrap();
            provider.save_tree(Arc::new(tree)).await;
        }

        let reopened = FileNewsProvider::open(&flat_path, &tree_path)
            .await
            .unwrap();

        assert_eq!(reopened.load_flat().await.as_ref(), "hello");
        assert_eq!(reopened.load_tree().await.entries.len(), 1);
    }

    #[tokio::test]
    async fn opening_missing_files_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileNewsProvider::open(
            dir.path().join("missing.txt"),
            dir.path().join("missing.json"),
        )
        .await
        .unwrap();

        assert_eq!(provider.load_flat().await.as_ref(), "");
        assert_eq!(provider.load_tree().await.entries.len(), 0);
    }
}
