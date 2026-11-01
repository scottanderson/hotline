//! AppleDouble sidecar files (RFC 1740). The sidecar of `dir/name` is
//! `dir/._name`. It holds the item's Finder info, comment, and resource
//! fork.
//!
//! A sidecar is opened and checked as xnu's `bsd/vfs/vfs_xattr.c` does
//! (xnu-11417.101.15). A read finds nothing in a sidecar that fails a
//! check, and a change to it returns an error.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::Metadata;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, PoisonError};
use std::task::{ready, Context, Poll};

use hotline_proto::wire::{PacketReader, PacketWriter, VecWriter};
use hotline_server::{BoxedReader, FinderInfo};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, MutexGuard};

/// The start of a sidecar's name.
pub const PREFIX: &str = "._";

const MAGIC: u32 = 0x0005_1607;
const VERSION: u32 = 0x0002_0000;
const RESOURCE_FORK_ID: u32 = 2;
const COMMENT_ID: u32 = 4;
const FINDER_INFO_ID: u32 = 9;

/// The most entries that a sidecar can hold.
const MAX_ENTRIES: u16 = 15;
/// The most bytes that a read of the header takes.
const ATTR_MAX_HDR_SIZE: u32 = 65536;

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
}

/// One entry descriptor: what the entry is, and where it is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    id: u32,
    offset: u32,
    len: u32,
}

impl Entry {
    const LEN: usize = 12;

    /// The file offset of the end of the entry. `None` if it is past 4 GiB.
    const fn end(&self) -> Option<u32> {
        self.offset.checked_add(self.len)
    }

    fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .u32(self.id)
            .u32(self.offset)
            .u32(self.len)
            .finish()
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

/// The lock on the sidecars, and the sidecars that have an open
/// [`ResourceForkWriter`].
#[derive(Debug, Default)]
pub struct Sidecars {
    changing: Mutex<()>,
    open: std::sync::Mutex<HashSet<PathBuf>>,
}

impl Sidecars {
    /// Hold the lock while a sidecar changes or moves.
    pub async fn lock(&self) -> MutexGuard<'_, ()> {
        self.changing.lock().await
    }

    /// `true` if the sidecar at `path` has an open fork writer.
    pub fn contains(&self, path: &Path) -> bool {
        self.paths().contains(path)
    }

    /// Mark the sidecar at `path` as open until the claim drops. `None` if
    /// it is open already.
    pub fn claim(self: &Arc<Self>, path: &Path) -> Option<ForkClaim> {
        self.paths().insert(path.to_path_buf()).then(|| ForkClaim {
            sidecars: Arc::clone(self),
            path: path.to_path_buf(),
        })
    }

    fn paths(&self) -> std::sync::MutexGuard<'_, HashSet<PathBuf>> {
        self.open.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A sidecar's mark in [`Sidecars`]. Drop removes it.
#[derive(Debug)]
pub struct ForkClaim {
    sidecars: Arc<Sidecars>,
    path: PathBuf,
}

impl Drop for ForkClaim {
    fn drop(&mut self) {
        self.sidecars.paths().remove(&self.path);
    }
}

/// The sidecar path of the item at `item`.
pub fn path_of(item: &Path) -> PathBuf {
    let mut name = OsString::from(PREFIX);
    name.push(item.file_name().unwrap_or_default());
    item.with_file_name(name)
}

/// Read the sidecar of `item`. Empty if there is none, or if it fails a
/// check.
pub async fn read(item: &Path) -> io::Result<Sidecar> {
    let Some(mut file) = open_sidecar(item, Access::Read).await? else {
        return Ok(Sidecar::default());
    };
    let Some(entries) = check_file(&mut file).await? else {
        tracing::debug!(item = %item.display(), "a sidecar is not a valid AppleDouble file");
        return Ok(Sidecar::default());
    };
    let mut sidecar = Sidecar::default();
    for entry in entries {
        let offset = u64::from(entry.offset);
        let len = u64::from(entry.len);
        match entry.id {
            FINDER_INFO_ID => {
                let bytes =
                    read_at(&mut file, offset, len.min(FinderInfoEntry::LEN as u64)).await?;
                let mut buf = [0; FinderInfoEntry::LEN];
                buf.iter_mut().zip(&bytes).for_each(|(b, &v)| *b = v);
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

/// Write `sidecar`, as [`read`] returned it, to the sidecar of `item`. The
/// resource fork moves to [`RESOURCE_FORK_OFFSET`], and other entries are
/// dropped. An empty sidecar removes the file, unless it fails a check.
pub async fn write(item: &Path, sidecar: &Sidecar) -> io::Result<()> {
    if sidecar.is_empty() {
        let Some(mut file) = open_sidecar(item, Access::Read).await? else {
            return Ok(());
        };
        if check_file(&mut file).await?.is_none() {
            return Ok(());
        }
        return remove(&path_of(item)).await;
    }
    write_file(item, sidecar).await.map(drop)
}

/// Open the resource fork of `item` for reading from `at` onward. Empty if
/// there is no fork.
pub async fn open_resource_read(item: &Path, at: u64) -> io::Result<BoxedReader> {
    let Span { offset, len } = read(item).await?.resource_fork;
    if at >= len {
        return Ok(Box::new(tokio::io::empty()));
    }
    let mut file = File::open(path_of(item)).await?;
    file.seek(SeekFrom::Start(offset + at)).await?;
    Ok(Box::new(file.take(len - at)))
}

/// Open the resource fork of `item` for writing from `at` onward. Creates
/// the sidecar if missing, and cuts the fork to `at`. Call with the
/// [`Sidecars`] lock held. The writer keeps `claim` until it drops.
pub async fn open_resource_write(
    item: &Path,
    at: u64,
    claim: ForkClaim,
) -> io::Result<ResourceForkWriter> {
    let mut sidecar = read(item).await?;
    sidecar.resource_fork.len = sidecar.resource_fork.len.min(at);
    let mut file = write_file(item, &sidecar).await?;
    file.set_len(RESOURCE_FORK_OFFSET + at).await?;
    file.seek(SeekFrom::Start(RESOURCE_FORK_OFFSET + at))
        .await?;
    Ok(ResourceForkWriter {
        file: Some(file),
        len: at,
        closing: None,
        claim,
    })
}

/// Writes a resource fork from [`open_resource_write`]. Shutdown writes
/// the fork's length into the sidecar's entry table, with the [`Sidecars`]
/// lock held.
pub struct ResourceForkWriter {
    file: Option<File>,
    len: u64,
    closing: Option<Pin<Box<dyn Future<Output = io::Result<()>> + Send>>>,
    claim: ForkClaim,
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
        let closing = if let Some(closing) = self.closing.take() {
            closing
        } else {
            let mut file = self
                .file
                .take()
                .ok_or_else(|| io::Error::other("resource fork writer is closed"))?;
            let entry = Entry {
                id: RESOURCE_FORK_ID,
                offset: RESOURCE_FORK_OFFSET as u32,
                len: self.len as u32,
            };
            let sidecars = Arc::clone(&self.claim.sidecars);
            Box::pin(async move {
                let _sidecars = sidecars.lock().await;
                file.seek(SeekFrom::Start(RESOURCE_FORK_ENTRY_OFFSET))
                    .await?;
                file.write_all(&entry.encode()).await?;
                file.shutdown().await
            })
        };
        self.closing.insert(closing).as_mut().poll(cx)
    }
}

/// Write `sidecar` to the sidecar of `item` like [`write()`], also when it
/// is empty. Create the sidecar if missing. An error of kind `InvalidData`
/// if it fails a check.
async fn write_file(item: &Path, sidecar: &Sidecar) -> io::Result<File> {
    let mut file = open_sidecar(item, Access::Create)
        .await?
        .ok_or_else(not_valid)?;
    if file.metadata().await?.len() > 0 && check_file(&mut file).await?.is_none() {
        return Err(not_valid());
    }
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

/// How [`open_sidecar`] opens a sidecar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Read,
    /// Open it for writing, and create it if missing.
    Create,
}

/// Open the sidecar of `item` for `access`. `None` if it is missing, if it
/// is not a regular file, or if its owner is not the owner of `item`.
async fn open_sidecar(item: &Path, access: Access) -> io::Result<Option<File>> {
    let path = path_of(item);
    let existing = match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) => Some(metadata),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    if existing
        .as_ref()
        .is_some_and(|metadata| !metadata.is_file())
    {
        return Ok(None);
    }
    let item_metadata = tokio::fs::metadata(item).await.ok();
    let file = match (access, &existing) {
        (Access::Create, _) => {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .await?
        }
        (Access::Read, Some(_)) => File::open(&path).await?,
        (Access::Read, None) => return Ok(None),
    };
    if existing.is_none() {
        if let Some(item_metadata) = &item_metadata {
            set_new_owner(&file, item_metadata).await;
        }
    }
    let metadata = file.metadata().await?;
    let owner = item_metadata.as_ref().and_then(owner_of);
    if owner.is_some() && owner != owner_of(&metadata) {
        if existing.is_none() {
            remove(&path).await?;
        }
        return Ok(None);
    }
    if access == Access::Create && metadata.len() == 0 && link_count(&metadata) > 1 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(Some(file))
}

/// Give a new sidecar the owner, the group, and the read and write bits
/// of the item with `item_metadata`. A change that the user may not make
/// does not occur.
#[cfg(unix)]
async fn set_new_owner(file: &File, item_metadata: &Metadata) {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let mode = item_metadata.permissions().mode() & 0o666;
    let mode_set = file
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .await;
    let (uid, gid) = (item_metadata.uid(), item_metadata.gid());
    let owner_set = std::os::unix::fs::fchown(file, Some(uid), Some(gid))
        .or_else(|_| std::os::unix::fs::fchown(file, Some(uid), None));
    if let Err(e) = mode_set.and(owner_set) {
        tracing::debug!(error = %e, "a new sidecar did not get the owner or mode of its item");
    }
}

/// Give a new sidecar the owner, the group, and the read and write bits
/// of the item with `item_metadata`.
#[cfg(not(unix))]
async fn set_new_owner(_file: &File, _item_metadata: &Metadata) {}

/// The user ID of the owner of a file.
#[cfg(unix)]
fn owner_of(metadata: &Metadata) -> Option<u32> {
    Some(std::os::unix::fs::MetadataExt::uid(metadata))
}

/// The user ID of the owner of a file.
#[cfg(not(unix))]
const fn owner_of(_metadata: &Metadata) -> Option<u32> {
    None
}

/// The number of hard links to a file.
#[cfg(unix)]
fn link_count(metadata: &Metadata) -> u64 {
    std::os::unix::fs::MetadataExt::nlink(metadata)
}

/// The number of hard links to a file.
#[cfg(not(unix))]
const fn link_count(_metadata: &Metadata) -> u64 {
    1
}

/// The entries of the sidecar in `file`. `None` if it fails a check.
async fn check_file(file: &mut File) -> io::Result<Option<Vec<Entry>>> {
    let file_len = file.metadata().await?.len();
    let mut buf = Vec::new();
    file.seek(SeekFrom::Start(0)).await?;
    (&mut *file)
        .take(u64::from(ATTR_MAX_HDR_SIZE))
        .read_to_end(&mut buf)
        .await?;
    Ok(check_header(&buf, file_len))
}

/// The entry table at the start of `buf`, from a file of `file_len` bytes.
/// `None` if the header is not valid, if there are no entries or more than
/// [`MAX_ENTRIES`], or if an entry starts in the table, ends past the end
/// of the file, or overlaps an entry before it.
fn check_header(buf: &[u8], file_len: u64) -> Option<Vec<Entry>> {
    let raw_len = buf.len();
    if raw_len < Header::LEN || u32_at(buf, 0)? != MAGIC || u32_at(buf, 4)? != VERSION {
        return None;
    }
    let count = u16_at(buf, 24)?;
    if !(1..=MAX_ENTRIES).contains(&count) {
        return None;
    }
    let table_end = Header::LEN + usize::from(count) * Entry::LEN;
    if raw_len < table_end {
        return None;
    }
    let entries: Vec<Entry> = (0..usize::from(count))
        .map(|i| {
            let at = Header::LEN + i * Entry::LEN;
            Some(Entry {
                id: u32_at(buf, at)?,
                offset: u32_at(buf, at + 4)?,
                len: u32_at(buf, at + 8)?,
            })
        })
        .collect::<Option<_>>()?;
    let valid = entries.iter().enumerate().all(|(i, entry)| {
        entry.end().is_some_and(|end| {
            entry.offset as usize >= table_end
                && u64::from(end) <= file_len
                && entries.iter().take(i).all(|before| {
                    end <= before.offset
                        || before
                            .end()
                            .is_some_and(|before_end| before_end <= entry.offset)
                })
        })
    });
    valid.then_some(entries)
}

/// The error for a sidecar that fails a check.
fn not_valid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the sidecar is not a valid AppleDouble file",
    )
}

/// The big-endian `u32` at `at` in `buf`.
fn u32_at(buf: &[u8], at: usize) -> Option<u32> {
    buf.get(at..)?
        .first_chunk()
        .copied()
        .map(u32::from_be_bytes)
}

/// The big-endian `u16` at `at` in `buf`.
fn u16_at(buf: &[u8], at: usize) -> Option<u16> {
    buf.get(at..)?
        .first_chunk()
        .copied()
        .map(u16::from_be_bytes)
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
    s.get(..s.floor_char_boundary(max)).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory with the item `a.txt`, and the item's path.
    fn item() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let item = dir.path().join("a.txt");
        std::fs::write(&item, b"data").unwrap();
        (dir, item)
    }

    fn noted() -> Sidecar {
        Sidecar {
            comment: "a note".into(),
            ..Sidecar::default()
        }
    }

    /// A sidecar with the Finder info at 50 and `fork` after it.
    fn two_entries(fork: &[u8]) -> Vec<u8> {
        let table_end = (Header::LEN + 2 * Entry::LEN) as u32;
        let mut file = Header { entry_count: 2 }.encode().to_vec();
        let finder_info = Entry {
            id: FINDER_INFO_ID,
            offset: table_end,
            len: 32,
        };
        let fork_entry = Entry {
            id: RESOURCE_FORK_ID,
            offset: table_end + 32,
            len: fork.len() as u32,
        };
        file.extend_from_slice(&finder_info.encode());
        file.extend_from_slice(&fork_entry.encode());
        file.extend_from_slice(b"APPLaust");
        file.extend_from_slice(&[0; 24]);
        file.extend_from_slice(fork);
        file
    }

    #[tokio::test]
    async fn read_of_a_missing_or_empty_file_is_empty() {
        let (_dir, item) = item();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
        std::fs::write(path_of(&item), b"").unwrap();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
    }

    #[tokio::test]
    async fn write_and_read_round_trip() {
        let (_dir, item) = item();
        let mut sidecar = noted();
        sidecar.set_finder_info(FinderInfo {
            file_type: *b"TEXT",
            creator: *b"ttxt",
            flags: 0x0400,
        });
        write(&item, &sidecar).await.unwrap();

        let read_back = read(&item).await.unwrap();
        assert_eq!(read_back.finder_info(), sidecar.finder_info());
        assert_eq!(read_back.comment, "a note");
        let len = tokio::fs::metadata(path_of(&item)).await.unwrap().len();
        assert_eq!(len, RESOURCE_FORK_OFFSET);
    }

    #[tokio::test]
    async fn write_moves_a_foreign_resource_fork_and_keeps_it() {
        let (_dir, item) = item();
        std::fs::write(path_of(&item), two_entries(b"rsrc!")).unwrap();

        let mut sidecar = read(&item).await.unwrap();
        assert_eq!(&sidecar.finder_info().file_type, b"APPL");
        sidecar.comment = "a note".into();
        write(&item, &sidecar).await.unwrap();

        let read_back = read(&item).await.unwrap();
        assert_eq!(&read_back.finder_info().creator, b"aust");
        assert_eq!(read_back.comment, "a note");
        assert_eq!(
            read_back.resource_fork,
            Span {
                offset: RESOURCE_FORK_OFFSET,
                len: 5
            }
        );
        let bytes = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(&bytes[RESOURCE_FORK_OFFSET as usize..], b"rsrc!");
    }

    #[tokio::test]
    async fn a_written_resource_fork_reads_back_and_keeps_the_comment() {
        let (_dir, item) = item();
        write(&item, &noted()).await.unwrap();

        let mut file = open_resource_write(&item, 0, claim(&item)).await.unwrap();
        file.write_all(b"rsrc").await.unwrap();
        file.shutdown().await.unwrap();
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 4);

        let mut file = open_resource_write(&item, 2, claim(&item)).await.unwrap();
        file.write_all(b"RC!").await.unwrap();
        file.shutdown().await.unwrap();
        let read_back = read(&item).await.unwrap();
        assert_eq!(read_back.comment, "a note");
        assert_eq!(read_back.resource_fork_len(), 5);

        let mut fork = Vec::new();
        let mut reader = open_resource_read(&item, 1).await.unwrap();
        reader.read_to_end(&mut fork).await.unwrap();
        assert_eq!(fork, b"sRC!");
    }

    fn claim(item: &Path) -> ForkClaim {
        Arc::<Sidecars>::default().claim(&path_of(item)).unwrap()
    }

    #[tokio::test]
    async fn a_resource_fork_keeps_its_length_until_the_writer_closes() {
        let (_dir, item) = item();

        let mut file = open_resource_write(&item, 0, claim(&item)).await.unwrap();
        file.write_all(b"rsrc").await.unwrap();
        file.flush().await.unwrap();
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 0);
        file.shutdown().await.unwrap();
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 4);
    }

    #[tokio::test]
    async fn writing_an_empty_sidecar_removes_the_file() {
        let (_dir, item) = item();
        write(&item, &noted()).await.unwrap();
        assert!(path_of(&item).exists());
        write(&item, &Sidecar::default()).await.unwrap();
        assert!(!path_of(&item).exists());
    }

    /// Check that `bytes` in a sidecar read as nothing, and that a change
    /// fails and leaves the file as it was.
    async fn assert_not_valid(bytes: &[u8]) {
        let (_dir, item) = item();
        std::fs::write(path_of(&item), bytes).unwrap();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
        let error = write(&item, &noted()).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        write(&item, &Sidecar::default()).await.unwrap();
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), bytes);
    }

    #[tokio::test]
    async fn a_file_that_is_not_appledouble_is_not_valid() {
        assert_not_valid(&[b'x'; 200]).await;
    }

    #[tokio::test]
    async fn a_file_with_more_than_fifteen_entries_is_not_valid() {
        let mut file = two_entries(b"");
        file[24..26].copy_from_slice(&16u16.to_be_bytes());
        assert_not_valid(&file).await;
        file[24..26].copy_from_slice(&0u16.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn entries_that_overlap_are_not_valid() {
        let mut file = two_entries(b"0123456789");
        file[42..46].copy_from_slice(&80u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn an_entry_past_the_end_of_the_file_is_not_valid() {
        let mut file = two_entries(b"0123456789");
        file[46..50].copy_from_slice(&11u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn an_entry_in_the_table_is_not_valid() {
        let mut file = two_entries(b"");
        file[30..34].copy_from_slice(&49u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_sidecar_gets_the_mode_and_owner_of_its_item() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, item) = item();
        std::fs::set_permissions(&item, std::fs::Permissions::from_mode(0o640)).unwrap();
        write(&item, &noted()).await.unwrap();
        let sidecar = std::fs::metadata(path_of(&item)).unwrap();
        let item = std::fs::metadata(&item).unwrap();
        assert_eq!(sidecar.permissions().mode() & 0o777, 0o640);
        assert_eq!((sidecar.uid(), sidecar.gid()), (item.uid(), item.gid()));
    }

    #[tokio::test]
    async fn a_hard_linked_empty_sidecar_is_not_used() {
        let (dir, item) = item();
        std::fs::write(path_of(&item), b"").unwrap();
        std::fs::hard_link(path_of(&item), dir.path().join("other")).unwrap();
        let error = write(&item, &noted()).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), b"");
    }

    #[test]
    fn path_of_prefixes_the_name() {
        assert_eq!(path_of(Path::new("dir/a.txt")), Path::new("dir/._a.txt"));
    }
}
