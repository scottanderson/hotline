use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use hotline_proto::types::{FileNameWithInfo, HlPath};

use crate::file::writer::{ForkWriter, MemWriter};
use crate::file::{
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, Dir, FileProvider,
    FileRequester, FinderInfo, Node,
};

/// Per-item metadata, keyed by the item's directory and name.
pub(crate) type ItemMap<V> = HashMap<(Vec<String>, String), V>;

/// The tree and the resource forks, which the open writers share.
#[derive(Default)]
pub(crate) struct Store {
    pub(crate) root: Mutex<Dir>,
    pub(crate) resource_forks: Mutex<ItemMap<Vec<u8>>>,
}

/// Move every value keyed under `old_dir`/`old_name` to `new_dir`/
/// `new_name`: the entry itself, plus (if it's a folder) every descendant
/// value, whose key's directory has `old_dir`/`old_name` as a prefix.
fn rekey<V>(
    map: &mut ItemMap<V>,
    old_dir: &[String],
    old_name: &str,
    new_dir: &[String],
    new_name: &str,
) {
    if let Some(value) = map.remove(&(old_dir.to_vec(), old_name.to_string())) {
        map.insert((new_dir.to_vec(), new_name.to_string()), value);
    }
    let mut old_prefix = old_dir.to_vec();
    old_prefix.push(old_name.to_string());
    let mut new_prefix = new_dir.to_vec();
    new_prefix.push(new_name.to_string());
    let descendants: Vec<_> = map
        .keys()
        .filter(|(dir, _)| dir.starts_with(&old_prefix))
        .cloned()
        .collect();
    for (dir, name) in descendants {
        let value = map.remove(&(dir.clone(), name.clone())).unwrap();
        let mut rekeyed_dir = new_prefix.clone();
        rekeyed_dir.extend_from_slice(&dir[old_prefix.len()..]);
        map.insert((rekeyed_dir, name), value);
    }
}

/// Drop the value on `dir`/`name` and on everything under it.
fn drop_under<V>(map: &mut ItemMap<V>, dir: &[String], name: &str) {
    map.remove(&(dir.to_vec(), name.to_string()));
    let mut prefix = dir.to_vec();
    prefix.push(name.to_string());
    map.retain(|(d, _), _| !d.starts_with(&prefix));
}

/// The type and creator of the file `dir`/`name`: the stored ones, or
/// those for `name` when the stored type is zero.
fn file_type_and_creator(
    finder_infos: &ItemMap<FinderInfo>,
    dir: &[String],
    name: &str,
) -> ([u8; 4], [u8; 4]) {
    match finder_infos.get(&(dir.to_vec(), name.to_string())) {
        Some(info) if info.file_type != [0; 4] => (info.file_type, info.creator),
        _ => type_and_creator_for_name(name),
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
    store: Arc<Store>,
    comments: Mutex<ItemMap<String>>,
    finder_infos: Mutex<ItemMap<FinderInfo>>,
    free_space: Mutex<Option<u64>>,
    comment_writes_fail: AtomicBool,
}

impl InMemoryFileProvider {
    /// Seed a file at `dir`/`name` with `data`, creating any missing
    /// intermediate directories. For tests/fixtures.
    pub fn seed(&self, dir: &HlPath, name: &str, data: impl Into<Vec<u8>>) {
        let mut root = self.store.root.lock().unwrap();
        let d = root
            .make_dirs(&dir.components)
            .expect("the seed path runs through a file");
        d.entries.insert(name.to_string(), Node::File(data.into()));
    }

    /// Set the resource fork of `dir`/`name` to `data`. For tests/fixtures.
    pub fn seed_resource_fork(&self, dir: &HlPath, name: &str, data: impl Into<Vec<u8>>) {
        let key = (dir.components.clone(), name.to_string());
        self.store
            .resource_forks
            .lock()
            .unwrap()
            .insert(key, data.into());
    }

    /// Set the bytes that [`FileProvider::free_space`] reports.
    pub fn set_free_space(&self, bytes: Option<u64>) {
        *self.free_space.lock().unwrap() = bytes;
    }

    /// Make [`FileProvider::set_comment`] fail when `fail` is true.
    pub fn set_comment_writes_fail(&self, fail: bool) {
        self.comment_writes_fail.store(fail, Ordering::Relaxed);
    }
}

#[async_trait]
impl FileProvider for InMemoryFileProvider {
    async fn list(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        let root = self.store.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        let finder_infos = self.finder_infos.lock().unwrap();
        let mut entries: Vec<FileNameWithInfo> = d
            .entries
            .iter()
            .map(|(name, entry)| {
                let (file_type, creator, size) = match entry {
                    Node::Dir(d) => (*b"fldr", *b"\0\0\0\0", d.entries.len() as u32),
                    Node::File(f) => {
                        let (file_type, creator) =
                            file_type_and_creator(&finder_infos, &dir.components, name);
                        (file_type, creator, f.len() as u32)
                    }
                };
                FileNameWithInfo {
                    file_type,
                    creator,
                    size,
                    name: name.as_str().into(),
                }
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Some(entries)
    }

    async fn size(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> Option<u64> {
        let root = self.store.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        match d.entries.get(name)? {
            Node::File(data) => Some(data.len() as u64),
            Node::Dir(_) => None,
        }
    }

    async fn free_space(&self, _requester: FileRequester<'_>, _dir: &HlPath) -> Option<u64> {
        *self.free_space.lock().unwrap()
    }

    async fn exists(&self, _requester: FileRequester<'_>, dir: &HlPath, name: &str) -> bool {
        let root = self.store.root.lock().unwrap();
        root.get_dir(&dir.components)
            .is_some_and(|d| d.entries.contains_key(name))
    }

    async fn open_read(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedReader> {
        let root = self.store.root.lock().unwrap();
        let d = root.get_dir(&dir.components).ok_or_else(not_found)?;
        match d.entries.get(name) {
            Some(Node::File(data)) => Ok(Box::new(io::Cursor::new(data.clone()))),
            Some(Node::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(not_found()),
        }
    }

    async fn create_write(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedWriter> {
        let partial_name = partial_upload_name(name);
        let mut root = self.store.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        if let Some(Node::Dir(_)) = d.entries.get(&partial_name) {
            return Err(io::Error::from(io::ErrorKind::IsADirectory));
        }
        d.entries
            .insert(partial_name.clone(), Node::File(Vec::new()));
        drop(root);
        self.store
            .resource_forks
            .lock()
            .unwrap()
            .remove(&(dir.components.clone(), partial_name.clone()));
        Ok(Box::new(MemWriter::appending(
            self.store.clone(),
            dir.components.clone(),
            partial_name,
        )))
    }

    async fn open_write_resume(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedWriter> {
        let partial_name = partial_upload_name(name);
        let mut root = self.store.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        match d.entries.get_mut(&partial_name) {
            Some(Node::File(data)) => {
                if data.len() as u64 != at {
                    tracing::debug!(
                        name,
                        current_len = data.len(),
                        at,
                        "resuming upload: on-disk length didn't match the negotiated offset"
                    );
                }
                // Matches a real seek-and-write: a gap reads back as zeros.
                data.resize(at as usize, 0);
                drop(root);
                Ok(Box::new(MemWriter::appending(
                    self.store.clone(),
                    dir.components.clone(),
                    partial_name,
                )))
            }
            Some(Node::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(not_found()),
        }
    }

    async fn info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FileNameWithInfo> {
        let root = self.store.root.lock().unwrap();
        let d = root.get_dir(&dir.components)?;
        Some(match d.entries.get(name)? {
            Node::Dir(_) => FileNameWithInfo {
                file_type: *b"fldr",
                creator: *b"\0\0\0\0",
                size: 0,
                name: name.into(),
            },
            Node::File(data) => {
                let finder_infos = self.finder_infos.lock().unwrap();
                let (file_type, creator) =
                    file_type_and_creator(&finder_infos, &dir.components, name);
                FileNameWithInfo {
                    file_type,
                    creator,
                    size: data.len() as u32,
                    name: name.into(),
                }
            }
        })
    }

    async fn finder_info(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FinderInfo> {
        let info = self.info(requester, dir, name).await?;
        let key = (dir.components.clone(), name.to_string());
        let flags = self
            .finder_infos
            .lock()
            .unwrap()
            .get(&key)
            .map_or(0, |stored| stored.flags);
        Some(FinderInfo {
            file_type: info.file_type,
            creator: info.creator,
            flags,
        })
    }

    async fn set_finder_info(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        info: FinderInfo,
    ) -> io::Result<()> {
        if !self.exists(requester, dir, name).await {
            return Err(not_found());
        }
        let key = (dir.components.clone(), name.to_string());
        self.finder_infos.lock().unwrap().insert(key, info);
        Ok(())
    }

    async fn resource_fork_size(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> u64 {
        let key = (dir.components.clone(), name.to_string());
        self.store
            .resource_forks
            .lock()
            .unwrap()
            .get(&key)
            .map_or(0, |fork| fork.len() as u64)
    }

    async fn open_resource_read(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedReader> {
        let key = (dir.components.clone(), name.to_string());
        let fork = self
            .store
            .resource_forks
            .lock()
            .unwrap()
            .get(&key)
            .map(|fork| fork.get(at as usize..).unwrap_or_default().to_vec())
            .unwrap_or_default();
        Ok(Box::new(io::Cursor::new(fork)))
    }

    async fn open_resource_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedWriter> {
        let partial_name = partial_upload_name(name);
        if !self.exists(requester, dir, &partial_name).await {
            return Err(not_found());
        }
        let key = (dir.components.clone(), partial_name);
        self.store
            .resource_forks
            .lock()
            .unwrap()
            .entry(key.clone())
            .or_default()
            .resize(at as usize, 0);
        Ok(Box::new(ForkWriter::appending(self.store.clone(), key)))
    }

    async fn delete(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        _is_dir: bool,
    ) -> io::Result<()> {
        let mut root = self.store.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        d.entries.remove(name).ok_or_else(not_found)?;
        drop_under(&mut self.comments.lock().unwrap(), &dir.components, name);
        drop_under(
            &mut self.finder_infos.lock().unwrap(),
            &dir.components,
            name,
        );
        drop_under(
            &mut self.store.resource_forks.lock().unwrap(),
            &dir.components,
            name,
        );
        Ok(())
    }

    async fn mkdir(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<()> {
        let mut root = self.store.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        if d.entries.contains_key(name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        d.entries
            .insert(name.to_string(), Node::Dir(Dir::default()));
        Ok(())
    }

    async fn rename(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        old_name: &str,
        new_name: &str,
    ) -> io::Result<()> {
        let mut root = self.store.root.lock().unwrap();
        let d = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        if d.entries.contains_key(new_name) {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        let entry = d.entries.remove(old_name).ok_or_else(not_found)?;
        d.entries.insert(new_name.to_string(), entry);
        rekey(
            &mut self.comments.lock().unwrap(),
            &dir.components,
            old_name,
            &dir.components,
            new_name,
        );
        rekey(
            &mut self.finder_infos.lock().unwrap(),
            &dir.components,
            old_name,
            &dir.components,
            new_name,
        );
        rekey(
            &mut self.store.resource_forks.lock().unwrap(),
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
        let mut root = self.store.root.lock().unwrap();
        let source = root.get_dir_mut(&dir.components).ok_or_else(not_found)?;
        if !source.entries.contains_key(name) {
            return Err(not_found());
        }
        let into_itself = to_dir.components.len() > dir.components.len()
            && to_dir.components.starts_with(&dir.components)
            && to_dir.components[dir.components.len()] == name;
        if into_itself {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        match root.get_dir(&to_dir.components) {
            None => return Err(not_found()),
            Some(dest) if dest.entries.contains_key(name) => {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            Some(_) => {}
        }
        let entry = root
            .get_dir_mut(&dir.components)
            .and_then(|d| d.entries.remove(name))
            .ok_or_else(not_found)?;
        root.get_dir_mut(&to_dir.components)
            .ok_or_else(not_found)?
            .entries
            .insert(name.to_string(), entry);
        rekey(
            &mut self.comments.lock().unwrap(),
            &dir.components,
            name,
            &to_dir.components,
            name,
        );
        rekey(
            &mut self.finder_infos.lock().unwrap(),
            &dir.components,
            name,
            &to_dir.components,
            name,
        );
        rekey(
            &mut self.store.resource_forks.lock().unwrap(),
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
        let mut root = self.store.root.lock().unwrap();
        let entry = match root
            .get_dir(&target_dir.components)
            .and_then(|d| d.entries.get(target_name))
        {
            Some(entry) => entry.clone(),
            None => return Err(not_found()),
        };
        let dest = root
            .get_dir_mut(&link_dir.components)
            .ok_or_else(not_found)?;
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
        if self.comment_writes_fail.load(Ordering::Relaxed) {
            return Err(io::Error::other("comment write failed"));
        }
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

/// The error for a missing file or folder, or a path through a file.
fn not_found() -> io::Error {
    io::ErrorKind::NotFound.into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file::{FolderItem, PARTIAL_CREATOR, PARTIAL_TYPE};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn requester() -> FileRequester<'static> {
        FileRequester { login: "alice" }
    }

    #[tokio::test]
    async fn list_gives_a_folder_its_entry_count_as_its_size() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        provider.seed(&HlPath::new(["Top", "Sub"]), "b.txt", *b"x");

        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        assert_eq!(&*entries[0].name, "Top");
        assert_eq!(entries[0].size, 2);
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
        assert_eq!(&*entries[0].name, "a.txt");
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
    async fn walk_lists_a_folder_before_its_contents() {
        let provider = InMemoryFileProvider::default();
        let top = HlPath::new(["Top"]);
        provider.seed(&top, "b.txt", *b"bb");
        provider.seed(&HlPath::new(["Top", "A"]), "inner.txt", *b"x");
        provider.seed(&HlPath::default(), "outside.txt", *b"no");

        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        let item = |path: &[&str], is_folder, size| FolderItem {
            path: HlPath::new(path.iter().copied()),
            is_folder,
            size,
        };
        assert_eq!(
            items,
            [
                item(&["A"], true, 0),
                item(&["A", "inner.txt"], false, 1),
                item(&["b.txt"], false, 2),
            ]
        );
    }

    #[tokio::test]
    async fn walk_of_a_file_is_empty_and_of_a_missing_name_is_none() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hello");

        let root = HlPath::default();
        let walk = |name| provider.walk(requester(), &root, name);
        assert_eq!(walk("a.txt").await, Some(Vec::new()));
        assert_eq!(walk("missing").await, None);
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
                .partial_upload_size(requester(), &HlPath::default(), "b.txt")
                .await,
            Some(5)
        );
    }

    #[tokio::test]
    async fn an_in_progress_upload_is_listed_under_its_real_hpf_name() {
        let provider = InMemoryFileProvider::default();
        let mut writer = provider
            .create_write(requester(), &HlPath::default(), "b.txt")
            .await
            .unwrap();
        writer.write_all(b"world").await.unwrap();

        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        assert_eq!(&*entries[0].name, "b.txt.hpf");
        assert_eq!(entries[0].file_type, PARTIAL_TYPE);
        assert_eq!(entries[0].creator, PARTIAL_CREATOR);

        provider
            .finish_upload(requester(), &HlPath::default(), "b.txt")
            .await
            .unwrap();

        assert_eq!(
            provider
                .size(requester(), &HlPath::default(), "b.txt")
                .await,
            Some(5)
        );
        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        assert_eq!(&*entries[0].name, "b.txt");
        assert_ne!(entries[0].file_type, PARTIAL_TYPE);
    }

    #[tokio::test]
    async fn create_write_drops_a_stale_resource_fork() {
        let provider = InMemoryFileProvider::default();
        let root = HlPath::default();
        provider.seed(&root, &partial_upload_name("b.txt"), *b"old");
        provider.seed_resource_fork(&root, &partial_upload_name("b.txt"), *b"rsrc");
        provider
            .create_write(requester(), &root, "b.txt")
            .await
            .unwrap();
        let partial = partial_upload_name("b.txt");
        assert_eq!(
            provider
                .resource_fork_size(requester(), &root, &partial)
                .await,
            0
        );
    }

    #[tokio::test]
    async fn open_resource_write_appends_from_the_offset_and_follows_the_upload() {
        let provider = InMemoryFileProvider::default();
        let root = HlPath::default();
        provider.seed(&root, &partial_upload_name("b.txt"), *b"");
        provider.seed_resource_fork(&root, &partial_upload_name("b.txt"), *b"rsXX");
        let mut writer = provider
            .open_resource_write(requester(), &root, "b.txt", 2)
            .await
            .unwrap();
        writer.write_all(b"rc").await.unwrap();
        provider
            .finish_upload(requester(), &root, "b.txt")
            .await
            .unwrap();

        let mut reader = provider
            .open_resource_read(requester(), &root, "b.txt", 1)
            .await
            .unwrap();
        let mut fork = Vec::new();
        reader.read_to_end(&mut fork).await.unwrap();
        assert_eq!(fork, b"src");
    }

    #[tokio::test]
    async fn open_write_resume_appends_to_an_existing_file() {
        let provider = InMemoryFileProvider::default();
        provider.seed(
            &HlPath::default(),
            &partial_upload_name("b.txt"),
            *b"hello ",
        );
        let mut writer = provider
            .open_write_resume(requester(), &HlPath::default(), "b.txt", 6)
            .await
            .unwrap();
        writer.write_all(b"world").await.unwrap();

        assert_eq!(
            provider
                .partial_upload_size(requester(), &HlPath::default(), "b.txt")
                .await,
            Some(11)
        );
    }

    #[tokio::test]
    async fn open_write_resume_truncates_bytes_written_past_the_resume_offset() {
        let provider = InMemoryFileProvider::default();
        provider.seed(
            &HlPath::default(),
            &partial_upload_name("b.txt"),
            *b"hello world",
        );
        let mut writer = provider
            .open_write_resume(requester(), &HlPath::default(), "b.txt", 6)
            .await
            .unwrap();
        writer.write_all(b"there").await.unwrap();

        assert_eq!(
            provider
                .partial_upload_size(requester(), &HlPath::default(), "b.txt")
                .await,
            Some(11)
        );
        let mut reader = provider
            .open_read(
                requester(),
                &HlPath::default(),
                &partial_upload_name("b.txt"),
            )
            .await
            .unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).await.unwrap();
        assert_eq!(buf, b"hello there");
    }

    #[tokio::test]
    async fn open_write_resume_errors_when_the_file_does_not_exist() {
        let provider = InMemoryFileProvider::default();
        assert!(provider
            .open_write_resume(requester(), &HlPath::default(), "missing.txt", 0)
            .await
            .is_err());
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
        assert_eq!(&*entries[0].name, "a.txt");
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
    async fn move_onto_an_existing_name_keeps_both_entries() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"root");
        provider.seed(&HlPath::new(["Sub"]), "a.txt", *b"sub");

        let err = provider
            .r#move(
                requester(),
                &HlPath::default(),
                "a.txt",
                &HlPath::new(["Sub"]),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            provider
                .size(requester(), &HlPath::default(), "a.txt")
                .await,
            Some(4)
        );
    }

    #[tokio::test]
    async fn move_of_a_folder_into_itself_errors() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::new(["A", "B"]), "a.txt", *b"hi");

        let err = provider
            .r#move(
                requester(),
                &HlPath::default(),
                "A",
                &HlPath::new(["A", "B"]),
            )
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(
            provider
                .exists(requester(), &HlPath::new(["A", "B"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn a_path_through_a_file_errors_and_the_tree_stays_usable() {
        let provider = InMemoryFileProvider::default();
        provider.seed(&HlPath::default(), "a.txt", *b"hi");
        let through_file = HlPath::new(["a.txt"]);

        let err = provider
            .mkdir(requester(), &through_file, "New")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        let err = provider
            .symlink(requester(), &HlPath::default(), "a.txt", &through_file, "b")
            .await
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(provider
            .create_write(requester(), &through_file, "up.txt")
            .await
            .is_err());

        provider
            .mkdir(requester(), &HlPath::default(), "New")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn create_write_onto_a_folder_with_the_partial_name_errors() {
        let provider = InMemoryFileProvider::default();
        let partial = partial_upload_name("up.txt");
        provider.seed(&HlPath::new([partial.as_str()]), "inner.txt", *b"keep");

        let err = provider
            .create_write(requester(), &HlPath::default(), "up.txt")
            .await
            .err()
            .unwrap();
        assert_eq!(err.kind(), io::ErrorKind::IsADirectory);
        assert!(provider
            .list(requester(), &HlPath::new([partial.as_str()]))
            .await
            .is_some());
    }

    #[tokio::test]
    async fn create_write_into_a_missing_folder_errors() {
        let provider = InMemoryFileProvider::default();
        assert!(provider
            .create_write(requester(), &HlPath::new(["Nope"]), "up.txt")
            .await
            .is_err());
        assert!(provider
            .list(requester(), &HlPath::new(["Nope"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn deleting_a_folder_drops_its_own_and_its_contents_comments() {
        let provider = InMemoryFileProvider::default();
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
            .delete(requester(), &HlPath::default(), "Vacation", true)
            .await
            .unwrap();
        provider.seed(&HlPath::new(["Vacation"]), "photo.png", *b"img");

        assert_eq!(
            provider
                .comment(requester(), &HlPath::default(), "Vacation")
                .await,
            None
        );
        assert_eq!(
            provider
                .comment(requester(), &HlPath::new(["Vacation"]), "photo.png")
                .await,
            None
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
        assert_eq!(&*entries[0].name, "a.txt");
        assert_eq!(entries[0].size, 2);
    }
}
