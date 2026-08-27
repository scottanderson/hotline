//! `hls`'s file-backed `NewsProvider`.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use hotline_server::{NewsBundle, NewsProvider};

/// Flat news as a plain text file, the threaded tree as a separate JSON file.
///
/// Flat news entries keep the wire's bare `\r` separators, like openline's
/// `MessageBoard.txt`. The text is UTF-8, not the wire encoding.
pub struct FileNewsProvider {
    flat_path: PathBuf,
    tree_path: PathBuf,
    flat_cache: ArcSwap<Box<str>>,
    tree_cache: ArcSwap<NewsBundle>,
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
            flat_cache: ArcSwap::from_pointee(flat.into_boxed_str()),
            tree_cache: ArcSwap::from_pointee(tree),
        })
    }
}

#[async_trait]
impl NewsProvider for FileNewsProvider {
    async fn load_flat(&self) -> Arc<Box<str>> {
        self.flat_cache.load_full()
    }

    async fn save_flat(&self, flat: Arc<Box<str>>) {
        if let Err(e) = tokio::fs::write(&self.flat_path, flat.as_bytes()).await {
            tracing::error!(error = %e, "failed to persist flat news");
        }
        self.flat_cache.store(flat);
    }

    async fn load_tree(&self) -> Arc<NewsBundle> {
        self.tree_cache.load_full()
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
        self.tree_cache.store(tree);
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

        provider.save_flat(Arc::new("hello world".into())).await;

        assert_eq!(
            tokio::fs::read_to_string(&flat_path).await.unwrap(),
            "hello world"
        );
        assert_eq!(provider.load_flat().await.as_ref().as_ref(), "hello world");
    }

    /// A `\n` in a post body survives a restart.
    #[tokio::test]
    async fn flat_news_round_trips_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let flat_path = dir.path().join("news.txt");
        let tree_path = dir.path().join("news.json");
        let provider = FileNewsProvider::open(&flat_path, &tree_path)
            .await
            .unwrap();

        let entry = "From Alice\r\rline one\nline two\r\r";
        provider.save_flat(Arc::new(entry.into())).await;

        let on_disk = tokio::fs::read(&flat_path).await.unwrap();
        assert_eq!(on_disk, entry.as_bytes());

        let reopened = FileNewsProvider::open(&flat_path, &tree_path)
            .await
            .unwrap();
        assert_eq!(reopened.load_flat().await.as_ref().as_ref(), entry);
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
            provider.save_flat(Arc::new("hello".into())).await;
            let mut tree = NewsBundle::default();
            tree.mkdir(&[], "General").unwrap();
            provider.save_tree(Arc::new(tree)).await;
        }

        let reopened = FileNewsProvider::open(&flat_path, &tree_path)
            .await
            .unwrap();

        assert_eq!(reopened.load_flat().await.as_ref().as_ref(), "hello");
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

        assert_eq!(provider.load_flat().await.as_ref().as_ref(), "");
        assert_eq!(provider.load_tree().await.entries.len(), 0);
    }
}
