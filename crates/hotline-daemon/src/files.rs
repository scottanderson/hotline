//! A directory-backed [`FileProvider`], the same directory tree for
//! every login. `hotline-server` deliberately doesn't own file storage
//! (see its module docs); this is the concrete backend `hls` plugs in.

use std::io;
use std::path::PathBuf;

use async_trait::async_trait;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use hotline_server::{BoxedReader, BoxedWriter, FileProvider, FileRequester};

/// Serves one directory on disk, the same for every login.
pub struct DirectoryFileProvider {
    root: PathBuf,
}

impl DirectoryFileProvider {
    /// Open (creating if missing) a directory to serve.
    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await?;
        Ok(Self { root })
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
impl FileProvider for DirectoryFileProvider {
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
            entries.push(FileNameWithInfo {
                file_type: if metadata.is_dir() {
                    *b"fldr"
                } else {
                    *b"\0\0\0\0"
                },
                creator: *b"\0\0\0\0",
                size: if metadata.is_dir() {
                    0
                } else {
                    metadata.len() as u32
                },
                name,
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
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        Ok(Box::new(tokio::fs::File::create(path).await?))
    }

    async fn info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FileNameWithInfo> {
        let path = self.resolve_file(dir, name)?;
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        Some(FileNameWithInfo {
            file_type: if metadata.is_dir() {
                *b"fldr"
            } else {
                *b"\0\0\0\0"
            },
            creator: *b"\0\0\0\0",
            size: if metadata.is_dir() {
                0
            } else {
                metadata.len() as u32
            },
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
        tokio::fs::rename(old_path, new_path).await
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
        tokio::fs::rename(old_path, new_path).await
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

        let provider = DirectoryFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DirectoryFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DirectoryFileProvider::open(dir.path()).await.unwrap();

        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "a.txt")
            .await
            .unwrap();
        writer.write_all(b"hello").await.unwrap();
        writer.shutdown().await.unwrap();

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
    async fn delete_trusts_the_passed_is_dir_instead_of_restatting() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir(dir.path().join("actually_a_folder"))
            .await
            .unwrap();
        let provider = DirectoryFileProvider::open(dir.path()).await.unwrap();

        // Authorized as a file (is_dir = false); must not fall back to
        // deleting the real directory on disk.
        let result = provider
            .delete(requester(), &HlPath::default(), "actually_a_folder", false)
            .await;
        assert!(result.is_err());
        assert!(dir.path().join("actually_a_folder").is_dir());
    }
}
