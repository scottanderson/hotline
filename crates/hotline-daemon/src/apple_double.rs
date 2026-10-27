//! AppleDouble sidecar files (RFC 1740). The sidecar of `dir/name` is
//! `dir/._name`. It holds the item's Finder info, comment, and resource
//! fork.

use std::collections::HashSet;
use std::ffi::OsString;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, PoisonError};
use std::task::{ready, Context, Poll};

use hotline_proto::wire::{PacketReader, PacketWriter, VecWriter};
use hotline_server::{BoxedReader, FinderInfo, SIDECAR_PREFIX};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Mutex;

const MAGIC: u32 = 0x0005_1607;
const VERSION: u32 = 0x0002_0000;
const RESOURCE_FORK_ID: u32 = 2;
const COMMENT_ID: u32 = 4;
const FINDER_INFO_ID: u32 = 9;

/// Where a written sidecar's resource fork starts.
pub const RESOURCE_FORK_OFFSET: u64 = 4096;

/// Where [`write_file`] puts the resource fork's entry, the third.
const RESOURCE_FORK_ENTRY_OFFSET: u64 = (Header::LEN + 2 * Entry::LEN) as u64;

/// The longest comment a written sidecar holds, in bytes.
const MAX_COMMENT_LEN: usize =
    RESOURCE_FORK_OFFSET as usize - Header::LEN - 3 * Entry::LEN - FinderInfoEntry::LEN;

/// The fixed start of a sidecar: magic, version, filler, and entry count.
struct Header {
    entry_count: u16,
}

impl Header {
    const LEN: usize = 26;

    fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .u32(MAGIC)
            .u32(VERSION)
            .bytes(&[0; 16])
            .u16(self.entry_count)
            .finish()
    }

    /// `None` if `buf` does not start an AppleDouble version 2 file.
    fn decode(buf: [u8; Self::LEN]) -> Option<Self> {
        let mut r = PacketReader::new(buf);
        let magic = r.u32();
        let version = r.u32();
        let _filler = r.take::<16>();
        let entry_count = r.u16();
        r.finish();
        (magic == MAGIC && version == VERSION).then_some(Self { entry_count })
    }
}

/// One entry descriptor: what the entry is, and where it is in the file.
struct Entry {
    id: u32,
    offset: u32,
    len: u32,
}

impl Entry {
    const LEN: usize = 12;

    fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .u32(self.id)
            .u32(self.offset)
            .u32(self.len)
            .finish()
    }

    const fn decode(buf: [u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(buf);
        let entry = Self {
            id: r.u32(),
            offset: r.u32(),
            len: r.u32(),
        };
        r.finish();
        entry
    }
}

/// The Finder info entry: [`FinderInfo`], then bytes that hls keeps as is.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct FinderInfoEntry {
    info: FinderInfo,
    rest: [u8; 22],
}

impl FinderInfoEntry {
    const LEN: usize = 32;

    fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .bytes(&self.info.file_type)
            .bytes(&self.info.creator)
            .u16(self.info.flags)
            .bytes(&self.rest)
            .finish()
    }

    const fn decode(buf: [u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(buf);
        let entry = Self {
            info: FinderInfo {
                file_type: r.take(),
                creator: r.take(),
                flags: r.u16(),
            },
            rest: r.take(),
        };
        r.finish();
        entry
    }
}

/// Where an entry's bytes are in the file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Span {
    offset: u64,
    len: u64,
}

/// The entries of a sidecar that hls keeps.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sidecar {
    /// The Finder info entry.
    finder_info: FinderInfoEntry,
    /// The comment entry, as UTF-8.
    pub comment: String,
    /// The resource fork entry.
    resource_fork: Span,
}

impl Sidecar {
    /// The type, creator, and flags in the Finder info entry.
    pub const fn finder_info(&self) -> FinderInfo {
        self.finder_info.info
    }

    /// Set the type, creator, and flags in the Finder info entry.
    pub const fn set_finder_info(&mut self, info: FinderInfo) {
        self.finder_info.info = info;
    }

    /// The byte length of the resource fork.
    pub const fn resource_fork_len(&self) -> u64 {
        self.resource_fork.len
    }

    fn is_empty(&self) -> bool {
        self.finder_info == FinderInfoEntry::default()
            && self.comment.is_empty()
            && self.resource_fork.len == 0
    }
}

/// The sidecars that have an open [`ResourceForkWriter`].
#[derive(Debug, Default)]
pub struct OpenForks(std::sync::Mutex<HashSet<PathBuf>>);

impl OpenForks {
    /// `true` if the sidecar at `path` has an open fork writer.
    pub fn contains(&self, path: &Path) -> bool {
        self.paths().contains(path)
    }

    /// Mark the sidecar at `path` as open until the claim drops. `None` if
    /// it is open already.
    pub fn claim(self: &Arc<Self>, path: &Path) -> Option<ForkClaim> {
        self.paths().insert(path.to_path_buf()).then(|| ForkClaim {
            forks: Arc::clone(self),
            path: path.to_path_buf(),
        })
    }

    fn paths(&self) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A sidecar's mark in [`OpenForks`]. Drop removes it.
#[derive(Debug)]
pub struct ForkClaim {
    forks: Arc<OpenForks>,
    path: PathBuf,
}

impl Drop for ForkClaim {
    fn drop(&mut self) {
        self.forks.paths().remove(&self.path);
    }
}

/// The sidecar path of the item at `item`.
pub fn path_of(item: &Path) -> PathBuf {
    let mut name = OsString::from(SIDECAR_PREFIX);
    name.push(item.file_name().unwrap_or_default());
    item.with_file_name(name)
}

/// Read the sidecar at `path`. Empty if there is none, or if it is not an
/// AppleDouble version 2 file.
pub async fn read(path: &Path) -> io::Result<Sidecar> {
    let mut file = match File::open(path).await {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Sidecar::default()),
        Err(e) => return Err(e),
    };
    let file_len = file.metadata().await?.len();
    if file_len < Header::LEN as u64 {
        return Ok(Sidecar::default());
    }
    let mut header = [0u8; Header::LEN];
    file.read_exact(&mut header).await?;
    let Some(header) = Header::decode(header) else {
        return Ok(Sidecar::default());
    };
    let table_len = usize::from(header.entry_count) * Entry::LEN;
    if (Header::LEN + table_len) as u64 > file_len {
        return Ok(Sidecar::default());
    }
    let mut table = vec![0u8; table_len];
    file.read_exact(&mut table).await?;

    let mut sidecar = Sidecar::default();
    for entry in table.as_chunks().0.iter().copied().map(Entry::decode) {
        let offset = u64::from(entry.offset);
        let len = u64::from(entry.len);
        if offset + len > file_len {
            continue;
        }
        match entry.id {
            FINDER_INFO_ID => {
                let bytes =
                    read_at(&mut file, offset, len.min(FinderInfoEntry::LEN as u64)).await?;
                let mut buf = [0; FinderInfoEntry::LEN];
                buf[..bytes.len()].copy_from_slice(&bytes);
                sidecar.finder_info = FinderInfoEntry::decode(buf);
            }
            COMMENT_ID => {
                let bytes = read_at(&mut file, offset, len.min(MAX_COMMENT_LEN as u64)).await?;
                sidecar.comment = String::from_utf8_lossy(&bytes).into_owned();
            }
            RESOURCE_FORK_ID => sidecar.resource_fork = Span { offset, len },
            _ => {}
        }
    }
    Ok(sidecar)
}

/// Write `sidecar`, as [`read`] returned it, to `path`. The resource fork
/// moves to [`RESOURCE_FORK_OFFSET`], and other entries are dropped. An
/// empty sidecar removes the file.
pub async fn write(path: &Path, sidecar: &Sidecar) -> io::Result<()> {
    if sidecar.is_empty() {
        return remove(path).await;
    }
    write_file(path, sidecar).await.map(drop)
}

/// Open the resource fork of the sidecar at `path` for reading from `at`
/// onward. Empty if there is no fork.
pub async fn open_resource_read(path: &Path, at: u64) -> io::Result<BoxedReader> {
    let Span { offset, len } = read(path).await?.resource_fork;
    if at >= len {
        return Ok(Box::new(tokio::io::empty()));
    }
    let mut file = File::open(path).await?;
    file.seek(SeekFrom::Start(offset + at)).await?;
    Ok(Box::new(file.take(len - at)))
}

/// Open the resource fork of the sidecar at `path` for writing from `at`
/// onward. Creates the sidecar if missing, and cuts the fork to `at`.
/// Call with `sidecars` held. The writer keeps `claim` until it drops.
pub async fn open_resource_write(
    path: &Path,
    at: u64,
    sidecars: Arc<Mutex<()>>,
    claim: ForkClaim,
) -> io::Result<ResourceForkWriter> {
    let mut sidecar = read(path).await?;
    sidecar.resource_fork.len = sidecar.resource_fork.len.min(at);
    let mut file = write_file(path, &sidecar).await?;
    file.set_len(RESOURCE_FORK_OFFSET + at).await?;
    file.seek(SeekFrom::Start(RESOURCE_FORK_OFFSET + at))
        .await?;
    Ok(ResourceForkWriter {
        file: Some(file),
        len: at,
        sidecars,
        closing: None,
        _claim: claim,
    })
}

/// Writes a resource fork from [`open_resource_write`]. Shutdown writes
/// the fork's length into the sidecar's entry table, with `sidecars` held.
pub struct ResourceForkWriter {
    file: Option<File>,
    len: u64,
    sidecars: Arc<Mutex<()>>,
    closing: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
    _claim: ForkClaim,
}

impl ResourceForkWriter {
    fn file(&mut self) -> io::Result<&mut File> {
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("resource fork writer is closed"))
    }
}

impl AsyncWrite for ResourceForkWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = ready!(Pin::new(self.file()?).poll_write(cx, buf))?;
        self.len += written as u64;
        Poll::Ready(Ok(written))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(self.file()?).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.closing.is_none() {
            let mut file = self
                .file
                .take()
                .ok_or_else(|| io::Error::other("resource fork writer is closed"))?;
            let entry = Entry {
                id: RESOURCE_FORK_ID,
                offset: RESOURCE_FORK_OFFSET as u32,
                len: self.len as u32,
            };
            let sidecars = Arc::clone(&self.sidecars);
            self.closing = Some(Box::pin(async move {
                let _sidecars = sidecars.lock().await;
                file.seek(SeekFrom::Start(RESOURCE_FORK_ENTRY_OFFSET))
                    .await?;
                file.write_all(&entry.encode()).await?;
                file.shutdown().await
            }));
        }
        self.closing.as_mut().expect("set above").as_mut().poll(cx)
    }
}

/// Write `sidecar` to `path` like [`write()`], also when it is empty.
async fn write_file(path: &Path, sidecar: &Sidecar) -> io::Result<File> {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .await?;
    let Span {
        offset: fork_offset,
        len: fork_len,
    } = sidecar.resource_fork;
    let moved_fork = if fork_len > 0 && fork_offset != RESOURCE_FORK_OFFSET {
        Some(read_at(&mut file, fork_offset, fork_len).await?)
    } else {
        None
    };
    let comment = cut(&sidecar.comment, MAX_COMMENT_LEN);
    let finder_info_offset = Header::LEN + 3 * Entry::LEN;
    let comment_offset = finder_info_offset + FinderInfoEntry::LEN;
    let entries = [
        Entry {
            id: FINDER_INFO_ID,
            offset: finder_info_offset as u32,
            len: FinderInfoEntry::LEN as u32,
        },
        Entry {
            id: COMMENT_ID,
            offset: comment_offset as u32,
            len: comment.len() as u32,
        },
        Entry {
            id: RESOURCE_FORK_ID,
            offset: RESOURCE_FORK_OFFSET as u32,
            len: fork_len as u32,
        },
    ];
    let mut head = VecWriter::with_capacity(RESOURCE_FORK_OFFSET as usize).bytes(
        &Header {
            entry_count: entries.len() as u16,
        }
        .encode(),
    );
    for entry in &entries {
        head = head.bytes(&entry.encode());
    }
    let mut head = head
        .bytes(&sidecar.finder_info.encode())
        .bytes(comment.as_bytes())
        .finish();
    head.resize(RESOURCE_FORK_OFFSET as usize, 0);

    file.seek(SeekFrom::Start(0)).await?;
    file.write_all(&head).await?;
    if let Some(fork) = moved_fork {
        file.write_all(&fork).await?;
    }
    file.flush().await?;
    file.set_len(RESOURCE_FORK_OFFSET + fork_len).await?;
    Ok(file)
}

/// Remove the sidecar at `path`, if there is one.
pub async fn remove(path: &Path) -> io::Result<()> {
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

/// Move the sidecar at `from` to `to`, replacing any sidecar at `to`.
pub async fn rename(from: &Path, to: &Path) -> io::Result<()> {
    match tokio::fs::rename(from, to).await {
        Err(e) if e.kind() == io::ErrorKind::NotFound => remove(to).await,
        result => result,
    }
}

/// Read `len` bytes of `file` from `offset`.
async fn read_at(file: &mut File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

/// The longest start of `s` of at most `max` bytes, cut at a character
/// boundary.
fn cut(s: &str, max: usize) -> &str {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn read_of_a_missing_or_foreign_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");
        assert_eq!(read(&path).await.unwrap(), Sidecar::default());
        tokio::fs::write(&path, b"not an AppleDouble file at all")
            .await
            .unwrap();
        assert_eq!(read(&path).await.unwrap(), Sidecar::default());
    }

    #[tokio::test]
    async fn write_and_read_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");
        let mut sidecar = Sidecar {
            comment: "a note".into(),
            ..Sidecar::default()
        };
        sidecar.set_finder_info(FinderInfo {
            file_type: *b"TEXT",
            creator: *b"ttxt",
            flags: 0x0400,
        });
        write(&path, &sidecar).await.unwrap();

        let read_back = read(&path).await.unwrap();
        assert_eq!(read_back.finder_info(), sidecar.finder_info());
        assert_eq!(read_back.comment, "a note");
        let len = tokio::fs::metadata(&path).await.unwrap().len();
        assert_eq!(len, RESOURCE_FORK_OFFSET);
    }

    #[tokio::test]
    async fn write_moves_a_foreign_resource_fork_and_keeps_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");
        let table_end = (Header::LEN + 2 * Entry::LEN) as u32;
        let mut foreign = Header { entry_count: 2 }.encode().to_vec();
        let finder_info = Entry {
            id: FINDER_INFO_ID,
            offset: table_end,
            len: 32,
        };
        let fork = Entry {
            id: RESOURCE_FORK_ID,
            offset: table_end + 32,
            len: 5,
        };
        foreign.extend_from_slice(&finder_info.encode());
        foreign.extend_from_slice(&fork.encode());
        foreign.extend_from_slice(b"APPLaust");
        foreign.extend_from_slice(&[0; 24]);
        foreign.extend_from_slice(b"rsrc!");
        tokio::fs::write(&path, &foreign).await.unwrap();

        let mut sidecar = read(&path).await.unwrap();
        assert_eq!(&sidecar.finder_info().file_type, b"APPL");
        sidecar.comment = "a note".into();
        write(&path, &sidecar).await.unwrap();

        let read_back = read(&path).await.unwrap();
        assert_eq!(&read_back.finder_info().creator, b"aust");
        assert_eq!(read_back.comment, "a note");
        assert_eq!(
            read_back.resource_fork,
            Span {
                offset: RESOURCE_FORK_OFFSET,
                len: 5
            }
        );
        let bytes = tokio::fs::read(&path).await.unwrap();
        assert_eq!(&bytes[RESOURCE_FORK_OFFSET as usize..], b"rsrc!");
    }

    #[tokio::test]
    async fn a_written_resource_fork_reads_back_and_keeps_the_comment() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");
        let sidecar = Sidecar {
            comment: "a note".into(),
            ..Sidecar::default()
        };
        write(&path, &sidecar).await.unwrap();

        let mut file = open_resource_write(&path, 0, Arc::default(), claim(&path))
            .await
            .unwrap();
        file.write_all(b"rsrc").await.unwrap();
        file.shutdown().await.unwrap();
        assert_eq!(read(&path).await.unwrap().resource_fork_len(), 4);

        let mut file = open_resource_write(&path, 2, Arc::default(), claim(&path))
            .await
            .unwrap();
        file.write_all(b"RC!").await.unwrap();
        file.shutdown().await.unwrap();
        let read_back = read(&path).await.unwrap();
        assert_eq!(read_back.comment, "a note");
        assert_eq!(read_back.resource_fork_len(), 5);

        let mut fork = Vec::new();
        let mut reader = open_resource_read(&path, 1).await.unwrap();
        reader.read_to_end(&mut fork).await.unwrap();
        assert_eq!(fork, b"sRC!");
    }

    fn claim(path: &Path) -> ForkClaim {
        Arc::<OpenForks>::default().claim(path).unwrap()
    }

    #[tokio::test]
    async fn a_resource_fork_keeps_its_length_until_the_writer_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");

        let mut file = open_resource_write(&path, 0, Arc::default(), claim(&path))
            .await
            .unwrap();
        file.write_all(b"rsrc").await.unwrap();
        file.flush().await.unwrap();
        assert_eq!(read(&path).await.unwrap().resource_fork_len(), 0);
        file.shutdown().await.unwrap();
        assert_eq!(read(&path).await.unwrap().resource_fork_len(), 4);
    }

    #[tokio::test]
    async fn writing_an_empty_sidecar_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("._a");
        let sidecar = Sidecar {
            comment: "a note".into(),
            ..Sidecar::default()
        };
        write(&path, &sidecar).await.unwrap();
        assert!(path.exists());
        write(&path, &Sidecar::default()).await.unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn path_of_prefixes_the_name() {
        let path = path_of(Path::new("files/Folder/a.txt"));
        assert_eq!(path, Path::new("files/Folder/._a.txt"));
    }
}
