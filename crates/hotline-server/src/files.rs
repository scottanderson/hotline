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

/// `file_type`/`creator` inferred from a file's extension, the same
/// fallback the reference server's non-HFS (Windows) build uses in place
/// of real Mac type/creator metadata (`_FSWinExtToMacTypeCode` in
/// `UFileSys(W).cp`).
pub fn type_and_creator_for_name(name: &str) -> ([u8; 4], [u8; 4]) {
    let ext = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match ext.to_ascii_uppercase().as_str() {
        "TXT" => (*b"TEXT", *b"ttxt"),
        "SIT" => (*b"SITD", *b"SIT!"),
        "DAT" => (*b"BINA", *b"hDmp"),
        "EXE" => (*b"DEXE", *b"CWIE"),
        "ZIP" => (*b"ZIP ", *b"ZIP "),
        "DOC" => (*b"W6BN", *b"MSWD"),
        "CPP" | "C" | "CP" | "H" | "HPP" => (*b"TEXT", *b"CWIE"),
        "HBM" => (*b"HTbm", *b"HTLC"),
        "HNZ" => (*b"HLNZ", *b"HTLC"),
        "GIF" => (*b"GIFf", *b"GKON"),
        "JPG" => (*b"JPEG", *b"GKON"),
        "DIB" | "BMP" => (*b"BMP ", *b"GKON"),
        "PCT" => (*b"PICT", *b"GKON"),
        "FPX" => (*b"FPix", *b"TVOD"),
        "MAC" | "PNT" => (*b"PNTG", *b"TVOD"),
        "PSD" => (*b"8BPS", *b"TVOD"),
        "PNG" => (*b"PNGf", *b"TVOD"),
        "SGI" => (*b"SGI ", *b"TVOD"),
        "TGA" => (*b"TPIC", *b"TVOD"),
        "TIF" => (*b"TIFF", *b"GKON"),
        "QT" | "MOV" => (*b"MooV", *b"TVOD"),
        "MPA" | "MP2" | "MPG" => (*b"MPEG", *b"TVOD"),
        "AVI" => (*b"VfW ", *b"TVOD"),
        "DV" | "DIF" => (*b"dvc!", *b"TVOD"),
        "SWF" => (*b"SWFL", *b"SWF2"),
        "FLC" | "FLI" => (*b"FLI ", *b"TVOD"),
        "QD3" | "3DM" => (*b"3DMF", *b"TVOD"),
        "MP3" => (*b"MP3 ", *b"MAmp"),
        "RAM" | "RM" | "RA" | "RAE" => (*b"RAE ", *b"REAL"),
        "AIF" => (*b"AIFF", *b"TVOD"),
        "AU" | "SND" | "ULW" => (*b"ULAW", *b"TVOD"),
        "WAV" => (*b"WAVE", *b"TVOD"),
        "SD2" => (*b"Sd2f", *b"TVOD"),
        "SMF" | "KAR" | "MID" => (*b"MiDi", *b"TVOD"),
        _ => (*b"BINA", *b"dosa"),
    }
}

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

    /// A file or folder's stored comment, or `None` if it's never had
    /// one set.
    async fn comment(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<String>;

    /// Set a file or folder's comment, replacing any prior one. An
    /// empty string clears it rather than storing an empty comment.
    async fn set_comment(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
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

    /// Like [`Self::get_dir`], but mutable; reports a missing intermediate
    /// directory as `None` instead of creating it.
    fn get_dir_mut_existing<'a>(&'a mut self, components: &[String]) -> Option<&'a mut Dir> {
        let mut current = self;
        for component in components {
            current = match current.entries.get_mut(component)? {
                Entry::Dir(d) => d,
                Entry::File(_) => return None,
            };
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

/// Move every comment keyed under `old_dir`/`old_name` to `new_dir`/
/// `new_name`: the entry itself, plus (if it's a folder) every descendant
/// comment, whose key's directory has `old_dir`/`old_name` as a prefix.
fn rekey_comments(
    comments: &mut HashMap<(Vec<String>, String), String>,
    old_dir: &[String],
    old_name: &str,
    new_dir: &[String],
    new_name: &str,
) {
    if let Some(comment) = comments.remove(&(old_dir.to_vec(), old_name.to_string())) {
        comments.insert((new_dir.to_vec(), new_name.to_string()), comment);
    }
    let mut old_prefix = old_dir.to_vec();
    old_prefix.push(old_name.to_string());
    let mut new_prefix = new_dir.to_vec();
    new_prefix.push(new_name.to_string());
    let descendants: Vec<_> = comments
        .keys()
        .filter(|(dir, _)| dir.starts_with(&old_prefix))
        .cloned()
        .collect();
    for (dir, name) in descendants {
        let comment = comments.remove(&(dir.clone(), name.clone())).unwrap();
        let mut rekeyed_dir = new_prefix.clone();
        rekeyed_dir.extend_from_slice(&dir[old_prefix.len()..]);
        comments.insert((rekeyed_dir, name), comment);
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
    comments: Mutex<HashMap<(Vec<String>, String), String>>,
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
            .map(|(name, entry)| {
                let (file_type, creator, size) = match entry {
                    Entry::Dir(_) => (*b"fldr", *b"\0\0\0\0", 0),
                    Entry::File(f) => {
                        let (file_type, creator) = type_and_creator_for_name(name);
                        (file_type, creator, f.len() as u32)
                    }
                };
                FileNameWithInfo {
                    file_type,
                    creator,
                    size,
                    name: name.clone(),
                }
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
            Some(Entry::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
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
            Entry::File(data) => {
                let (file_type, creator) = type_and_creator_for_name(name);
                FileNameWithInfo {
                    file_type,
                    creator,
                    size: data.len() as u32,
                    name: name.to_string(),
                }
            }
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
        let d = root
            .get_dir_mut_existing(&dir.components)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
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
        let d = root
            .get_dir_mut_existing(&dir.components)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        if d.entries.contains_key(new_name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let entry = d
            .entries
            .remove(old_name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        d.entries.insert(new_name.to_string(), entry);
        rekey_comments(
            &mut self.comments.lock().unwrap(),
            &dir.components,
            old_name,
            &dir.components,
            new_name,
        );
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
            .get_dir_mut_existing(&dir.components)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
            .entries
            .remove(name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let dest = match root.get_dir_mut_existing(&to_dir.components) {
            Some(dest) => dest,
            None => {
                // Put the entry back where it came from before reporting failure.
                root.get_dir_mut(&dir.components)
                    .entries
                    .insert(name.to_string(), entry);
                return Err(io::Error::from(io::ErrorKind::NotFound));
            }
        };
        if dest.entries.contains_key(name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        dest.entries.insert(name.to_string(), entry);
        rekey_comments(
            &mut self.comments.lock().unwrap(),
            &dir.components,
            name,
            &to_dir.components,
            name,
        );
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

    async fn comment(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<String> {
        let comments = self.comments.lock().unwrap();
        comments
            .get(&(dir.components.clone(), name.to_string()))
            .cloned()
    }

    async fn set_comment(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
    ) -> io::Result<()> {
        let mut comments = self.comments.lock().unwrap();
        let key = (dir.components.clone(), name.to_string());
        if comment.is_empty() {
            comments.remove(&key);
        } else {
            comments.insert(key, comment.to_string());
        }
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
    async fn delete_in_a_missing_directory_errors_without_creating_it() {
        let provider = InMemoryFileProvider::default();

        let err = provider
            .delete(requester(), &HlPath::new(["Nope"]), "a.txt", false)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(provider
            .list(requester(), &HlPath::new(["Nope"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn rename_in_a_missing_directory_errors_without_creating_it() {
        let provider = InMemoryFileProvider::default();

        let err = provider
            .rename(requester(), &HlPath::new(["Nope"]), "a.txt", "b.txt")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(provider
            .list(requester(), &HlPath::new(["Nope"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn move_from_a_missing_directory_errors_without_creating_it() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");

        let err = provider
            .r#move(
                requester(),
                &HlPath::new(["Nope"]),
                "a.txt",
                &HlPath::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(provider
            .list(requester(), &HlPath::new(["Nope"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn move_into_a_missing_directory_errors_without_creating_it() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");

        let err = provider
            .r#move(
                requester(),
                &HlPath::default(),
                "a.txt",
                &HlPath::new(["Nope"]),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(provider
            .list(requester(), &HlPath::new(["Nope"]))
            .await
            .is_none());

        // The entry should still be where it started.
        assert!(
            provider
                .exists(requester(), &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn set_comment_round_trips_and_empty_string_clears_it() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");

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

        provider
            .set_comment(requester(), &HlPath::default(), "a.txt", "")
            .await
            .unwrap();
        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "a.txt")
                .await,
            None
        );
    }

    #[tokio::test]
    async fn renaming_a_file_carries_its_comment_to_the_new_name() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");
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
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");
        provider
            .mkdir(requester(), &HlPath::default(), "Dest")
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
        let provider = InMemoryFileProvider::default();
        provider
            .mkdir(requester(), &HlPath::default(), "Vacation")
            .await
            .unwrap();
        provider.seed(&HlPath::new(["Vacation"]), "photo.png", *b"img");
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
