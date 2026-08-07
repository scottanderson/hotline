//! File storage backing `FILE_LIST`/`FILE_GET`/`FILE_PUT`.
//!
//! [`FileProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`InMemoryFileProvider`] as a default. The
//! embedding application supplies the real backend to
//! [`crate::Server::new`] the same way it supplies an [`AccountProvider`]
//! (see that trait's module docs) — and since every call carries a
//! [`FileRequester`], a backend can show different logins different
//! files (a per-account home directory, a members-only folder, ...)
//! rather than serving one directory tree to everyone.
//!
//! [`AccountProvider`]: crate::AccountProvider

use std::collections::HashMap;
use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};

use hotline_proto::types::{FileNameWithInfo, HlPath};

/// Which login a file operation is being performed on behalf of, so a
/// [`FileProvider`] can decide what that login is allowed to see.
#[derive(Debug, Clone, Copy)]
pub struct FileRequester<'a> {
    /// The requesting session's account login.
    pub login: &'a str,
}

/// A boxed, already-open async byte source, the shape [`FileProvider::open_read`]
/// returns so implementations aren't tied to `tokio::fs::File` specifically.
pub type BoxedReader = Box<dyn AsyncRead + Send + Unpin>;

/// A boxed, already-open async byte sink. See [`BoxedReader`].
pub type BoxedWriter = Box<dyn AsyncWrite + Send + Unpin>;

/// Storage backing `FILE_LIST`/`FILE_GET`/`FILE_PUT`. `hotline-server`
/// doesn't own file storage any more than it owns account storage (see
/// [`crate::AccountProvider`]'s module docs); the embedding application
/// supplies the concrete backend to [`crate::Server::new`].
#[async_trait]
pub trait FileProvider: Send + Sync {
    /// List a directory as seen by `requester`. `None` if it doesn't
    /// exist (or isn't visible to `requester`).
    async fn list(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>>;

    /// A file's size in bytes, or `None` if it doesn't exist, is a
    /// folder, or isn't visible to `requester`.
    async fn size(&self, requester: FileRequester<'_>, dir: &HlPath, name: &str) -> Option<u64>;

    /// Whether a file or folder already exists at this path.
    async fn exists(&self, requester: FileRequester<'_>, dir: &HlPath, name: &str) -> bool;

    /// Open a file for reading, for a `FILE_GET` download.
    async fn open_read(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedReader>;

    /// Create a file for writing, for a `FILE_PUT` upload.
    async fn create_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedWriter>;

    /// One file or folder's listing metadata, or `None` if it doesn't
    /// exist (or isn't visible to `requester`). Whether it's a folder is
    /// `entry.file_type == *b"fldr"`, matching [`Self::list`]'s entries.
    async fn info(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FileNameWithInfo>;

    /// Delete a file, or a folder and everything under it. Trusts
    /// `is_dir` rather than re-deriving it, closing a TOCTOU window.
    async fn delete(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        is_dir: bool,
    ) -> io::Result<()>;

    /// Create a folder.
    async fn mkdir(&self, requester: FileRequester<'_>, dir: &HlPath, name: &str)
        -> io::Result<()>;

    /// Rename a file or folder in place.
    async fn rename(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        old_name: &str,
        new_name: &str,
    ) -> io::Result<()>;

    /// Move a file or folder into another directory, keeping its name.
    async fn r#move(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        to_dir: &HlPath,
    ) -> io::Result<()>;

    /// Create an alias at `link_dir`/`link_name` pointing at
    /// `target_dir`/`target_name`.
    async fn symlink(
        &self,
        requester: FileRequester<'_>,
        target_dir: &HlPath,
        target_name: &str,
        link_dir: &HlPath,
        link_name: &str,
    ) -> io::Result<()>;
}

#[derive(Clone)]
enum Entry {
    File(Vec<u8>),
    Dir(Dir),
}

#[derive(Clone, Default)]
struct Dir {
    entries: HashMap<String, Entry>,
}

impl Dir {
    fn get_dir<'a>(&'a self, components: &[String]) -> Option<&'a Dir> {
        let mut current = self;
        for component in components {
            match current.entries.get(component)? {
                Entry::Dir(d) => current = d,
                Entry::File(_) => return None,
            }
        }
        Some(current)
    }

    fn get_dir_mut<'a>(&'a mut self, components: &[String]) -> &'a mut Dir {
        let mut current = self;
        for component in components {
            current = match current
                .entries
                .entry(component.clone())
                .or_insert_with(|| Entry::Dir(Dir::default()))
            {
                Entry::Dir(d) => d,
                Entry::File(_) => panic!("path component names an existing file"),
            };
        }
        current
    }
}

/// A [`FileProvider`] that keeps files in memory only, the same tree for
/// every login; nothing survives a restart. Used as
/// [`crate::state::SharedState`]'s default and by this crate's own
/// tests. A real server wants a backend with actual persistence and
/// per-login scoping (see [`crate::AccountProvider`]'s module docs for
/// the same split on account storage).
#[derive(Default)]
pub struct InMemoryFileProvider {
    root: Arc<Mutex<Dir>>,
}

impl InMemoryFileProvider {
    /// Seed a file at `dir`/`name` with `data`, creating any missing
    /// intermediate directories. For tests/fixtures.
    pub fn seed(&self, dir: &HlPath, name: &str, data: impl Into<Vec<u8>>) {
        let mut root = self.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components);
        d.entries.insert(name.to_string(), Entry::File(data.into()));
    }
}

/// Writes accumulate directly into the shared tree as they arrive
/// (rather than being buffered until shutdown), so a provider consumer
/// that never calls `shutdown()` still sees the bytes it wrote.
struct MemWriter {
    root: Arc<Mutex<Dir>>,
    dir: Vec<String>,
    name: String,
}

impl MemWriter {
    fn new(root: Arc<Mutex<Dir>>, dir: Vec<String>, name: String) -> Self {
        {
            let mut guard = root.lock().unwrap();
            let d = guard.get_dir_mut(&dir);
            d.entries.insert(name.clone(), Entry::File(Vec::new()));
        }
        Self { root, dir, name }
    }
}

impl AsyncWrite for MemWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.root.lock().unwrap();
        let d = guard.get_dir_mut(&self.dir);
        if let Some(Entry::File(data)) = d.entries.get_mut(&self.name) {
            data.extend_from_slice(buf);
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[async_trait]
impl FileProvider for InMemoryFileProvider {
    async fn list(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        let root = self.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        let mut entries: Vec<FileNameWithInfo> = d
            .entries
            .iter()
            .map(|(name, entry)| match entry {
                Entry::Dir(_) => FileNameWithInfo {
                    file_type: *b"fldr",
                    creator: *b"\0\0\0\0",
                    size: 0,
                    name: name.clone(),
                },
                Entry::File(data) => FileNameWithInfo {
                    file_type: *b"\0\0\0\0",
                    creator: *b"\0\0\0\0",
                    size: data.len() as u32,
                    name: name.clone(),
                },
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Some(entries)
    }

    async fn size(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> Option<u64> {
        let root = self.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        match d.entries.get(name)? {
            Entry::File(data) => Some(data.len() as u64),
            Entry::Dir(_) => None,
        }
    }

    async fn exists(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> bool {
        let root = self.root.lock().unwrap();
        root.get_dir(&dir.components)
            .is_some_and(|d| d.entries.contains_key(name))
    }

    async fn open_read(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedReader> {
        let root = self.root.lock().unwrap();
        let d = root
            .get_dir(&dir.components)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        match d.entries.get(name) {
            Some(Entry::File(data)) => Ok(Box::new(std::io::Cursor::new(data.clone()))),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    async fn create_write(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedWriter> {
        Ok(Box::new(MemWriter::new(
            self.root.clone(),
            dir.components.clone(),
            name.to_string(),
        )))
    }

    async fn info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FileNameWithInfo> {
        let root = self.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        Some(match d.entries.get(name)? {
            Entry::Dir(_) => FileNameWithInfo {
                file_type: *b"fldr",
                creator: *b"\0\0\0\0",
                size: 0,
                name: name.to_string(),
            },
            Entry::File(data) => FileNameWithInfo {
                file_type: *b"\0\0\0\0",
                creator: *b"\0\0\0\0",
                size: data.len() as u32,
                name: name.to_string(),
            },
        })
    }

    async fn delete(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        _is_dir: bool,
    ) -> io::Result<()> {
        let mut root = self.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components);
        d.entries
            .remove(name)
            .map(|_| ())
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    async fn mkdir(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<()> {
        let mut root = self.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components);
        if d.entries.contains_key(name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        d.entries
            .insert(name.to_string(), Entry::Dir(Dir::default()));
        Ok(())
    }

    async fn rename(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        old_name: &str,
        new_name: &str,
    ) -> io::Result<()> {
        let mut root = self.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components);
        if d.entries.contains_key(new_name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let entry = d
            .entries
            .remove(old_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        d.entries.insert(new_name.to_string(), entry);
        Ok(())
    }

    async fn r#move(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        to_dir: &HlPath,
    ) -> io::Result<()> {
        let mut root = self.root.lock().unwrap();
        let entry = root
            .get_dir_mut(&dir.components)
            .entries
            .remove(name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let dest = root.get_dir_mut(&to_dir.components);
        if dest.entries.contains_key(name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        dest.entries.insert(name.to_string(), entry);
        Ok(())
    }

    /// Simplified: deep-copies the target entry rather than a real
    /// symlink, since this in-memory tree has no symlink entry kind.
    async fn symlink(
        &self,
        _requester: FileRequester<'_>,
        target_dir: &HlPath,
        target_name: &str,
        link_dir: &HlPath,
        link_name: &str,
    ) -> io::Result<()> {
        let mut root = self.root.lock().unwrap();
        let entry = match root
            .get_dir(&target_dir.components)
            .and_then(|d| d.entries.get(target_name))
        {
            Some(entry) => entry.clone(),
            None => return Err(io::Error::from(io::ErrorKind::NotFound)),
        };
        let dest = root.get_dir_mut(&link_dir.components);
        if dest.entries.contains_key(link_name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        dest.entries.insert(link_name.to_string(), entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn requester() -> FileRequester<'static> {
        FileRequester { login: "alice" }
    }

    #[tokio::test]
    async fn seeded_file_is_listed_and_readable() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hello");

        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 5);

        assert_eq!(
            provider
                .size(requester(), &HlPath::default(), "a.txt")
                .await,
            Some(5)
        );
        assert!(
            provider
                .exists(requester(), &HlPath::default(), "a.txt")
                .await
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
    async fn create_write_persists_bytes_without_an_explicit_shutdown() {
        let provider = InMemoryFileProvider::default();
        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "b.txt")
            .await
            .unwrap();
        writer.write_all(b"world").await.unwrap();

        assert_eq!(
            provider
                .size(requester(), &HlPath::default(), "b.txt")
                .await,
            Some(5)
        );
    }

    #[tokio::test]
    async fn list_of_missing_directory_is_none() {
        let provider = InMemoryFileProvider::default();
        assert!(provider
            .list(requester(), &HlPath::new(["nope"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn seed_creates_missing_intermediate_directories() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::new(["Uploads"]), "a.txt", *b"hi");

        let entries = provider
            .list(requester(), &HlPath::new(["Uploads"]))
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
    }

    #[tokio::test]
    async fn symlink_of_a_folder_deep_copies_its_contents() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::new(["Folder"]), "a.txt", *b"hi");

        provider
            .symlink(
                requester(),
                &HlPath::default(),
                "Folder",
                &HlPath::default(),
                "Alias",
            )
            .await
            .unwrap();

        let entries = provider
            .list(requester(), &HlPath::new(["Alias"]))
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
    }
}
