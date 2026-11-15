//! AppleDouble sidecar files (RFC 1740), as macOS keeps them on a volume
//! without extended attributes. The sidecar of `dir/name` is
//! `dir/._name`. It holds the item's Finder info, its resource fork, and
//! its extended attributes. The comment is the extended attribute
//! [`COMMENT_NAME`], a property list with one string.
//!
//! Each change does what xnu's `bsd/vfs/vfs_xattr.c` does for the same
//! `setxattr` or `removexattr` call (xnu-11417.101.15). The extended
//! attributes are in the Finder info entry, after an [`AttrHeader`]. They
//! are read only from a sidecar with the Finder info entry first, at byte
//! 50. A read finds nothing in a sidecar that fails a check, and a change
//! to it returns an error.
//!
//! Two changes differ from xnu. A change that would write over another
//! entry fails, where xnu writes over it. A Finder info change is written
//! wherever the entry is, where xnu writes only the first 120 bytes.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::Metadata;
use std::future::Future;
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, PoisonError};
use std::task::{Context, Poll, ready};

use hotline_proto::wire::{ArrayWriter, VecWriter};
use hotline_server::{BoxedReader, FinderInfo};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, MutexGuard};

use crate::bplist;

/// The start of a sidecar's name.
pub const PREFIX: &str = "._";

/// The extended attribute that holds the comment.
const COMMENT_NAME: &str = "com.apple.metadata:kMDItemFinderComment";

const MAGIC: u32 = 0x0005_1607;
const VERSION: u32 = 0x0002_0000;
/// The filler of a new sidecar.
const FILLER: &[u8; 16] = b"Mac OS X        ";
const RESOURCE_FORK_ID: u32 = 2;
const FINDER_INFO_ID: u32 = 9;

/// The most entries that a sidecar can hold.
const MAX_ENTRIES: u16 = 15;

/// The fixed start of a sidecar: magic, version, filler, and entry count.
const HEADER_LEN: usize = 26;
/// The length of one entry descriptor.
const ENTRY_LEN: usize = 12;
/// The length of the Finder info.
const FINDER_INFO_LEN: u32 = 32;
/// Where the Finder info entry starts in a sidecar with two entries.
const FINDER_INFO_AT: u32 = 50;
/// The end of the header of a sidecar with two entries, the Finder info,
/// and 2 bytes of pad.
const AD_HEADER_END: usize = 84;
/// The end of the [`AttrHeader`] after [`AD_HEADER_END`].
const ATTR_HEADER_END: u32 = 120;
/// The magic number of an [`AttrHeader`], `ATTR`.
const ATTR_MAGIC: u32 = 0x4154_5452;
/// The length of a new sidecar, and the step by which the attributes grow.
const ATTR_BUF_SIZE: u32 = 4096;
/// The most bytes that a read of the header takes. The attribute entries
/// end inside them.
const ATTR_MAX_HDR_SIZE: u32 = 65536;
/// The most extended attributes that a sidecar can hold.
const MAX_ATTRS: u16 = 256;
/// The longest attribute value.
const ATTR_MAX_SIZE: usize = i32::MAX as usize;

/// The length of an empty resource fork.
const EMPTY_FORK_LEN: u32 = 286;
/// The system data of an empty resource fork from a new sidecar.
const EMPTY_FORK_TAG: &[u8; 47] = b"This resource fork intentionally left blank   \0";

/// One entry descriptor: what the entry is, and where it is in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Entry {
    id: u32,
    offset: u32,
    len: u32,
}

impl Entry {
    /// The file offset of the end of the entry. `None` if it is past 4 GiB.
    const fn end(&self) -> Option<u32> {
        self.offset.checked_add(self.len)
    }

    fn encode(&self) -> [u8; ENTRY_LEN] {
        ArrayWriter::new()
            .u32(self.id)
            .u32(self.offset)
            .u32(self.len)
            .finish()
    }
}

/// The header of the extended attributes, after the Finder info and 2
/// bytes of pad. Its offsets count from the start of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AttrHeader {
    debug_tag: u32,
    /// The end of the space for the attributes.
    total_size: u32,
    data_start: u32,
    data_length: u32,
    flags: u16,
    num_attrs: u16,
}

impl AttrHeader {
    const LEN: usize = 36;

    fn encode(&self) -> [u8; Self::LEN] {
        ArrayWriter::new()
            .u32(ATTR_MAGIC)
            .u32(self.debug_tag)
            .u32(self.total_size)
            .u32(self.data_start)
            .u32(self.data_length)
            .bytes(&[0; 12])
            .u16(self.flags)
            .u16(self.num_attrs)
            .finish()
    }

    /// The header at [`AD_HEADER_END`] in `buf`. `None` without
    /// [`ATTR_MAGIC`].
    fn decode(buf: &[u8]) -> Option<Self> {
        (u32_at(buf, AD_HEADER_END)? == ATTR_MAGIC).then_some(())?;
        Some(Self {
            debug_tag: u32_at(buf, AD_HEADER_END + 4)?,
            total_size: u32_at(buf, AD_HEADER_END + 8)?,
            data_start: u32_at(buf, AD_HEADER_END + 12)?,
            data_length: u32_at(buf, AD_HEADER_END + 16)?,
            flags: u16_at(buf, AD_HEADER_END + 32)?,
            num_attrs: u16_at(buf, AD_HEADER_END + 34)?,
        })
    }

    /// The file offset of the end of the attribute data.
    const fn data_end(&self) -> u32 {
        self.data_start.saturating_add(self.data_length)
    }
}

/// The length of an attribute entry with a name of `name_len` bytes and
/// its NUL, padded to 4 bytes.
const fn attr_entry_len(name_len: u8) -> u32 {
    (name_len as u32).saturating_add(11).next_multiple_of(4)
}

/// One attribute entry: the place of its data and its name.
struct AttrEntry {
    /// Where the entry is in the file.
    at: usize,
    offset: u32,
    len: u32,
    name_len: u8,
}

/// What [`get_info`] finds in a sidecar.
struct Info {
    file: File,
    file_len: u64,
    /// The first bytes of the file, at most [`ATTR_MAX_HDR_SIZE`]. When
    /// the sidecar changes, zero bytes follow up to that size.
    buf: Vec<u8>,
    /// How many bytes of `buf` came from the file.
    raw_len: usize,
    entries: Vec<Entry>,
    /// The index of the Finder info entry.
    finder_info: Option<usize>,
    /// The Finder info is all zero.
    empty_finder_info: bool,
    /// The index of the resource fork entry.
    fork: Option<usize>,
    attrs: Option<AttrHeader>,
}

impl Info {
    /// `true` if the attribute entry at `at` is inside the bytes read.
    fn attr_valid(&self, at: usize) -> bool {
        let name_len_at = at.saturating_add(10);
        name_len_at < self.raw_len
            && self.buf.get(name_len_at).is_some_and(|&name_len| {
                at.saturating_add(attr_entry_len(name_len) as usize) <= self.raw_len
            })
    }

    /// The attribute entry at `at`.
    fn attr_entry(&self, at: usize) -> io::Result<AttrEntry> {
        Ok(AttrEntry {
            at,
            offset: u32_at(&self.buf, at).ok_or_else(out_of_range)?,
            len: u32_at(&self.buf, at.saturating_add(4)).ok_or_else(out_of_range)?,
            name_len: *self
                .buf
                .get(at.saturating_add(10))
                .ok_or_else(out_of_range)?,
        })
    }

    /// `true` if the attribute entry at `at` has the name `name`, which
    /// ends with its NUL.
    fn has_name(&self, at: usize, name: &[u8]) -> bool {
        let start = at.saturating_add(11);
        self.buf.get(start..start.saturating_add(name.len())) == Some(name)
    }

    /// Find the attribute `name`, which ends with its NUL. The first value
    /// is its entry, and the second is `true` if it is the last one. With
    /// no such attribute, the entry is `None`, and the third value is the
    /// place after the entries that are inside the bytes read.
    fn find_attr(&self, name: &[u8]) -> io::Result<(Option<AttrEntry>, bool, usize)> {
        let count = self.attrs.map_or(0, |attrs| attrs.num_attrs);
        let mut at = ATTR_HEADER_END as usize;
        for i in 0..count {
            if !self.attr_valid(at) {
                break;
            }
            let entry = self.attr_entry(at)?;
            if self.has_name(at, name) {
                return Ok((Some(entry), i.saturating_add(1) == count, at));
            }
            at = at.saturating_add(attr_entry_len(entry.name_len) as usize);
        }
        Ok((None, false, at))
    }

    /// The value of the attribute `entry`.
    async fn attr_value(&mut self, entry: &AttrEntry) -> io::Result<Vec<u8>> {
        let start = entry.offset as usize;
        let end = start.saturating_add(entry.len as usize);
        if end < ATTR_MAX_HDR_SIZE as usize {
            return Ok(self.buf.get(start..end).ok_or_else(out_of_range)?.to_vec());
        }
        read_at(
            &mut self.file,
            u64::from(entry.offset),
            u64::from(entry.len),
        )
        .await
    }

    /// The Finder info, or `None` if there is none or it is all zero.
    fn finder_info_bytes(&self) -> Option<[u8; FINDER_INFO_LEN as usize]> {
        let entry = self.entries.get(self.finder_info?)?;
        let start = entry.offset as usize;
        let bytes = self
            .buf
            .get(start..start.saturating_add(FINDER_INFO_LEN as usize))?;
        (!self.empty_finder_info).then(|| bytes.try_into().ok())?
    }

    /// The number of attributes, counting the Finder info and the fork.
    fn attr_count(&self) -> u32 {
        u32::from(self.attrs.map_or(0, |attrs| attrs.num_attrs))
            .saturating_add(u32::from(self.fork.is_some()))
            .saturating_add(u32::from(
                self.finder_info.is_some() && !self.empty_finder_info,
            ))
    }

    /// `true` if no other entry starts at or after a resource fork entry,
    /// so a longer fork overwrites nothing.
    fn fork_is_last(&self) -> bool {
        let starts_after = |(i, fork): (usize, &Entry)| {
            self.entries
                .iter()
                .enumerate()
                .any(|(j, entry)| j != i && entry.offset >= fork.offset)
        };
        !self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.id == RESOURCE_FORK_ID)
            .any(starts_after)
    }

    /// The entry at `index`.
    fn entry_mut(&mut self, index: usize) -> io::Result<&mut Entry> {
        self.entries.get_mut(index).ok_or_else(out_of_range)
    }

    /// Write the entry table, the [`AttrHeader`], and the rest of the
    /// first `len` bytes of `buf` to the file.
    async fn write(&mut self, len: usize) -> io::Result<()> {
        let table: Vec<u8> = self.entries.iter().flat_map(Entry::encode).collect();
        put(&mut self.buf, HEADER_LEN, &table)?;
        if let Some(attrs) = self.attrs {
            put(&mut self.buf, AD_HEADER_END, &attrs.encode())?;
        }
        let bytes = self.buf.get(..len).ok_or_else(out_of_range)?;
        self.file.seek(SeekFrom::Start(0)).await?;
        self.file.write_all(bytes).await?;
        self.file.flush().await?;
        self.file_len = self.file_len.max(len as u64);
        Ok(())
    }
}

/// What a read of a sidecar finds.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Sidecar {
    finder_info: [u8; FINDER_INFO_LEN as usize],
    /// The comment.
    pub comment: Option<String>,
    resource_fork: Span,
}

impl Sidecar {
    /// The type, creator, and flags in the Finder info.
    pub fn finder_info(&self) -> FinderInfo {
        finder_info_of(&self.finder_info)
    }

    /// The byte length of the resource fork.
    pub const fn resource_fork_len(&self) -> u64 {
        self.resource_fork.len
    }
}

/// Where an entry's bytes are in the file.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Span {
    offset: u64,
    len: u64,
}

/// The type, creator, and flags at the start of the Finder info `bytes`.
fn finder_info_of(bytes: &[u8; FINDER_INFO_LEN as usize]) -> FinderInfo {
    let (file_type, rest) = bytes.split_first_chunk().unwrap_or((&[0; 4], &[]));
    let (creator, rest) = rest.split_first_chunk().unwrap_or((&[0; 4], &[]));
    let flags = rest
        .first_chunk()
        .map_or(0, |&flags| u16::from_be_bytes(flags));
    FinderInfo {
        file_type: *file_type,
        creator: *creator,
        flags,
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
    let Some(file) = open_sidecar(item, Access::Read).await? else {
        return Ok(Sidecar::default());
    };
    let Some(mut info) = get_info(file, false).await? else {
        tracing::debug!(item = %item.display(), "a sidecar is not a valid AppleDouble file");
        return Ok(Sidecar::default());
    };
    let resource_fork = info
        .fork
        .and_then(|index| info.entries.get(index))
        .map_or_else(Span::default, |entry| Span {
            offset: u64::from(entry.offset),
            len: u64::from(entry.len),
        });
    let comment = match info.find_attr(&nul_terminated(COMMENT_NAME))?.0 {
        Some(entry) => bplist::decode_string(&info.attr_value(&entry).await?),
        None => None,
    };
    Ok(Sidecar {
        finder_info: info.finder_info_bytes().unwrap_or_default(),
        comment,
        resource_fork,
    })
}

/// Set the type, creator, and flags in the Finder info of `item`, and keep
/// the rest of its Finder info. A Finder info of all zero removes it.
pub async fn set_finder_info(item: &Path, info: FinderInfo) -> io::Result<()> {
    let mut bytes = read(item).await?.finder_info;
    let fields = [
        info.file_type.as_slice(),
        &info.creator,
        &info.flags.to_be_bytes(),
    ]
    .concat();
    put(&mut bytes, 0, &fields)?;
    if bytes == [0; FINDER_INFO_LEN as usize] {
        return ignore_no_attr(remove_attr(item, AttrName::FinderInfo).await);
    }
    let file = open_sidecar(item, Access::Create)
        .await?
        .ok_or_else(no_attr)?;
    let mut info = get_info(file, true).await?.ok_or_else(no_attr)?;
    let index = info.finder_info.ok_or_else(no_attr)?;
    let offset = info.entries.get(index).ok_or_else(out_of_range)?.offset as usize;
    put(&mut info.buf, offset, &bytes)?;
    let end = offset.saturating_add(FINDER_INFO_LEN as usize);
    info.write(end.max(ATTR_HEADER_END as usize)).await?;
    touch(item).await;
    Ok(())
}

/// Set the comment of `item`. An empty comment removes it.
pub async fn set_comment(item: &Path, comment: &str) -> io::Result<()> {
    if comment.is_empty() {
        return ignore_no_attr(remove_attr(item, AttrName::Other(COMMENT_NAME)).await);
    }
    set_attr(item, COMMENT_NAME, &bplist::encode_string(comment)).await
}

/// Open the resource fork of `item` for reading from `at` onward. Empty if
/// there is no fork.
pub async fn open_resource_read(item: &Path, at: u64) -> io::Result<BoxedReader> {
    let Span { offset, len } = read(item).await?.resource_fork;
    if at >= len {
        return Ok(Box::new(tokio::io::empty()));
    }
    let mut file = File::open(path_of(item)).await?;
    file.seek(SeekFrom::Start(offset.saturating_add(at)))
        .await?;
    Ok(Box::new(file.take(len.saturating_sub(at))))
}

/// Open the resource fork of `item` for writing from `at` onward. At 0 the
/// old fork goes first, and the sidecar is created if missing. A fork
/// longer than `at` is cut to `at`. `PermissionDenied` if another entry
/// starts after the fork. Call with the [`Sidecars`] lock held. The writer
/// keeps `claim` until it drops.
pub async fn open_resource_write(
    item: &Path,
    at: u64,
    claim: ForkClaim,
) -> io::Result<ResourceForkWriter> {
    if !tokio::fs::metadata(item).await?.is_file() || !fork_is_last(item).await? {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if at == 0 {
        ignore_no_attr(remove_attr(item, AttrName::ResourceFork).await)?;
    }
    let file = open_sidecar(item, Access::Create)
        .await?
        .ok_or_else(no_attr)?;
    let mut info = get_info(file, true).await?.ok_or_else(no_attr)?;
    let index = info.fork.ok_or_else(no_attr)?;
    let entry = *info.entries.get(index).ok_or_else(out_of_range)?;
    let fork_at = u64::from(entry.offset)
        .checked_add(at)
        .ok_or_else(out_of_range)?;
    if at < u64::from(entry.len) {
        if entry.end().map(u64::from) == Some(info.file_len) {
            info.file.set_len(fork_at).await?;
        }
        info.entry_mut(index)?.len = wire_u32(at)?;
        info.write(ATTR_HEADER_END as usize).await?;
    }
    let mut file = info.file;
    file.seek(SeekFrom::Start(fork_at)).await?;
    Ok(ResourceForkWriter {
        file: Some(file),
        end: at,
        entry: Entry {
            len: entry.len.min(wire_u32(at)?),
            ..entry
        },
        entry_at: HEADER_LEN.saturating_add(index.saturating_mul(ENTRY_LEN)) as u64,
        item: item.to_path_buf(),
        closing: None,
        claim,
    })
}

/// `true` if the sidecar of `item` is missing, fails a check, or has no
/// entry after its resource fork.
async fn fork_is_last(item: &Path) -> io::Result<bool> {
    let Some(file) = open_sidecar(item, Access::Read).await? else {
        return Ok(true);
    };
    Ok(get_info(file, false)
        .await?
        .is_none_or(|info| info.fork_is_last()))
}

/// Writes a resource fork from [`open_resource_write`]. Shutdown writes a
/// longer length into the fork's entry, with the [`Sidecars`] lock held,
/// and then [`touch`]es the item.
pub struct ResourceForkWriter {
    file: Option<File>,
    /// The end of the bytes written, from the start of the fork.
    end: u64,
    /// The fork's entry, as it was when the writer opened.
    entry: Entry,
    /// The file offset of the fork's entry.
    entry_at: u64,
    /// The path of the item, to [`touch`] at shutdown.
    item: PathBuf,
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
        self.end = self.end.saturating_add(written as u64);
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
            let end = wire_u32(self.end)?;
            let entry = (end > self.entry.len).then_some(Entry {
                len: end,
                ..self.entry
            });
            let entry_at = self.entry_at;
            let item = self.item.clone();
            let sidecars = Arc::clone(&self.claim.sidecars);
            Box::pin(async move {
                let _sidecars = sidecars.lock().await;
                if let Some(entry) = entry {
                    file.seek(SeekFrom::Start(entry_at)).await?;
                    file.write_all(&entry.encode()).await?;
                }
                file.shutdown().await?;
                touch(&item).await;
                Ok(())
            })
        };
        self.closing.insert(closing).as_mut().poll(cx)
    }
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
    Write,
    /// Open it for writing, create it if missing, and give an empty file
    /// the layout of a new sidecar.
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
    let mut file = match (access, &existing) {
        (Access::Create, _) => {
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&path)
                .await?
        }
        (_, Some(_)) => {
            OpenOptions::new()
                .read(true)
                .write(access == Access::Write)
                .open(&path)
                .await?
        }
        (_, None) => return Ok(None),
    };
    if existing.is_none()
        && let Some(item_metadata) = &item_metadata
    {
        set_new_owner(&file, item_metadata).await;
    }
    let metadata = file.metadata().await?;
    let owner = item_metadata.as_ref().and_then(owner_of);
    if owner.is_some() && owner != owner_of(&metadata) {
        if existing.is_none() {
            remove(&path).await?;
        }
        return Ok(None);
    }
    if access == Access::Create && metadata.len() == 0 {
        if link_count(&metadata) > 1 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        file.write_all(&new_sidecar(file_id(&metadata))).await?;
        file.flush().await?;
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

/// Write the modify time of `item` back to it, which sets its change time.
async fn touch(item: &Path) {
    let item = item.to_path_buf();
    let touched = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&item)?;
        let modified = file.metadata()?.modified()?;
        file.set_modified(modified)
    })
    .await;
    if let Ok(Err(e)) = touched {
        tracing::debug!(error = %e, "the change time of an item did not change");
    }
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

/// The low 32 bits of a file's inode number.
#[cfg(unix)]
fn file_id(metadata: &Metadata) -> u32 {
    std::os::unix::fs::MetadataExt::ino(metadata) as u32
}

/// The low 32 bits of a file's inode number.
#[cfg(not(unix))]
const fn file_id(_metadata: &Metadata) -> u32 {
    0
}

/// A new sidecar of [`ATTR_BUF_SIZE`] bytes: a Finder info entry at 50 with
/// an [`AttrHeader`] and no attributes, then an empty resource fork.
fn new_sidecar(debug_tag: u32) -> [u8; ATTR_BUF_SIZE as usize] {
    let fork_at = ATTR_BUF_SIZE - EMPTY_FORK_LEN;
    let finder_info = Entry {
        id: FINDER_INFO_ID,
        offset: FINDER_INFO_AT,
        len: fork_at - FINDER_INFO_AT,
    };
    let fork = Entry {
        id: RESOURCE_FORK_ID,
        offset: fork_at,
        len: EMPTY_FORK_LEN,
    };
    let attrs = AttrHeader {
        debug_tag,
        total_size: fork_at,
        data_start: ATTR_HEADER_END,
        data_length: 0,
        flags: 0,
        num_attrs: 0,
    };
    ArrayWriter::new()
        .u32(MAGIC)
        .u32(VERSION)
        .bytes(FILLER)
        .u16(2)
        .bytes(&finder_info.encode())
        .bytes(&fork.encode())
        .pad_to(AD_HEADER_END)
        .bytes(&attrs.encode())
        .pad_to(fork_at as usize)
        .bytes(&empty_fork())
        .finish()
}

/// An empty resource fork: a header and a map with no resources.
fn empty_fork() -> [u8; EMPTY_FORK_LEN as usize] {
    const FIRST_RESOURCE: u32 = 256;
    const NULL_MAP_LEN: u16 = 30;
    let mut system_data = [0; 112];
    put(&mut system_data, 0, EMPTY_FORK_TAG).unwrap_or_default();
    ArrayWriter::new()
        .u32(FIRST_RESOURCE)
        .u32(FIRST_RESOURCE)
        .u32(0)
        .u32(u32::from(NULL_MAP_LEN))
        .bytes(&system_data)
        .bytes(&[0; 128])
        .u32(FIRST_RESOURCE)
        .u32(FIRST_RESOURCE)
        .u32(0)
        .u32(u32::from(NULL_MAP_LEN))
        .u32(0)
        .u16(0)
        .u8(0)
        .u8(0)
        .u16(NULL_MAP_LEN - 2)
        .u16(NULL_MAP_LEN)
        .u16(0xFFFF)
        .finish()
}

/// Read and check the sidecar in `file`. `None` if it fails a check. When
/// `setting`, the result can be changed, and a sidecar with the Finder
/// info at 50 and the fork second gets room for the attributes first.
async fn get_info(mut file: File, setting: bool) -> io::Result<Option<Info>> {
    let metadata = file.metadata().await?;
    let mut file_len = metadata.len();
    let read_len = if setting {
        ATTR_MAX_HDR_SIZE as usize
    } else {
        file_len.min(u64::from(ATTR_MAX_HDR_SIZE)) as usize
    };
    if read_len < AD_HEADER_END {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(read_len);
    file.seek(SeekFrom::Start(0)).await?;
    (&mut file)
        .take(read_len as u64)
        .read_to_end(&mut buf)
        .await?;
    let raw_len = buf.len();
    buf.resize(read_len, 0);
    let Some(mut entries) = check_header(&buf, raw_len, file_len) else {
        return Ok(None);
    };

    let mut finder_info = None;
    let mut empty_finder_info = false;
    let mut fork = None;
    for (index, entry) in entries.iter().enumerate() {
        if entry.id == FINDER_INFO_ID && entry.len >= FINDER_INFO_LEN {
            finder_info = Some(index);
            let start = entry.offset as usize;
            match entry.offset.checked_add(FINDER_INFO_LEN) {
                Some(end) if end as usize <= raw_len => {
                    let bytes = buf.get(start..end as usize).unwrap_or_default();
                    empty_finder_info |= bytes.iter().all(|&byte| byte == 0);
                }
                _ => return Ok(None),
            }
        }
        if entry.id == RESOURCE_FORK_ID {
            if !setting
                && (entry.len == 0
                    || entry.len == EMPTY_FORK_LEN && is_empty_fork(&mut file, entry).await)
            {
                continue;
            }
            fork = Some(index);
        }
    }

    if setting
        && entries.len() == 2
        && finder_info == Some(0)
        && fork == Some(1)
        && entries
            .first()
            .is_some_and(|entry| entry.offset == FINDER_INFO_AT && entry.len == FINDER_INFO_LEN)
    {
        let [first, second] = entries.as_mut_slice() else {
            return Ok(None);
        };
        let mut delta = ATTR_BUF_SIZE - (FINDER_INFO_AT + FINDER_INFO_LEN);
        let moved_offset = second.offset.checked_add(delta).ok_or_else(out_of_range)?;
        let write_len;
        if second.len != 0 {
            shift_down(
                &mut file,
                u64::from(second.offset),
                u64::from(second.len),
                u64::from(delta),
            )
            .await?;
            let fork_end = u64::from(moved_offset).saturating_add(u64::from(second.len));
            file_len = file_len.max(fork_end);
            write_len = ATTR_HEADER_END as usize;
        } else {
            if (moved_offset as usize).saturating_add(EMPTY_FORK_LEN as usize) > read_len {
                return Ok(None);
            }
            file_len = u64::from(moved_offset);
            file.set_len(file_len).await?;
            delta = delta.saturating_sub(EMPTY_FORK_LEN);
            let pad = (FINDER_INFO_AT + FINDER_INFO_LEN) as usize;
            buf.get_mut(pad..pad.saturating_add(delta as usize))
                .ok_or_else(out_of_range)?
                .fill(0);
            put(
                &mut buf,
                second.offset.saturating_add(delta) as usize,
                &empty_fork(),
            )?;
            second.len = EMPTY_FORK_LEN;
            write_len = ATTR_BUF_SIZE as usize;
        }
        first.len = first.len.saturating_add(delta);
        second.offset = second.offset.saturating_add(delta);
        let attrs = AttrHeader {
            debug_tag: file_id(&metadata),
            total_size: second.offset,
            data_start: ATTR_HEADER_END,
            data_length: 0,
            flags: 0,
            num_attrs: 0,
        };
        let table: Vec<u8> = entries.iter().flat_map(Entry::encode).collect();
        put(&mut buf, HEADER_LEN, &table)?;
        put(&mut buf, AD_HEADER_END, &attrs.encode())?;
        file.seek(SeekFrom::Start(0)).await?;
        file.write_all(buf.get(..write_len).ok_or_else(out_of_range)?)
            .await?;
        file.flush().await?;
        file_len = file_len.max(write_len as u64);
    }

    let mut attrs = None;
    if let Some(first) = entries
        .first()
        .filter(|first| finder_info == Some(0) && first.len as usize >= AttrHeader::LEN)
    {
        if first.offset != FINDER_INFO_AT {
            return Ok(None);
        }
        attrs = check_attrs(&buf, raw_len, first);
    }
    Ok(Some(Info {
        file,
        file_len,
        buf,
        raw_len,
        entries,
        finder_info,
        empty_finder_info,
        fork,
        attrs,
    }))
}

/// The entry table at the start of `buf`, of which `raw_len` bytes came
/// from a file of `file_len` bytes. `None` if the header is not valid, if
/// there are no entries or more than [`MAX_ENTRIES`], or if an entry
/// starts in the table, ends past the end of the file, or overlaps an
/// entry before it.
fn check_header(buf: &[u8], raw_len: usize, file_len: u64) -> Option<Vec<Entry>> {
    if raw_len < HEADER_LEN || u32_at(buf, 0)? != MAGIC || u32_at(buf, 4)? != VERSION {
        return None;
    }
    let count = u16_at(buf, 24)?;
    if !(1..=MAX_ENTRIES).contains(&count) {
        return None;
    }
    let table_end = HEADER_LEN.saturating_add(usize::from(count).saturating_mul(ENTRY_LEN));
    if raw_len < table_end {
        return None;
    }
    let entries: Vec<Entry> = (0..usize::from(count))
        .map(|i| {
            let at = HEADER_LEN.saturating_add(i.saturating_mul(ENTRY_LEN));
            Some(Entry {
                id: u32_at(buf, at)?,
                offset: u32_at(buf, at.saturating_add(4))?,
                len: u32_at(buf, at.saturating_add(8))?,
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

/// `true` if the fork of `entry` is the empty fork of a new sidecar, or
/// cannot be read.
async fn is_empty_fork(file: &mut File, entry: &Entry) -> bool {
    let mut system_data = [0; 64];
    let read = async {
        file.seek(SeekFrom::Start(u64::from(entry.offset) + 16))
            .await?;
        let mut filled = 0;
        while let Some(rest) = system_data
            .get_mut(filled..)
            .filter(|rest| !rest.is_empty())
        {
            match file.read(rest).await? {
                0 => break,
                n => filled = filled.saturating_add(n),
            }
        }
        io::Result::Ok(())
    };
    read.await.is_err() || system_data.starts_with(EMPTY_FORK_TAG)
}

/// The [`AttrHeader`] in `buf`, of which `raw_len` bytes came from the
/// file, for the Finder info `entry` at 50. `None` if the header or an
/// attribute entry is not valid.
fn check_attrs(buf: &[u8], raw_len: usize, entry: &Entry) -> Option<AttrHeader> {
    let header = AttrHeader::decode(buf)?;
    let data_end = header.data_start.checked_add(header.data_length)?;
    if header.total_size > entry.end()?
        || header.data_start < ATTR_HEADER_END
        || data_end > header.total_size
        || header.data_start as usize > raw_len
        || header.num_attrs > MAX_ATTRS
    {
        return None;
    }
    let buf_end = header.data_start as usize;
    let mut at = ATTR_HEADER_END as usize;
    let mut header_size = ATTR_HEADER_END;
    let mut data_size = 0u32;
    for _ in 0..header.num_attrs {
        let name_len = *buf.get(at.saturating_add(10))?;
        let name_at = at.saturating_add(11);
        let name_end = name_at.saturating_add(usize::from(name_len));
        if at.saturating_add(12) > buf_end || name_end > buf_end {
            return None;
        }
        let name = buf.get(name_at..name_end)?;
        if name.iter().position(|&byte| byte == 0) != Some(usize::from(name_len).checked_sub(1)?) {
            return None;
        }
        let offset = u32_at(buf, at)?;
        let len = u32_at(buf, at.saturating_add(4))?;
        let end = offset.checked_add(len)?;
        if end > header.total_size || offset < header.data_start || end > data_end {
            return None;
        }
        header_size = header_size.checked_add(attr_entry_len(name_len))?;
        data_size = data_size.checked_add(len)?;
        at = at.saturating_add(attr_entry_len(name_len) as usize);
    }
    (header.data_start >= header_size && data_size == header.data_length).then_some(header)
}

/// An attribute that [`remove_attr`] removes.
#[derive(Debug, Clone, Copy)]
enum AttrName<'a> {
    FinderInfo,
    ResourceFork,
    Other(&'a str),
}

/// Remove the attribute `name` of `item`, and then [`touch`] `item`.
/// Removing the last attribute removes the sidecar. An error of kind
/// `InvalidData` if there is no such attribute. A sidecar with no
/// attributes is removed, and the call fails.
async fn remove_attr(item: &Path, name: AttrName<'_>) -> io::Result<()> {
    remove_attr_from_sidecar(item, name).await?;
    touch(item).await;
    Ok(())
}

/// Remove the attribute `name` of `item`, as [`remove_attr`] does, but
/// leave the times of `item`.
async fn remove_attr_from_sidecar(item: &Path, name: AttrName<'_>) -> io::Result<()> {
    let file = open_sidecar(item, Access::Write)
        .await?
        .ok_or_else(no_attr)?;
    let mut info = get_info(file, false).await?.ok_or_else(no_attr)?;
    if info.attr_count() == 0 {
        remove(&path_of(item)).await?;
        if matches!(name, AttrName::ResourceFork) && !tokio::fs::metadata(item).await?.is_file() {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        return Err(no_attr());
    }
    let last = info.attr_count() == 1;
    match name {
        AttrName::FinderInfo => {
            let index = info.finder_info.filter(|_| !info.empty_finder_info);
            let index = index.ok_or_else(no_attr)?;
            if last {
                return remove(&path_of(item)).await;
            }
            let offset = info.entries.get(index).ok_or_else(out_of_range)?.offset as usize;
            put(&mut info.buf, offset, &[0; FINDER_INFO_LEN as usize])?;
            let len = info.buf.len();
            info.write(len).await
        }
        AttrName::ResourceFork => {
            if !tokio::fs::metadata(item).await?.is_file() {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let index = info.fork.ok_or_else(no_attr)?;
            let entry = *info.entries.get(index).ok_or_else(out_of_range)?;
            if entry.len == 0 {
                return Err(no_attr());
            }
            if last {
                return remove(&path_of(item)).await;
            }
            if entry.end().map(u64::from) == Some(info.file_len) {
                info.file_len = u64::from(entry.offset);
                info.file.set_len(info.file_len).await?;
            }
            info.entry_mut(index)?.len = 0;
            info.write(ATTR_HEADER_END as usize).await
        }
        AttrName::Other(name) => {
            let header = info.attrs.ok_or_else(no_attr)?;
            let name = nul_terminated(name);
            let (Some(entry), is_last, _) = info.find_attr(&name)? else {
                return Err(no_attr());
            };
            if last {
                return remove(&path_of(item)).await;
            }
            remove_attr_entry(&mut info, header, &entry, name.len(), is_last).await
        }
    }
}

/// Remove the attribute `entry`, with a name of `name_len` bytes and its
/// NUL, from `info`. `is_last` is `true` for the last entry.
async fn remove_attr_entry(
    info: &mut Info,
    mut header: AttrHeader,
    entry: &AttrEntry,
    name_len: usize,
    is_last: bool,
) -> io::Result<()> {
    let data_start = header.data_start as usize;
    let data_end = header.data_end() as usize;
    let data_at = entry.offset as usize;
    let data_len = entry.len as usize;
    let entry_len = attr_entry_len(u8::try_from(name_len).map_err(|_| out_of_range())?) as usize;
    let split = data_end > ATTR_MAX_HDR_SIZE as usize;
    if !is_last {
        copy_within(
            &mut info.buf,
            entry.at.saturating_add(entry_len)..data_start,
            entry.at,
        )?;
    }
    let write_len = if split {
        let delta = entry_len as u64;
        shift_up(
            &mut info.file,
            data_start as u64,
            data_at.saturating_sub(data_start) as u64,
            delta,
        )
        .await?;
        if !is_last {
            let after = data_at.saturating_add(data_len) as u64;
            shift_up(
                &mut info.file,
                after,
                (data_end as u64).saturating_sub(after),
                data_len.saturating_add(entry_len) as u64,
            )
            .await?;
        }
        data_start.saturating_sub(entry_len)
    } else {
        copy_within(
            &mut info.buf,
            data_start..data_at,
            data_start.saturating_sub(entry_len),
        )?;
        if !is_last {
            copy_within(
                &mut info.buf,
                data_at.saturating_add(data_len)..data_end,
                data_at.saturating_sub(entry_len),
            )?;
        }
        let freed = data_end.saturating_sub(data_len.saturating_add(entry_len));
        info.buf
            .get_mut(freed..data_end)
            .ok_or_else(out_of_range)?
            .fill(0);
        data_end
    };
    header.num_attrs = header.num_attrs.saturating_sub(1);
    header.data_start = header.data_start.saturating_sub(entry_len as u32);
    header.data_length = header.data_length.saturating_sub(entry.len);
    info.attrs = Some(header);
    let mut at = ATTR_HEADER_END as usize;
    for _ in 0..header.num_attrs {
        if !info.attr_valid(at) {
            break;
        }
        let moved = info.attr_entry(at)?;
        let mut offset = moved.offset.saturating_sub(entry_len as u32);
        if at >= entry.at {
            offset = offset.saturating_sub(entry.len);
        }
        put(&mut info.buf, at, &offset.to_be_bytes())?;
        at = at.saturating_add(attr_entry_len(moved.name_len) as usize);
    }
    info.write(write_len).await
}

/// Set the attribute `name` of `item` to `value`, and then [`touch`]
/// `item`. Create the sidecar if missing. A value of a new length removes
/// the attribute and adds it again at the end. `PermissionDenied` if the
/// attributes must grow over an entry that is not the resource fork.
async fn set_attr(item: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    set_attr_in_sidecar(item, name, value).await?;
    touch(item).await;
    Ok(())
}

/// Set the attribute `name` of `item`, as [`set_attr`] does, but leave the
/// times of `item` to the caller.
async fn set_attr_in_sidecar(item: &Path, name: &str, value: &[u8]) -> io::Result<()> {
    if value.len() > ATTR_MAX_SIZE {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let name = nul_terminated(name);
    let name_len =
        u8::try_from(name.len()).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
    let entry_len = attr_entry_len(name_len);
    let value_len = wire_u32(value.len() as u64)?;
    loop {
        let file = open_sidecar(item, Access::Create)
            .await?
            .ok_or_else(no_attr)?;
        let mut info = get_info(file, true).await?.ok_or_else(no_attr)?;
        let mut header = info.attrs.ok_or_else(no_attr)?;
        let split = u64::from(header.data_end())
            .saturating_add(u64::from(entry_len))
            .saturating_add(u64::from(value_len))
            > u64::from(ATTR_MAX_HDR_SIZE);
        let (found, _, slot) = info.find_attr(&name)?;
        if let Some(entry) = found {
            if entry.len != value_len {
                drop(info);
                remove_attr(item, AttrName::Other(name_str(&name))).await?;
                continue;
            }
            if split {
                info.file
                    .seek(SeekFrom::Start(u64::from(entry.offset)))
                    .await?;
                info.file.write_all(value).await?;
                return info.file.flush().await;
            }
            put(&mut info.buf, entry.offset as usize, value)?;
            let len = header.data_end() as usize;
            return info.write(len).await;
        }
        if !info.attr_valid(slot) || header.data_start.saturating_add(entry_len) > ATTR_MAX_HDR_SIZE
        {
            return Err(io::ErrorKind::StorageFull.into());
        }
        let free = header.total_size.saturating_sub(header.data_end());
        let needed = value_len.checked_add(entry_len).ok_or_else(out_of_range)?;
        if needed > free {
            let moves_only_the_fork = info
                .entries
                .iter()
                .all(|entry| matches!(entry.id, FINDER_INFO_ID | RESOURCE_FORK_ID));
            if !moves_only_the_fork {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            let mut grow = needed.saturating_sub(free).next_multiple_of(ATTR_BUF_SIZE);
            if !split && header.total_size.saturating_add(grow) > ATTR_MAX_HDR_SIZE {
                grow = ATTR_MAX_HDR_SIZE
                    .checked_sub(header.total_size)
                    .ok_or_else(out_of_range)?;
            }
            info.file_len = info.file_len.saturating_add(u64::from(grow));
            info.file.set_len(info.file_len).await?;
            if let Some(index) = info.fork {
                let fork = info.entry_mut(index)?;
                let (offset, len) = (fork.offset, fork.len);
                fork.offset = offset.checked_add(grow).ok_or_else(out_of_range)?;
                if len != 0 {
                    shift_down(
                        &mut info.file,
                        u64::from(offset),
                        u64::from(len),
                        u64::from(grow),
                    )
                    .await?;
                }
            }
            let index = info.finder_info.ok_or_else(no_attr)?;
            let finder_info = info.entry_mut(index)?;
            finder_info.len = finder_info.len.checked_add(grow).ok_or_else(out_of_range)?;
            header.total_size = header.total_size.saturating_add(grow);
        }

        let data_start = header.data_start as usize;
        let data_end = header.data_end() as usize;
        if split {
            shift_down(
                &mut info.file,
                data_start as u64,
                u64::from(header.data_length),
                u64::from(entry_len),
            )
            .await?;
        } else {
            copy_within(
                &mut info.buf,
                data_start..data_end,
                data_start.saturating_add(entry_len as usize),
            )?;
        }
        header.data_start = header.data_start.saturating_add(entry_len);
        let mut at = ATTR_HEADER_END as usize;
        while at != slot && info.attr_valid(at) {
            let moved = info.attr_entry(at)?;
            let offset = moved.offset.saturating_add(entry_len);
            put(&mut info.buf, at, &offset.to_be_bytes())?;
            at = at.saturating_add(attr_entry_len(moved.name_len) as usize);
        }
        let value_at = header.data_end();
        if split {
            info.file.seek(SeekFrom::Start(u64::from(value_at))).await?;
            info.file.write_all(value).await?;
            info.file.flush().await?;
        } else {
            put(&mut info.buf, value_at as usize, value)?;
        }
        let entry = VecWriter::new()
            .u32(value_at)
            .u32(value_len)
            .u16(0)
            .u8(name_len)
            .bytes(&name)
            .finish();
        put(&mut info.buf, slot, &entry)?;
        header.num_attrs = header.num_attrs.saturating_add(1);
        header.data_length = header.data_length.saturating_add(value_len);
        info.attrs = Some(header);
        let len = if split {
            header.data_start
        } else {
            header.data_end()
        };
        return info.write(len as usize).await;
    }
}

/// Move `len` bytes of `file` at `start` to `start + delta`.
async fn shift_down(file: &mut File, start: u64, len: u64, delta: u64) -> io::Result<()> {
    if delta == 0 || len == 0 {
        return Ok(());
    }
    let mut end = start.saturating_add(len);
    while end > start {
        let chunk = end.saturating_sub(start).min(u64::from(ATTR_BUF_SIZE));
        end = end.saturating_sub(chunk);
        let bytes = read_at(file, end, chunk).await?;
        file.seek(SeekFrom::Start(end.saturating_add(delta)))
            .await?;
        file.write_all(&bytes).await?;
        file.flush().await?;
    }
    Ok(())
}

/// Move `len` bytes of `file` at `start` to `start - delta`.
async fn shift_up(file: &mut File, start: u64, len: u64, delta: u64) -> io::Result<()> {
    if delta == 0 || len == 0 {
        return Ok(());
    }
    let end = start.saturating_add(len);
    let mut at = start;
    while at < end {
        let chunk = end.saturating_sub(at).min(u64::from(ATTR_BUF_SIZE));
        let bytes = read_at(file, at, chunk).await?;
        file.seek(SeekFrom::Start(at.saturating_sub(delta))).await?;
        file.write_all(&bytes).await?;
        file.flush().await?;
        at = at.saturating_add(chunk);
    }
    Ok(())
}

/// `name` and its NUL.
fn nul_terminated(name: &str) -> Vec<u8> {
    [name.as_bytes(), &[0]].concat()
}

/// `name` without its NUL.
fn name_str(name: &[u8]) -> &str {
    std::str::from_utf8(name.strip_suffix(&[0]).unwrap_or(name)).unwrap_or_default()
}

/// `Ok` for an error of [`no_attr`].
fn ignore_no_attr(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(e) if e.kind() == io::ErrorKind::InvalidData => Ok(()),
        result => result,
    }
}

/// The error for a sidecar without the attribute, or that fails a check.
fn no_attr() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "the sidecar has no such attribute, or is not valid",
    )
}

/// The error for an offset outside the bytes read.
fn out_of_range() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "an offset in the sidecar is out of range",
    )
}

/// `n` as a sidecar's 32-bit offset or length.
fn wire_u32(n: u64) -> io::Result<u32> {
    u32::try_from(n).map_err(|_| io::Error::other("the sidecar is too large"))
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

/// Copy `bytes` into `buf` at `at`.
fn put(buf: &mut [u8], at: usize, bytes: &[u8]) -> io::Result<()> {
    buf.get_mut(at..at.saturating_add(bytes.len()))
        .ok_or_else(out_of_range)?
        .copy_from_slice(bytes);
    Ok(())
}

/// Copy the bytes of `buf` in `src` to `dest`. The ranges can overlap.
fn copy_within(buf: &mut [u8], src: std::ops::Range<usize>, dest: usize) -> io::Result<()> {
    if src.start > src.end || src.end > buf.len() || dest.saturating_add(src.len()) > buf.len() {
        return Err(out_of_range());
    }
    buf.copy_within(src, dest);
    Ok(())
}

/// Read `len` bytes of `file` from `offset`.
async fn read_at(file: &mut File, offset: u64, len: u64) -> io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).await?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn be32(bytes: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    }

    /// A directory with the item `a.txt`, and the item's path.
    fn item() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let item = dir.path().join("a.txt");
        std::fs::write(&item, b"data").unwrap();
        (dir, item)
    }

    /// A sidecar as macOS writes it: the Finder info at 50 with `attrs`,
    /// and `fork` at 3810.
    fn macos_sidecar(attrs: &[(&str, &[u8])], fork: &[u8]) -> Vec<u8> {
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x02".to_vec();
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 50, 0, 0, 0x0E, 0xB0]);
        file.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0x0E, 0xE2]);
        file.extend_from_slice(&(fork.len() as u32).to_be_bytes());
        file.extend_from_slice(b"TEXTttxt");
        file.resize(84, 0);
        let names: Vec<Vec<u8>> = attrs.iter().map(|(name, _)| nul_terminated(name)).collect();
        let entries_len: usize = names
            .iter()
            .map(|name| (11 + name.len()).next_multiple_of(4))
            .sum();
        let data_start = 120 + entries_len;
        let data_len: usize = attrs.iter().map(|(_, value)| value.len()).sum();
        file.extend_from_slice(b"ATTR");
        file.extend_from_slice(&7u32.to_be_bytes());
        file.extend_from_slice(&3810u32.to_be_bytes());
        file.extend_from_slice(&(data_start as u32).to_be_bytes());
        file.extend_from_slice(&(data_len as u32).to_be_bytes());
        file.extend_from_slice(&[0; 12]);
        file.extend_from_slice(&[0, 0]);
        file.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
        let mut offset = data_start;
        for (name, (_, value)) in names.iter().zip(attrs) {
            file.extend_from_slice(&(offset as u32).to_be_bytes());
            file.extend_from_slice(&(value.len() as u32).to_be_bytes());
            file.extend_from_slice(&[0, 0, name.len() as u8]);
            file.extend_from_slice(name);
            file.resize(file.len().next_multiple_of(4), 0);
            offset += value.len();
        }
        assert_eq!(file.len(), data_start);
        for (_, value) in attrs {
            file.extend_from_slice(value);
        }
        file.resize(3810, 0);
        file.extend_from_slice(fork);
        file
    }

    /// The attributes of `file` as xnu reads them: two entries, the Finder
    /// info at 50, and the attribute header at 84.
    fn xnu_attrs(file: &[u8]) -> Vec<(String, Vec<u8>)> {
        assert_eq!(u16::from_be_bytes([file[24], file[25]]), 2);
        assert_eq!(be32(file, 26), FINDER_INFO_ID);
        assert_eq!(be32(file, 30), 50);
        assert_eq!(be32(file, 38), RESOURCE_FORK_ID);
        assert_eq!(be32(file, 84), ATTR_MAGIC);
        let total_size = be32(file, 92) as usize;
        let data_start = be32(file, 96) as usize;
        let data_end = data_start + be32(file, 100) as usize;
        assert!(total_size <= 50 + be32(file, 34) as usize);
        assert!(data_end <= total_size);
        assert_eq!(be32(file, 42), total_size as u32);
        let count = u16::from_be_bytes([file[118], file[119]]);
        let mut at = 120;
        let mut attrs = Vec::new();
        for _ in 0..count {
            let offset = be32(file, at) as usize;
            let len = be32(file, at + 4) as usize;
            let name_len = usize::from(file[at + 10]);
            assert!(offset >= data_start && offset + len <= data_end);
            let name = String::from_utf8(file[at + 11..at + 10 + name_len].to_vec()).unwrap();
            attrs.push((name, file[offset..offset + len].to_vec()));
            at += (11 + name_len).next_multiple_of(4);
        }
        assert!(at <= data_start);
        attrs
    }

    /// The resource fork of `file` by its entry table.
    fn fork_bytes(file: &[u8]) -> &[u8] {
        let offset = be32(file, 42) as usize;
        &file[offset..offset + be32(file, 46) as usize]
    }

    async fn fork_of(item: &Path) -> Vec<u8> {
        let mut fork = Vec::new();
        let mut reader = open_resource_read(item, 0).await.unwrap();
        reader.read_to_end(&mut fork).await.unwrap();
        fork
    }

    fn claim(item: &Path) -> ForkClaim {
        Arc::new(Sidecars::default()).claim(&path_of(item)).unwrap()
    }

    async fn write_fork(item: &Path, at: u64, bytes: &[u8]) {
        let mut writer = open_resource_write(item, at, claim(item)).await.unwrap();
        writer.write_all(bytes).await.unwrap();
        writer.shutdown().await.unwrap();
    }

    fn text_info() -> FinderInfo {
        FinderInfo {
            file_type: *b"TEXT",
            creator: *b"ttxt",
            flags: 0x0100,
        }
    }

    #[tokio::test]
    async fn read_of_a_missing_or_empty_file_is_empty() {
        let (_dir, item) = item();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
        std::fs::write(path_of(&item), b"").unwrap();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
    }

    #[tokio::test]
    async fn a_new_sidecar_has_the_layout_that_xnu_creates() {
        let (_dir, item) = item();
        set_finder_info(&item, text_info()).await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(file.len(), 4096);
        assert_eq!(&file[8..24], b"Mac OS X        ");
        assert_eq!(
            &file[26..50],
            &[
                0, 0, 0, 9, 0, 0, 0, 50, 0, 0, 0x0E, 0xB0, 0, 0, 0, 2, 0, 0, 0x0E, 0xE2, 0, 0, 1,
                0x1E
            ]
        );
        assert_eq!(&file[50..60], b"TEXTttxt\x01\x00");
        assert_eq!(be32(&file, 92), 3810);
        assert_eq!(be32(&file, 96), 120);
        assert!(xnu_attrs(&file).is_empty());
        assert_eq!(&file[3810 + 16..3810 + 16 + 47], EMPTY_FORK_TAG);
        assert_eq!(&file[4094..], &[0xFF, 0xFF]);

        let sidecar = read(&item).await.unwrap();
        assert_eq!(sidecar.finder_info(), text_info());
        assert_eq!(sidecar.resource_fork_len(), 0);
        assert_eq!(sidecar.comment, None);
    }

    #[tokio::test]
    async fn a_comment_is_an_attribute_with_a_property_list() {
        let (_dir, item) = item();
        set_comment(&item, "a note").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(
            xnu_attrs(&file),
            [(COMMENT_NAME.to_owned(), bplist::encode_string("a note"))]
        );
        assert_eq!(
            read(&item).await.unwrap().comment.as_deref(),
            Some("a note")
        );
    }

    #[tokio::test]
    async fn removing_the_last_attribute_removes_the_sidecar() {
        let (_dir, item) = item();
        set_comment(&item, "a note").await.unwrap();
        set_finder_info(&item, text_info()).await.unwrap();
        set_comment(&item, "").await.unwrap();
        assert!(path_of(&item).exists());
        assert_eq!(read(&item).await.unwrap().comment, None);
        set_finder_info(&item, FinderInfo::default()).await.unwrap();
        assert!(!path_of(&item).exists());
        set_comment(&item, "").await.unwrap();
    }

    #[tokio::test]
    async fn a_remove_from_a_sidecar_with_no_attributes_removes_it() {
        let (_dir, item) = item();
        std::fs::write(path_of(&item), new_sidecar(0)).unwrap();
        let error = remove_attr(&item, AttrName::Other(COMMENT_NAME))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(!path_of(&item).exists());
    }

    #[tokio::test]
    async fn a_finder_info_change_keeps_the_attributes_and_the_fork() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[("com.apple.quarantine", b"q-data"), ("user.a", b"hello")];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"rsrc-bytes")).unwrap();
        let info = FinderInfo {
            file_type: *b"APPL",
            ..text_info()
        };
        set_finder_info(&item, info).await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(&file[50..60], b"APPLttxt\x01\x00");
        let mut expected = macos_sidecar(attrs, b"rsrc-bytes");
        expected[50..60].copy_from_slice(b"APPLttxt\x01\x00");
        assert_eq!(file, expected);
    }

    #[tokio::test]
    async fn a_new_comment_follows_the_other_attributes() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[("com.apple.quarantine", b"q-data"), ("user.a", b"hello")];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"rsrc-bytes")).unwrap();
        set_comment(&item, "a note").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        let list = bplist::encode_string("a note");
        assert_eq!(
            xnu_attrs(&file),
            [
                ("com.apple.quarantine".to_owned(), b"q-data".to_vec()),
                ("user.a".to_owned(), b"hello".to_vec()),
                (COMMENT_NAME.to_owned(), list.clone()),
            ]
        );
        let with_comment: &[(&str, &[u8])] = &[
            ("com.apple.quarantine", b"q-data"),
            ("user.a", b"hello"),
            (COMMENT_NAME, &list),
        ];
        // xnu leaves the bytes of an attribute in the pad after a new name.
        let mut expected = macos_sidecar(with_comment, b"rsrc-bytes");
        let pad = 152 + 11 + COMMENT_NAME.len() + 1;
        expected[pad..pad + 2].copy_from_slice(&file[pad..pad + 2]);
        assert_eq!(file, expected);
        assert_eq!(fork_of(&item).await, b"rsrc-bytes");
    }

    #[tokio::test]
    async fn a_comment_of_the_same_length_changes_in_place() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[
            (COMMENT_NAME, &bplist::encode_string("one")),
            ("user.a", b"hello"),
        ];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"")).unwrap();
        set_comment(&item, "two").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(
            xnu_attrs(&file)[0],
            (COMMENT_NAME.to_owned(), bplist::encode_string("two"))
        );
    }

    #[tokio::test]
    async fn a_comment_of_a_new_length_moves_to_the_end() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[
            ("com.apple.quarantine", b"q-data"),
            (COMMENT_NAME, &bplist::encode_string("one")),
            ("user.a", b"hello"),
        ];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"rsrc-bytes")).unwrap();
        set_comment(&item, "a longer note").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(
            xnu_attrs(&file),
            [
                ("com.apple.quarantine".to_owned(), b"q-data".to_vec()),
                ("user.a".to_owned(), b"hello".to_vec()),
                (
                    COMMENT_NAME.to_owned(),
                    bplist::encode_string("a longer note")
                ),
            ]
        );
        assert_eq!(fork_bytes(&file), b"rsrc-bytes");
    }

    #[tokio::test]
    async fn removing_a_comment_keeps_the_attributes_around_it() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[
            ("com.apple.quarantine", b"q-data"),
            (COMMENT_NAME, &bplist::encode_string("one")),
            ("user.a", b"hello"),
        ];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"rsrc-bytes")).unwrap();
        set_comment(&item, "").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        let rest: &[(&str, &[u8])] = &[("com.apple.quarantine", b"q-data"), ("user.a", b"hello")];
        assert_eq!(file, macos_sidecar(rest, b"rsrc-bytes"));
    }

    #[tokio::test]
    async fn a_large_comment_grows_the_attributes_and_moves_the_fork() {
        let (_dir, item) = item();
        let attrs: &[(&str, &[u8])] = &[("user.a", b"hello")];
        std::fs::write(path_of(&item), macos_sidecar(attrs, b"rsrc-bytes")).unwrap();
        let comment = "x".repeat(9000);
        set_comment(&item, &comment).await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        // The entry and value need 5,432 bytes more than are free: 8,192.
        assert_eq!(be32(&file, 42), 3810 + 8192);
        assert_eq!(be32(&file, 34), 3760 + 8192);
        assert_eq!(xnu_attrs(&file).len(), 2);
        assert_eq!(fork_bytes(&file), b"rsrc-bytes");
        assert_eq!(read(&item).await.unwrap().comment, Some(comment));
    }

    #[tokio::test]
    async fn a_written_resource_fork_takes_the_place_of_the_empty_one() {
        let (_dir, item) = item();
        set_comment(&item, "a note").await.unwrap();
        let fork = vec![7u8; 300];
        write_fork(&item, 0, &fork).await;
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(be32(&file, 42), 3810);
        assert_eq!(fork_bytes(&file), fork.as_slice());
        assert_eq!(fork_of(&item).await, fork);
        assert_eq!(
            read(&item).await.unwrap().comment.as_deref(),
            Some("a note")
        );

        write_fork(&item, 300, b"more").await;
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 304);
        write_fork(&item, 0, &[9; 290]).await;
        assert_eq!(fork_of(&item).await, [9; 290]);
    }

    #[tokio::test]
    async fn a_resource_fork_keeps_its_length_until_the_writer_closes() {
        let (_dir, item) = item();
        let mut writer = open_resource_write(&item, 0, claim(&item)).await.unwrap();
        writer.write_all(&[1; 400]).await.unwrap();
        writer.flush().await.unwrap();
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 0);
        writer.shutdown().await.unwrap();
        assert_eq!(read(&item).await.unwrap().resource_fork_len(), 400);
    }

    #[tokio::test]
    async fn an_old_layout_gets_room_for_the_attributes() {
        let (_dir, item) = item();
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x02".to_vec();
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 50, 0, 0, 0, 32]);
        file.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 82, 0, 0, 0, 40]);
        file.extend_from_slice(b"TEXTttxt");
        file.resize(82, 0);
        file.extend_from_slice(&[5; 40]);
        std::fs::write(path_of(&item), &file).unwrap();
        // The first change sees only the bytes read before the layout grew.
        let error = set_comment(&item, "a note").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::StorageFull);
        set_comment(&item, "a note").await.unwrap();
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(be32(&file, 42), 4096);
        assert_eq!(fork_bytes(&file), [5; 40]);
        assert_eq!(&file[50..58], b"TEXTttxt");
        assert_eq!(xnu_attrs(&file).len(), 1);
        assert_eq!(
            read(&item).await.unwrap().comment.as_deref(),
            Some("a note")
        );
    }

    /// Check that `bytes` in a sidecar read as nothing, and that a change
    /// fails and leaves the file as it was.
    async fn assert_not_valid(bytes: &[u8]) {
        let (_dir, item) = item();
        std::fs::write(path_of(&item), bytes).unwrap();
        assert_eq!(read(&item).await.unwrap(), Sidecar::default());
        let error = set_comment(&item, "a note").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        let error = set_finder_info(&item, text_info()).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), bytes);
    }

    #[tokio::test]
    async fn a_file_that_is_not_appledouble_is_not_valid() {
        assert_not_valid(&[b'x'; 200]).await;
    }

    #[tokio::test]
    async fn a_file_with_more_than_fifteen_entries_is_not_valid() {
        let mut file = macos_sidecar(&[], b"");
        file[24..26].copy_from_slice(&16u16.to_be_bytes());
        assert_not_valid(&file).await;
        file[24..26].copy_from_slice(&0u16.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn entries_that_overlap_are_not_valid() {
        let mut file = macos_sidecar(&[], b"0123456789");
        file[42..46].copy_from_slice(&3800u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn an_entry_past_the_end_of_the_file_is_not_valid() {
        let mut file = macos_sidecar(&[], b"0123456789");
        file[46..50].copy_from_slice(&11u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn an_entry_in_the_table_is_not_valid() {
        let mut file = macos_sidecar(&[], b"");
        file[30..34].copy_from_slice(&49u32.to_be_bytes());
        assert_not_valid(&file).await;
    }

    #[tokio::test]
    async fn attributes_with_the_finder_info_away_from_50_are_not_valid() {
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x03".to_vec();
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 62, 0, 0, 0, 36]);
        file.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 98, 0, 0, 0, 1]);
        file.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 99, 0, 0, 0, 0]);
        file.extend_from_slice(b"TEXTttxt");
        file.resize(99, 0);
        assert_not_valid(&file).await;
    }

    /// A sidecar in the order of RFC 1740: a comment entry, the Finder info
    /// at 122, and an empty resource fork.
    fn rfc_sidecar() -> Vec<u8> {
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00".to_vec();
        file.extend_from_slice(&[0; 16]);
        file.extend_from_slice(&[0, 3]);
        file.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 62, 0, 0, 0, 60]);
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 122, 0, 0, 0, 32]);
        file.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 154, 0, 0, 0, 0]);
        file.resize(122, b'c');
        file.extend_from_slice(b"TEXTttxt");
        file.resize(154, 0);
        file
    }

    #[tokio::test]
    async fn a_finder_info_change_past_the_attribute_header_is_written() {
        let (_dir, item) = item();
        std::fs::write(path_of(&item), rfc_sidecar()).unwrap();
        let info = FinderInfo {
            file_type: *b"APPL",
            creator: *b"abcd",
            flags: 0,
        };
        set_finder_info(&item, info).await.unwrap();
        assert_eq!(read(&item).await.unwrap().finder_info(), info);
        let file = std::fs::read(path_of(&item)).unwrap();
        assert_eq!(file[62..122], [b'c'; 60]);
    }

    #[tokio::test]
    async fn a_fork_before_another_entry_is_not_written() {
        let (_dir, item) = item();
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00".to_vec();
        file.extend_from_slice(&[0; 16]);
        file.extend_from_slice(&[0, 2]);
        file.extend_from_slice(&[0, 0, 0, 2, 0, 0, 0, 50, 0, 0, 0, 4]);
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 54, 0, 0, 0, 32]);
        file.extend_from_slice(b"rsrcTEXTttxt");
        file.resize(86, 0);
        std::fs::write(path_of(&item), &file).unwrap();
        for at in [0, 4] {
            let error = open_resource_write(&item, at, claim(&item))
                .await
                .err()
                .unwrap();
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        }
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), file);
    }

    #[tokio::test]
    async fn attributes_do_not_grow_over_another_entry() {
        let (_dir, item) = item();
        let mut file = b"\x00\x05\x16\x07\x00\x02\x00\x00Mac OS X        \x00\x02".to_vec();
        file.extend_from_slice(&[0, 0, 0, 9, 0, 0, 0, 50, 0, 0, 0, 70]);
        file.extend_from_slice(&[0, 0, 0, 4, 0, 0, 0, 120, 0, 0, 0, 64]);
        file.extend_from_slice(b"TEXTttxt");
        file.resize(84, 0);
        file.extend_from_slice(b"ATTR");
        file.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 120, 0, 0, 0, 120]);
        // A comment entry long enough to hold the new attribute entry.
        file.resize(184, 0);
        std::fs::write(path_of(&item), &file).unwrap();
        let error = set_comment(&item, "a note").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), file);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_change_sets_the_change_time_and_keeps_the_modify_time() {
        use std::os::unix::fs::MetadataExt;
        let (_dir, item) = item();
        let before = std::fs::metadata(&item).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        set_comment(&item, "a note").await.unwrap();
        let after = std::fs::metadata(&item).unwrap();
        assert_eq!(after.modified().unwrap(), before.modified().unwrap());
        assert!((after.ctime(), after.ctime_nsec()) > (before.ctime(), before.ctime_nsec()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_new_sidecar_gets_the_mode_and_owner_of_its_item() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let (_dir, item) = item();
        std::fs::set_permissions(&item, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_comment(&item, "a note").await.unwrap();
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
        let error = set_comment(&item, "a note").await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(std::fs::read(path_of(&item)).unwrap(), b"");
    }

    /// `check_attrs` for `file`, a sidecar from [`macos_sidecar`].
    fn attrs_of(file: &[u8]) -> Option<AttrHeader> {
        let entry = Entry {
            id: FINDER_INFO_ID,
            offset: 50,
            len: be32(file, 34),
        };
        check_attrs(file, file.len(), &entry)
    }

    fn two_attrs() -> Vec<u8> {
        macos_sidecar(
            &[("com.apple.quarantine", b"q-data"), ("user.a", b"hello")],
            b"",
        )
    }

    #[test]
    fn attributes_from_macos_are_valid() {
        assert_eq!(
            attrs_of(&two_attrs()).map(|header| header.num_attrs),
            Some(2)
        );
    }

    #[test]
    fn a_name_without_its_nul_is_not_valid() {
        let mut file = two_attrs();
        file[151] = b'x';
        assert_eq!(attrs_of(&file), None);
    }

    #[test]
    fn attribute_data_past_the_data_length_is_not_valid() {
        let mut file = two_attrs();
        file[156..160].copy_from_slice(&6u32.to_be_bytes());
        assert_eq!(attrs_of(&file), None);
    }

    #[test]
    fn attributes_whose_lengths_do_not_add_to_the_data_length_are_not_valid() {
        let mut file = two_attrs();
        file[156..160].copy_from_slice(&4u32.to_be_bytes());
        assert_eq!(attrs_of(&file), None);
    }

    #[test]
    fn attribute_data_before_the_data_start_is_not_valid() {
        let mut file = two_attrs();
        file[120..124].copy_from_slice(&171u32.to_be_bytes());
        assert_eq!(attrs_of(&file), None);
    }

    #[test]
    fn a_data_start_past_the_bytes_read_is_not_valid() {
        let file = two_attrs();
        let entry = Entry {
            id: FINDER_INFO_ID,
            offset: 50,
            len: 3760,
        };
        assert!(check_attrs(&file, 172, &entry).is_some());
        assert_eq!(check_attrs(&file, 171, &entry), None);
    }

    #[test]
    fn path_of_prefixes_the_name() {
        assert_eq!(path_of(Path::new("dir/a.txt")), Path::new("dir/._a.txt"));
    }
}
