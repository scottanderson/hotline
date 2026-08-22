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
use std::path::PathBuf;

use async_trait::async_trait;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use hotline_server::{
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, FileProvider,
    FileRequester,
};
use tokio::sync::RwLock;

/// Serves one directory on disk, the same for every login.
pub struct FileFileProvider {
    root: PathBuf,
    comments_path: PathBuf,
    comments: RwLock<HashMap<String, String>>,
}

impl FileFileProvider {
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
        let bytes = serde_json::to_vec_pretty(&*comments)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&self.comments_path, bytes).await
    }

    /// Resolve a directory path under `root`, rejecting any component
    /// that would escape it (`..`, a bare path separator).
    fn resolve_dir(&self, dir: &HlPath) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for component in &dir.components {
            if component.is_empty() || component.contains(['/', '\\']) || component == ".." {
                return None;
            }
            path.push(component);
        }
        Some(path)
    }

    /// Resolve a file path (directory components plus a file name) under
    /// `root`, with the same escape checks as [`Self::resolve_dir`].
    fn resolve_file(&self, dir: &HlPath, name: &str) -> Option<PathBuf> {
        if name.is_empty() || name.contains(['/', '\\']) || name == ".." {
            return None;
        }
        let mut path = self.resolve_dir(dir)?;
        path.push(name);
        Some(path)
    }
}

#[async_trait]
impl FileProvider for FileFileProvider {
    async fn list(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        let path = self.resolve_dir(dir)?;
        let mut read_dir = tokio::fs::read_dir(&path).await.ok()?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let Ok(metadata) = entry.metadata().await else {
                continue;
            };
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            entries.push(if metadata.is_dir() {
                FileNameWithInfo {
                    file_type: *b"fldr",
                    creator: *b"\0\0\0\0",
                    size: 0,
                    name,
                }
            } else {
                let (file_type, creator) = type_and_creator_for_name(&name);
                FileNameWithInfo {
                    file_type,
                    creator,
                    size: metadata.len() as u32,
                    name,
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
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
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
                name: name.to_string(),
            });
        }
        let (file_type, creator) = type_and_creator_for_name(name);
        Some(FileNameWithInfo {
            file_type,
            creator,
            size: metadata.len() as u32,
            name: name.to_string(),
        })
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
        if is_dir {
            tokio::fs::remove_dir_all(&path).await
        } else {
            tokio::fs::remove_file(&path).await
        }
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
        let bytes = serde_json::to_vec_pretty(&*comments)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&self.comments_path, bytes).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requester() -> FileRequester<'static> {
        FileRequester { login: "alice" }
    }

    #[tokio::test]
    async fn lists_files_and_folders_in_the_root() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        tokio::fs::create_dir(dir.path().join("sub")).await.unwrap();

        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
            .await
            .unwrap();
        let mut entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
        assert_eq!(entries[1].name, "sub");
        assert_eq!(&entries[1].file_type, b"fldr");
    }

    #[tokio::test]
    async fn rejects_traversal_out_of_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
            let provider = FileFileProvider::open(dir.path(), &comments_path)
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

        let reopened = FileFileProvider::open(dir.path(), &comments_path)
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
            FileFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
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
            FileFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
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
            FileFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
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
    async fn comments_file_never_appears_in_a_directory_listing() {
        let dir = tempfile::tempdir().unwrap();
        let comments_dir = tempfile::tempdir().unwrap();
        let provider =
            FileFileProvider::open(dir.path(), comments_dir.path().join("comments.json"))
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
        assert_eq!(entries[0].name, "a.txt");
    }

    #[tokio::test]
    async fn delete_trusts_the_passed_is_dir_instead_of_restatting() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("actually_a_folder"))
            .await
            .unwrap();
        let provider = FileFileProvider::open(dir.path(), dir.path().join("comments.json"))
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
