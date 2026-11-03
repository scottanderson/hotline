//! `hls`'s file-backed news providers.

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use hotline_proto::text_encoding::Encoding;
use hotline_server::{
    FlatNewsProvider, NewsArticle, NewsArticleDetail, NewsBundle, NewsCategory, NewsItemKind,
    NewsListKind, ThreadedNewsProvider, HLNZ_MAGIC,
};
use tokio::sync::Mutex;

/// Flat news as a plain text file.
///
/// Entries keep the wire's bare `\r` separators, like openline's
/// `MessageBoard.txt`. The text is UTF-8, not the wire encoding.
pub struct FileFlatNewsProvider {
    path: PathBuf,
    cache: RwLock<Arc<str>>,
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
            cache: RwLock::new(flat.into()),
            lock: Mutex::new(()),
        })
    }
}

#[async_trait]
impl FlatNewsProvider for FileFlatNewsProvider {
    async fn load(&self) -> Arc<str> {
        self.cache
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    async fn post(&self, entry: &str) {
        let _guard = self.lock.lock().await;
        let flat = self.load().await;
        let mut new_flat = String::with_capacity(entry.len() + flat.len());
        new_flat.push_str(entry);
        new_flat.push_str(&flat);

        if let Err(e) = tokio::fs::write(&self.path, new_flat.as_bytes()).await {
            tracing::error!(error = %e, "failed to persist flat news");
        }
        *self.cache.write().unwrap_or_else(PoisonError::into_inner) = new_flat.into();
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
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
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

/// The news tree in an openline `News` folder. A folder is a bundle, and
/// an `HLNZ` file is a category with its text in `encoding`. A hidden
/// item, another file, or an item that does not read is skipped.
pub fn import_openline_news(
    dir: &Path,
    encoding: &'static Encoding,
) -> std::io::Result<NewsBundle> {
    let mut tree = NewsBundle::default();
    import_folder(dir, &mut Vec::new(), &mut tree, encoding)?;
    Ok(tree)
}

fn import_folder(
    dir: &Path,
    components: &mut Vec<String>,
    tree: &mut NewsBundle,
    encoding: &'static Encoding,
) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let Ok(name) = entry.file_name().into_string() else {
            tracing::warn!(path = %path.display(), "skipped a news item whose name is not UTF-8");
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipped a news item");
                continue;
            }
        };
        if metadata.is_dir() {
            if let Err(e) = tree.mkdir(components, &name) {
                tracing::warn!(path = %path.display(), error = e, "skipped a news bundle");
                continue;
            }
            components.push(name);
            if let Err(e) = import_folder(&path, components, tree, encoding) {
                tracing::warn!(path = %path.display(), error = %e, "skipped part of a news bundle");
            }
            components.pop();
            continue;
        }
        let bytes = match read_hlnz(&path) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => continue,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "skipped a news item");
                continue;
            }
        };
        if let Err(e) = NewsCategory::from_hlnz(&bytes, encoding, &chrono::Local)
            .and_then(|category| tree.insert_category(components, &name, category))
        {
            tracing::warn!(path = %path.display(), error = e, "skipped a news category");
        }
    }
    Ok(())
}

/// The bytes of the file at `path`, or `None` if it is not an `HLNZ` file.
fn read_hlnz(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut magic = [0; 4];
    match file.read_exact(&mut magic) {
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    if &magic != HLNZ_MAGIC {
        return Ok(None);
    }
    let mut bytes = magic.to_vec();
    file.read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

#[cfg(test)]
mod tests {
    use hotline_proto::text_encoding;

    use super::*;

    /// An `HLNZ` file with no articles.
    fn empty_category() -> Vec<u8> {
        let mut bytes = HLNZ_MAGIC.to_vec();
        bytes.extend(2u32.to_be_bytes());
        bytes.extend([0; 16 + 8]);
        bytes.extend(256u32.to_be_bytes());
        bytes.resize(512, 0);
        bytes
    }

    #[test]
    fn import_openline_news_maps_folders_and_hlnz_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Bundle")).unwrap();
        std::fs::write(dir.path().join("Bundle/General"), empty_category()).unwrap();
        std::fs::write(dir.path().join("Top"), empty_category()).unwrap();
        std::fs::write(dir.path().join(".hidden"), empty_category()).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "text").unwrap();
        std::fs::write(dir.path().join("Broken"), HLNZ_MAGIC).unwrap();
        std::fs::write(dir.path().join("x"), "").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("missing", dir.path().join("Dangling")).unwrap();

        let tree = import_openline_news(dir.path(), text_encoding::DEFAULT).unwrap();

        let mut top = tree.list_dir(&[]).unwrap();
        top.sort_by(|a, b| a.0.cmp(&b.0));
        let names: Vec<_> = top.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["Bundle", "Top"]);
        assert_eq!(
            tree.item_kind(&["Bundle".into(), "General".into()]),
            Some(NewsItemKind::Category)
        );
    }

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
        assert_eq!(provider.load().await.as_ref(), "hello world");
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
        assert_eq!(reopened.load().await.as_ref(), entry);
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

        assert_eq!(reopened_flat.load().await.as_ref(), "hello");
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

        assert_eq!(flat.load().await.as_ref(), "");
        assert_eq!(tree.list_dir(&[]).await.unwrap().len(), 0);
    }
}
