//! A directory-backed [`FileProvider`], the same directory tree for
//! every login. `hotline-server` deliberately doesn't own file storage
//! (see its module docs); this is the concrete backend `hls` plugs in.
//!
//! File and folder comments aren't native filesystem metadata, so they
//! persist as a single JSON file elsewhere on disk (not under the served
//! root, so it never shows up in a client's own file listing), mapping
//! each path to its comment.

use std::collections::HashMap;
use std::io;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use hotline_server::{
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, FileProvider,
    FileRequester, ItemTimes,
};
use tokio::sync::RwLock;

/// Serves one directory on disk, the same for every login.
pub struct DiskFileProvider {
    root: PathBuf,
    comments_path: PathBuf,
    comments: RwLock<HashMap<String, String>>,
}

impl DiskFileProvider {
    /// Open (creating if missing) `root` to serve, with comments persisted
    /// to `comments_file` (also created if missing), a path outside `root`.
    pub async fn open(
        root: impl Into<PathBuf>,
        comments_file: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await?;
        let comments_path = comments_file.into();
        let comments = match tokio::fs::read(&comments_path).await {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            root,
            comments_path,
            comments: RwLock::new(comments),
        })
    }

    /// The key a comment is stored under: `dir`'s components joined with
    /// `name`, so folder and file comments share one flat map.
    fn comment_key(dir: &HlPath, name: &str) -> String {
        let mut parts = dir.components.clone();
        parts.push(name.to_string());
        parts.join("/")
    }

    /// Move every comment keyed under `old_key` to `new_key`: the entry
    /// itself, plus every descendant comment whose key has `old_key` as a
    /// path prefix (a renamed/moved folder's contents), then persists the
    /// updated map to disk.
    async fn rekey_comments(&self, old_key: &str, new_key: &str) -> io::Result<()> {
        let mut comments = self.comments.write().await;
        let old_prefix = format!("{old_key}/");
        let matches: Vec<_> = comments
            .keys()
            .filter(|key| *key == old_key || key.starts_with(&old_prefix))
            .cloned()
            .collect();
        if matches.is_empty() {
            return Ok(());
        }
        for key in matches {
            let comment = comments.remove(&key).unwrap();
            let rekeyed = match key.strip_prefix(&old_prefix) {
                Some(rest) => format!("{new_key}/{rest}"),
                None => new_key.to_string(),
            };
            comments.insert(rekeyed, comment);
        }
        self.write_comments(&comments).await
    }

    /// Drop the comment on `key` and on every key under it whose entry is
    /// gone from disk, then persist the updated map.
    async fn drop_comments(&self, key: &str) -> io::Result<()> {
        let mut comments = self.comments.write().await;
        let prefix = format!("{key}/");
        let mut gone = Vec::new();
        for k in comments
            .keys()
            .filter(|k| *k == key || k.starts_with(&prefix))
        {
            let path = k.split('/').fold(self.root.clone(), |path, c| path.join(c));
            if tokio::fs::symlink_metadata(&path).await.is_err() {
                gone.push(k.clone());
            }
        }
        if gone.is_empty() {
            return Ok(());
        }
        for k in &gone {
            comments.remove(k);
        }
        self.write_comments(&comments).await
    }

    async fn write_comments(&self, comments: &HashMap<String, String>) -> io::Result<()> {
        let bytes = serde_json::to_vec_pretty(comments)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&self.comments_path, bytes).await
    }

    /// Resolve a directory path under `root`. `None` when a component is
    /// not a plain name.
    fn resolve_dir(&self, dir: &HlPath) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for component in &dir.components {
            if !is_plain_name(component) {
                return None;
            }
            path.push(component);
        }
        Some(path)
    }

    /// Resolve the entry `name` in `dir` under `root`, with the checks of
    /// [`Self::resolve_dir`].
    fn resolve_file(&self, dir: &HlPath, name: &str) -> Option<PathBuf> {
        if !is_plain_name(name) {
            return None;
        }
        let mut path = self.resolve_dir(dir)?;
        path.push(name);
        Some(path)
    }
}

/// Whether `name` is one normal path component, with no `:` on Windows.
fn is_plain_name(name: &str) -> bool {
    let mut components = Path::new(name).components();
    !name.contains(['/', '\\'])
        && !(cfg!(windows) && name.contains(':'))
        && matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
}

#[async_trait]
impl FileProvider for DiskFileProvider {
    async fn list(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        let path = self.resolve_dir(dir)?;
        let mut read_dir = tokio::fs::read_dir(&path).await.ok()?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let metadata = match tokio::fs::metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(_) => match entry.metadata().await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                },
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            entries.push(if metadata.is_dir() {
                FileNameWithInfo {
                    file_type: *b"fldr",
                    creator: *b"\0\0\0\0",
                    size: 0,
                    name: name.into(),
                }
            } else {
                let (file_type, creator) = type_and_creator_for_name(&name);
                FileNameWithInfo {
                    file_type,
                    creator,
                    size: metadata.len() as u32,
                    name: name.into(),
                }
            });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Some(entries)
    }

    async fn size(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> Option<u64> {
        let path = self.resolve_file(dir, name)?;
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        metadata.is_file().then_some(metadata.len())
    }

    async fn exists(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> bool {
        let Some(path) = self.resolve_file(dir, name) else {
            return false;
        };
        tokio::fs::metadata(&path).await.is_ok()
    }

    async fn resolved_path(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<PathBuf> {
        tokio::fs::canonicalize(self.resolve_file(dir, name)?)
            .await
            .ok()
    }

    async fn open_read(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedReader> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(Box::new(tokio::fs::File::open(path).await?))
    }

    async fn create_write(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedWriter> {
        let path = self
            .resolve_file(dir, &partial_upload_name(name))
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        Ok(Box::new(tokio::fs::File::create(path).await?))
    }

    async fn open_write_resume(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedWriter> {
        use tokio::io::AsyncSeekExt;

        let path = self
            .resolve_file(dir, &partial_upload_name(name))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut file = tokio::fs::OpenOptions::new().write(true).open(path).await?;
        let current_len = file.metadata().await?.len();
        if current_len != at {
            tracing::debug!(
                name,
                current_len,
                at,
                "resuming upload: on-disk length didn't match the negotiated offset"
            );
        }
        file.set_len(at).await?;
        file.seek(io::SeekFrom::Start(at)).await?;
        Ok(Box::new(file))
    }

    async fn info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FileNameWithInfo> {
        let path = self.resolve_file(dir, name)?;
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        if metadata.is_dir() {
            return Some(FileNameWithInfo {
                file_type: *b"fldr",
                creator: *b"\0\0\0\0",
                size: 0,
                name: name.into(),
            });
        }
        let (file_type, creator) = type_and_creator_for_name(name);
        Some(FileNameWithInfo {
            file_type,
            creator,
            size: metadata.len() as u32,
            name: name.into(),
        })
    }

    #[cfg(unix)]
    async fn free_space(&self, _requester: FileRequester<'_>, dir: &HlPath) -> Option<u64> {
        let path = self.resolve_dir(dir)?;
        let stat = tokio::task::spawn_blocking(move || rustix::fs::statvfs(&path))
            .await
            .ok()?
            .ok()?;
        Some(stat.f_bavail.saturating_mul(stat.f_frsize))
    }

    async fn times(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> ItemTimes {
        let Some(path) = self.resolve_file(dir, name) else {
            return ItemTimes::default();
        };
        match tokio::fs::metadata(&path).await {
            Ok(metadata) => ItemTimes {
                created: metadata.created().ok(),
                modified: metadata.modified().ok(),
            },
            Err(_) => ItemTimes::default(),
        }
    }

    async fn delete(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        is_dir: bool,
    ) -> io::Result<()> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let removed = if is_dir {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        };
        if let Err(error) = self.drop_comments(&Self::comment_key(dir, name)).await {
            tracing::warn!(%error, name, "could not drop the comments of a deleted entry");
        }
        removed
    }

    async fn mkdir(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<()> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        tokio::fs::create_dir(&path).await
    }

    async fn rename(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        old_name: &str,
        new_name: &str,
    ) -> io::Result<()> {
        let old_path = self
            .resolve_file(dir, old_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let new_path = self
            .resolve_file(dir, new_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if tokio::fs::metadata(&new_path).await.is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        tokio::fs::rename(old_path, new_path).await?;
        self.rekey_comments(
            &Self::comment_key(dir, old_name),
            &Self::comment_key(dir, new_name),
        )
        .await
    }

    async fn r#move(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        to_dir: &HlPath,
    ) -> io::Result<()> {
        let old_path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let new_path = self
            .resolve_file(to_dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if tokio::fs::metadata(&new_path).await.is_ok() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        tokio::fs::rename(old_path, new_path).await?;
        self.rekey_comments(
            &Self::comment_key(dir, name),
            &Self::comment_key(to_dir, name),
        )
        .await
    }

    async fn symlink(
        &self,
        _requester: FileRequester<'_>,
        target_dir: &HlPath,
        target_name: &str,
        link_dir: &HlPath,
        link_name: &str,
    ) -> io::Result<()> {
        let target_path = self
            .resolve_file(target_dir, target_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let link_path = self
            .resolve_file(link_dir, link_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        #[cfg(unix)]
        {
            tokio::fs::symlink(target_path, link_path).await
        }
        #[cfg(not(unix))]
        {
            let _ = (target_path, link_path);
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
    }

    async fn comment(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<String> {
        self.comments
            .read()
            .await
            .get(&Self::comment_key(dir, name))
            .cloned()
    }

    async fn set_comment(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
    ) -> io::Result<()> {
        let key = Self::comment_key(dir, name);
        let mut comments = self.comments.write().await;
        if comment.is_empty() {
            comments.remove(&key);
        } else {
            comments.insert(key, comment.to_string());
        }
        self.write_comments(&comments).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requester() -> FileRequester<'static> {
        FileRequester { login: "alice" }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn free_space_reads_the_disk_and_is_none_for_a_bad_path() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        assert!(provider
            .free_space(requester(), &HlPath::default())
            .await
            .is_some());
        assert_eq!(
            provider
                .free_space(requester(), &HlPath::new(["missing"]))
                .await,
            None
        );
    }

    #[tokio::test]
    async fn times_reads_the_modify_time() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        let times = provider
            .times(requester(), &HlPath::default(), "a.txt")
            .await;

        let modified = std::fs::metadata(dir.path().join("a.txt"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(times.modified, Some(modified));
        assert_eq!(
            provider
                .times(requester(), &HlPath::default(), "missing")
                .await,
            ItemTimes::default()
        );
    }

    #[tokio::test]
    async fn lists_files_and_folders_in_the_root() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("sub")).await.unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let mut entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(&*entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
        assert_eq!(&*entries[1].name, "sub");
        assert_eq!(&entries[1].file_type, b"fldr");
    }

    #[tokio::test]
    async fn walk_lists_a_nested_folder_before_its_contents() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("Top");
        tokio::fs::create_dir_all(top.join("A")).await.unwrap();
        tokio::fs::write(top.join("A").join("inner.txt"), b"x")
            .await
            .unwrap();
        tokio::fs::write(top.join("b.txt"), b"bb").await.unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        let summary: Vec<_> = items
            .iter()
            .map(|i| (i.path.components.join("/"), i.is_folder, i.size))
            .collect();
        assert_eq!(
            summary,
            [
                ("A".to_string(), true, 0),
                ("A/inner.txt".to_string(), false, 1),
                ("b.txt".to_string(), false, 2),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn walk_skips_an_alias_of_an_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("Top");
        tokio::fs::create_dir(&top).await.unwrap();
        tokio::fs::symlink(&top, top.join("loop")).await.unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        assert!(items.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn walk_enters_a_symlinked_folder_outside_the_walk() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("Top");
        let other = dir.path().join("Other");
        tokio::fs::create_dir(&top).await.unwrap();
        tokio::fs::create_dir(&other).await.unwrap();
        tokio::fs::write(other.join("inner.txt"), b"x")
            .await
            .unwrap();
        tokio::fs::symlink(&other, top.join("alias")).await.unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        let summary: Vec<_> = items
            .iter()
            .map(|i| (i.path.components.join("/"), i.is_folder))
            .collect();
        assert_eq!(
            summary,
            [
                ("alias".to_string(), true),
                ("alias/inner.txt".to_string(), false),
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn walk_skips_an_alias_of_a_file_it_already_has() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("Top");
        tokio::fs::create_dir(&top).await.unwrap();
        tokio::fs::write(top.join("a.txt"), b"x").await.unwrap();
        tokio::fs::symlink(top.join("a.txt"), top.join("b.txt"))
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        let names: Vec<_> = items.iter().map(|i| i.path.components.join("/")).collect();
        assert_eq!(names, ["a.txt"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_shows_a_symlinked_folder_as_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("real"))
            .await
            .unwrap();
        tokio::fs::symlink(dir.path().join("real"), dir.path().join("alias"))
            .await
            .unwrap();
        tokio::fs::symlink(dir.path().join("gone"), dir.path().join("broken"))
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        let kinds: Vec<_> = entries.iter().map(|e| (&*e.name, e.is_folder())).collect();
        assert_eq!(kinds, [("alias", true), ("broken", false), ("real", true)]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn delete_of_a_symlinked_folder_keeps_its_target() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("real"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("real").join("keep.txt"), b"x")
            .await
            .unwrap();
        tokio::fs::symlink(dir.path().join("real"), dir.path().join("alias"))
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        provider
            .delete(requester(), &HlPath::default(), "alias", true)
            .await
            .unwrap();
        assert!(!dir.path().join("alias").exists());
        assert!(dir.path().join("real").join("keep.txt").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn walk_reports_a_symlinked_files_target_size() {
        let dir = tempfile::tempdir().unwrap();
        let top = dir.path().join("Top");
        tokio::fs::create_dir(&top).await.unwrap();
        let target = dir.path().join("a-file-with-a-long-name.txt");
        tokio::fs::write(&target, b"hi").await.unwrap();
        tokio::fs::symlink(&target, top.join("link")).await.unwrap();

        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].size, 2);
    }

    #[tokio::test]
    async fn rejects_traversal_out_of_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        assert!(provider.resolve_file(&HlPath::new([".."]), "etc").is_none());
        assert!(provider.resolve_file(&HlPath::default(), "..").is_none());
        assert!(provider.resolve_file(&HlPath::default(), "a/b").is_none());
        assert!(provider
            .resolve_file(&HlPath::default(), "ok.txt")
            .is_some());
    }

    #[tokio::test]
    async fn download_and_upload_round_trip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();
        writer.write_all(b"hello").await.unwrap();
        writer.shutdown().await.unwrap();
        provider
            .finish_upload(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();

        assert!(
            provider
                .exists(requester(), &HlPath::default(), "a.txt")
                .await
        );
        assert_eq!(
            provider
                .size(requester(), &HlPath::default(), "a.txt")
                .await,
            Some(5)
        );

        let mut reader = provider
            .open_read(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello");
    }

    #[tokio::test]
    async fn open_write_resume_appends_to_an_existing_file() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();
        writer.write_all(b"hello ").await.unwrap();
        writer.shutdown().await.unwrap();

        let mut resumed = provider
            .open_write_resume(requester(), &HlPath::default(), "a.txt", 6)
            .await
            .unwrap();
        resumed.write_all(b"world").await.unwrap();
        resumed.shutdown().await.unwrap();

        assert_eq!(
            provider
                .partial_upload_size(requester(), &HlPath::default(), "a.txt")
                .await,
            Some(11)
        );
    }

    #[tokio::test]
    async fn open_write_resume_truncates_bytes_written_past_the_resume_offset() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();
        writer.write_all(b"hello world").await.unwrap();
        writer.shutdown().await.unwrap();

        let mut resumed = provider
            .open_write_resume(requester(), &HlPath::default(), "a.txt", 6)
            .await
            .unwrap();
        resumed.write_all(b"there").await.unwrap();
        resumed.shutdown().await.unwrap();

        assert_eq!(
            provider
                .partial_upload_size(requester(), &HlPath::default(), "a.txt")
                .await,
            Some(11)
        );
        let mut reader = provider
            .open_read(requester(), &HlPath::default(), "a.txt.hpf")
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello there");
    }

    #[tokio::test]
    async fn open_write_resume_errors_when_the_file_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        assert!(provider
            .open_write_resume(requester(), &HlPath::default(), "missing.txt", 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn set_comment_round_trips_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let comments_path = comments_dir.path().join("comments.json");
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        {
            let provider = DiskFileProvider::open(dir.path(), &comments_path)
                .await
                .unwrap();
            assert_eq!(
                provider
                    .comment(requester(), &HlPath::default(), "a.txt")
                    .await,
                None
            );
            provider
                .set_comment(requester(), &HlPath::default(), "a.txt", "a note")
                .await
                .unwrap();
            assert_eq!(
                provider
                    .comment(requester(), &HlPath::default(), "a.txt")
                    .await,
                Some("a note".to_string())
            );
        }

        let reopened = DiskFileProvider::open(dir.path(), &comments_path)
            .await
            .unwrap();
        assert_eq!(
            reopened
                .comment(requester(), &HlPath::default(), "a.txt")
                .await,
            Some("a note".to_string())
        );

        reopened
            .set_comment(requester(), &HlPath::default(), "a.txt", "")
            .await
            .unwrap();
        assert_eq!(
            reopened
                .comment(requester(), &HlPath::default(), "a.txt")
                .await,
            None
        );
    }

    #[tokio::test]
    async fn renaming_a_file_carries_its_comment_to_the_new_name() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            DiskFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
                .await
                .unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "a.txt", "a note")
            .await
            .unwrap();

        provider
            .rename(requester(), &HlPath::default(), "a.txt", "b.txt")
            .await
            .unwrap();

        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "b.txt")
                .await,
            Some("a note".to_string())
        );
        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "a.txt")
                .await,
            None
        );
    }

    #[tokio::test]
    async fn moving_a_file_carries_its_comment_to_the_new_directory() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            DiskFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
                .await
                .unwrap();
        tokio::fs::create_dir(dir.path().join("Dest"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "a.txt", "a note")
            .await
            .unwrap();

        provider
            .r#move(
                requester(),
                &HlPath::default(),
                "a.txt",
                &HlPath::new(["Dest"]),
            )
            .await
            .unwrap();

        assert_eq!(
            provider
                .comment(requester(), &HlPath::new(["Dest"]), "a.txt")
                .await,
            Some("a note".to_string())
        );
    }

    #[tokio::test]
    async fn renaming_a_folder_carries_its_own_and_its_contents_comments() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            DiskFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
                .await
                .unwrap();
        tokio::fs::create_dir(dir.path().join("Vacation"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("Vacation/photo.png"), b"img")
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "Vacation", "a folder note")
            .await
            .unwrap();
        provider
            .set_comment(
                requester(),
                &HlPath::new(["Vacation"]),
                "photo.png",
                "a photo note",
            )
            .await
            .unwrap();

        provider
            .rename(requester(), &HlPath::default(), "Vacation", "Trip")
            .await
            .unwrap();

        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "Trip")
                .await,
            Some("a folder note".to_string())
        );
        assert_eq!(
            provider
                .comment(requester(), &HlPath::new(["Trip"]), "photo.png")
                .await,
            Some("a photo note".to_string())
        );
    }

    #[tokio::test]
    async fn deleting_a_folder_drops_its_own_and_its_contents_comments() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let comments_path = comments_dir.path().join("comments.json");
        let provider = DiskFileProvider::open(dir.path(), &comments_path)
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("Vacation"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("Vacation/photo.png"), b"img")
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "Vacation", "a folder note")
            .await
            .unwrap();
        provider
            .set_comment(
                requester(),
                &HlPath::new(["Vacation"]),
                "photo.png",
                "a photo note",
            )
            .await
            .unwrap();

        provider
            .delete(requester(), &HlPath::default(), "Vacation", true)
            .await
            .unwrap();
        provider
            .mkdir(requester(), &HlPath::default(), "Vacation")
            .await
            .unwrap();

        let reopened = DiskFileProvider::open(dir.path(), &comments_path)
            .await
            .unwrap();
        assert_eq!(
            reopened
                .comment(requester(), &HlPath::default(), "Vacation")
                .await,
            None
        );
        assert_eq!(
            reopened
                .comment(requester(), &HlPath::new(["Vacation"]), "photo.png")
                .await,
            None
        );
    }

    #[tokio::test]
    async fn a_failed_delete_keeps_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            DiskFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
                .await
                .unwrap();
        tokio::fs::create_dir(dir.path().join("Vacation"))
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "Vacation", "a folder note")
            .await
            .unwrap();

        assert!(provider
            .delete(requester(), &HlPath::default(), "Vacation", false)
            .await
            .is_err());

        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "Vacation")
                .await,
            Some("a folder note".to_string())
        );
    }

    #[tokio::test]
    async fn comments_file_never_appears_in_a_directory_listing() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            DiskFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
                .await
                .unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        provider
            .set_comment(requester(), &HlPath::default(), "a.txt", "a note")
            .await
            .unwrap();

        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(&*entries[0].name, "a.txt");
    }

    #[tokio::test]
    async fn a_dot_name_resolves_to_nothing() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("keep.txt"), b"hi")
            .await
            .unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        let result = provider
            .delete(requester(), &HlPath::default(), ".", true)
            .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        assert!(dir.path().join("keep.txt").is_file());
        assert!(provider
            .list(requester(), &HlPath::new(["."]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn delete_trusts_the_passed_is_dir_instead_of_restatting() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("actually_a_folder"))
            .await
            .unwrap();
        let provider = DiskFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();

        // Authorized as a file (is_dir = false); must not fall back to
        // deleting the real directory on disk.
        let result = provider
            .delete(requester(), &HlPath::default(), "actually_a_folder", false)
            .await;
        assert!(result.is_err());
        assert!(dir.path().join("actually_a_folder").is_dir());
    }
}
