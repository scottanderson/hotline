//! A directory-backed [`FileProvider`], the same directory tree for
//! every login. `hotline-server` deliberately doesn't own file storage
//! (see its module docs); this is the concrete backend `hls` plugs in.
//!
//! A [`ForkStore`] keeps each item's Finder info, comment, and resource
//! fork. Listings hide the names that it keeps for itself.

use std::io;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use hotline_proto::types::{FileNameWithInfo, HlPath};
use hotline_server::{
    partial_upload_name, type_and_creator_for_name, validate_file_name, BoxedReader, BoxedWriter,
    FileProvider, FileRequester, FinderInfo, ItemTimes,
};

use crate::fork_store::{ForkStore, ForkStoreKind};

/// Serves one directory on disk, the same for every login.
pub struct DiskFileProvider {
    root: PathBuf,
    fork_store: Box<dyn ForkStore>,
}

impl DiskFileProvider {
    /// Open (creating if missing) `root` to serve.
    pub async fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(&root).await?;
        Ok(Self {
            root,
            fork_store: ForkStoreKind::AppleDouble.store(),
        })
    }

    /// Keep each item's Mac metadata in `fork_store`.
    pub fn with_fork_store(mut self, fork_store: Box<dyn ForkStore>) -> Self {
        self.fork_store = fork_store;
        self
    }

    /// Resolve a directory path under `root`. `None` when a component is
    /// not a plain name.
    fn resolve_dir(&self, dir: &HlPath) -> Option<PathBuf> {
        let mut path = self.root.clone();
        for component in &dir.components {
            if !self.is_plain_name(component) {
                return None;
            }
            path.push(component);
        }
        Some(path)
    }

    /// Resolve the entry `name` in `dir` under `root`, with the checks of
    /// [`Self::resolve_dir`].
    fn resolve_file(&self, dir: &HlPath, name: &str) -> Option<PathBuf> {
        if !self.is_plain_name(name) {
            return None;
        }
        let mut path = self.resolve_dir(dir)?;
        path.push(name);
        Some(path)
    }
    /// Whether `name` is one normal path component that the fork store
    /// does not keep, with no `:` on Windows.
    fn is_plain_name(&self, name: &str) -> bool {
        let mut components = Path::new(name).components();
        !name.contains(['/', '\\'])
            && !self.fork_store.is_reserved(name)
            && !(cfg!(windows) && name.contains(':'))
            && matches!(components.next(), Some(Component::Normal(_)))
            && components.next().is_none()
    }

    /// The name of `entry` if a listing shows it: UTF-8 and not kept by the
    /// fork store.
    fn listed_name(&self, entry: &tokio::fs::DirEntry) -> Option<String> {
        let name = entry.file_name().into_string().ok()?;
        (!self.fork_store.is_reserved(&name)).then_some(name)
    }

    /// How many entries a listing of the folder at `path` shows. 0 when it
    /// cannot be read.
    async fn count_entries(&self, path: &Path) -> u32 {
        let Ok(mut read_dir) = tokio::fs::read_dir(path).await else {
            return 0;
        };
        let mut count = 0;
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            if self.listed_name(&entry).is_some() {
                count += 1;
            }
        }
        count
    }
}

/// The type and creator of the file `name`: the stored ones, or those for
/// `name` when the stored type is zero.
fn file_type_and_creator(stored: FinderInfo, name: &str) -> ([u8; 4], [u8; 4]) {
    if stored.file_type == [0; 4] {
        type_and_creator_for_name(name)
    } else {
        (stored.file_type, stored.creator)
    }
}

#[async_trait]
impl FileProvider for DiskFileProvider {
    fn stored_name(&self, name: &str) -> Result<String, &'static str> {
        validate_file_name(name).map(|name| self.fork_store.unreserved_name(name))
    }

    async fn list(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
    ) -> Option<Vec<FileNameWithInfo>> {
        let path = self.resolve_dir(dir)?;
        let mut read_dir = tokio::fs::read_dir(&path).await.ok()?;
        let mut entries = Vec::new();
        while let Ok(Some(entry)) = read_dir.next_entry().await {
            let Some(name) = self.listed_name(&entry) else {
                continue;
            };
            let metadata = match tokio::fs::metadata(entry.path()).await {
                Ok(metadata) => metadata,
                Err(_) => match entry.metadata().await {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                },
            };
            entries.push(if metadata.is_dir() {
                FileNameWithInfo {
                    file_type: *b"fldr",
                    creator: *b"\0\0\0\0",
                    size: self.count_entries(&entry.path()).await,
                    name: name.into(),
                }
            } else {
                let stored = self.fork_store.finder_info(&entry.path()).await;
                let (file_type, creator) = file_type_and_creator(stored, &name);
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
        let file = tokio::fs::File::create(&path).await?;
        self.fork_store.clear(&path).await?;
        Ok(Box::new(file))
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

    async fn finish_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<()> {
        self.rename(requester, dir, &partial_upload_name(name), name)
            .await
    }

    async fn resource_fork_size(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> u64 {
        let Some(path) = self.resolve_file(dir, name) else {
            return 0;
        };
        self.fork_store.resource_fork_size(&path).await
    }

    async fn open_resource_read(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedReader> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        self.fork_store.open_resource_read(&path, at).await
    }

    async fn open_resource_write(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedWriter> {
        let path = self
            .resolve_file(dir, &partial_upload_name(name))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        self.fork_store.open_resource_write(&path, at).await
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
        let stored = self.fork_store.finder_info(&path).await;
        let (file_type, creator) = file_type_and_creator(stored, name);
        Some(FileNameWithInfo {
            file_type,
            creator,
            size: metadata.len() as u32,
            name: name.into(),
        })
    }

    async fn finder_info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<FinderInfo> {
        let path = self.resolve_file(dir, name)?;
        let metadata = tokio::fs::metadata(&path).await.ok()?;
        let stored = self.fork_store.finder_info(&path).await;
        let (file_type, creator) = if metadata.is_dir() {
            (*b"fldr", *b"\0\0\0\0")
        } else {
            file_type_and_creator(stored, name)
        };
        Some(FinderInfo {
            file_type,
            creator,
            flags: stored.flags,
        })
    }

    async fn set_finder_info(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        info: FinderInfo,
    ) -> io::Result<()> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.fork_store.set_finder_info(&path, info).await
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
        self.fork_store.remove(&path, is_dir).await
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
        self.fork_store.rename(&old_path, &new_path).await
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
        self.fork_store.rename(&old_path, &new_path).await
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
        let path = self.resolve_file(dir, name)?;
        self.fork_store.comment(&path).await
    }

    async fn set_comment(
        &self,
        _requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        comment: &str,
    ) -> io::Result<()> {
        let path = self
            .resolve_file(dir, name)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        self.fork_store.set_comment(&path, comment).await
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        tokio::fs::create_dir(dir.path().join("sub").join("inner"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("sub").join("b.txt"), b"")
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        assert_eq!(entries[1].size, 2);
    }

    #[tokio::test]
    async fn sidecars_are_hidden_and_refused() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("._a.txt"), b"sidecar")
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let root = HlPath::default();
        let entries = provider.list(requester(), &root).await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["a.txt"]);
        assert!(!provider.exists(requester(), &root, "._a.txt").await);
        assert!(provider
            .open_read(requester(), &root, "._a.txt")
            .await
            .is_err());
        assert!(provider
            .delete(requester(), &root, "._a.txt", false)
            .await
            .is_err());
        assert!(provider
            .list(requester(), &HlPath::new(["._a.txt"]))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn stored_name_maps_a_sidecar_name() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        assert_eq!(provider.stored_name("._a.txt").unwrap(), "-_a.txt");
        assert_eq!(provider.stored_name("._").unwrap(), "-_");
        assert_eq!(provider.stored_name("a._b").unwrap(), "a._b");
        let provider = provider.with_fork_store(ForkStoreKind::NoStore.store());
        let validated = validate_file_name("._a.txt").unwrap();
        assert_eq!(provider.stored_name("._a.txt").unwrap(), validated);
    }

    #[tokio::test]
    async fn without_a_fork_store_a_sidecar_name_is_a_file() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("._a.txt"), b"x")
            .await
            .unwrap();
        let provider = DiskFileProvider::open(dir.path())
            .await
            .unwrap()
            .with_fork_store(ForkStoreKind::NoStore.store());
        let root = HlPath::default();
        let entries = provider.list(requester(), &root).await.unwrap();
        let names: Vec<_> = entries.iter().map(|e| &*e.name).collect();
        assert_eq!(names, ["._a.txt"]);
        assert!(provider
            .set_comment(requester(), &root, "._a.txt", "a note")
            .await
            .is_err());

        provider
            .create_write(requester(), &root, "b.txt")
            .await
            .unwrap();
        let mut writer = provider
            .open_resource_write(requester(), &root, "b.txt", 0)
            .await
            .unwrap();
        writer.write_all(b"rsrc").await.unwrap();
        writer.shutdown().await.unwrap();
        let partial = partial_upload_name("b.txt");
        let size = provider
            .resource_fork_size(requester(), &root, &partial)
            .await;
        assert_eq!(size, 0);
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let items = provider
            .walk(requester(), &HlPath::default(), "Top")
            .await
            .unwrap();
        let names: Vec<_> = items.iter().map(|i| i.path.components.join("/")).collect();
        assert_eq!(names, ["a.txt"]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn list_counts_only_the_entries_it_would_show_in_a_folder() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Top");
        tokio::fs::create_dir(&folder).await.unwrap();
        tokio::fs::write(folder.join("a.txt"), b"x").await.unwrap();
        let raw = std::ffi::OsStr::from_bytes(b"\xff.txt");
        tokio::fs::write(folder.join(raw), b"x").await.unwrap();
        tokio::fs::write(folder.join("._a.txt"), b"x")
            .await
            .unwrap();

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let entries = provider
            .list(requester(), &HlPath::default())
            .await
            .unwrap();
        let top = entries.iter().find(|e| &*e.name == "Top").unwrap();
        assert_eq!(top.size, 1);
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
    async fn an_uploaded_resource_fork_follows_the_file() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let root = HlPath::default();
        let mut writer = provider
            .create_write(requester(), &root, "a.txt")
            .await
            .unwrap();
        writer.write_all(b"hello").await.unwrap();
        writer.shutdown().await.unwrap();
        let mut writer = provider
            .open_resource_write(requester(), &root, "a.txt", 0)
            .await
            .unwrap();
        writer.write_all(b"rsrc").await.unwrap();
        writer.shutdown().await.unwrap();
        let partial = partial_upload_name("a.txt");
        assert_eq!(
            provider
                .resource_fork_size(requester(), &root, &partial)
                .await,
            4
        );
        provider
            .finish_upload(requester(), &root, "a.txt")
            .await
            .unwrap();

        assert_eq!(
            provider
                .resource_fork_size(requester(), &root, "a.txt")
                .await,
            4
        );
        let mut reader = provider
            .open_resource_read(requester(), &root, "a.txt", 0)
            .await
            .unwrap();
        let mut fork = Vec::new();
        reader.read_to_end(&mut fork).await.unwrap();
        assert_eq!(fork, b"rsrc");
    }

    #[tokio::test]
    async fn a_sidecar_change_waits_for_the_resource_fork_writer() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let root = HlPath::default();
        provider
            .create_write(requester(), &root, "a.txt")
            .await
            .unwrap();
        let partial = partial_upload_name("a.txt");
        let mut writer = provider
            .open_resource_write(requester(), &root, "a.txt", 0)
            .await
            .unwrap();
        writer.write_all(b"rsrc").await.unwrap();

        let busy = provider
            .set_comment(requester(), &root, &partial, "a note")
            .await
            .unwrap_err();
        assert_eq!(busy.kind(), io::ErrorKind::ResourceBusy);
        let second = provider
            .open_resource_write(requester(), &root, "a.txt", 0)
            .await;
        assert_eq!(second.err().unwrap().kind(), io::ErrorKind::ResourceBusy);

        writer.shutdown().await.unwrap();
        drop(writer);
        provider
            .set_comment(requester(), &root, &partial, "a note")
            .await
            .unwrap();
        assert_eq!(
            provider
                .resource_fork_size(requester(), &root, &partial)
                .await,
            4
        );
    }

    #[tokio::test]
    async fn create_write_drops_a_stale_resource_fork() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let root = HlPath::default();
        provider
            .create_write(requester(), &root, "a.txt")
            .await
            .unwrap();
        let mut writer = provider
            .open_resource_write(requester(), &root, "a.txt", 0)
            .await
            .unwrap();
        writer.write_all(b"rsrc").await.unwrap();
        writer.shutdown().await.unwrap();

        provider
            .create_write(requester(), &root, "a.txt")
            .await
            .unwrap();
        let partial = partial_upload_name("a.txt");
        assert_eq!(
            provider
                .resource_fork_size(requester(), &root, &partial)
                .await,
            0
        );
    }

    #[tokio::test]
    async fn open_write_resume_appends_to_an_existing_file() {
        use tokio::io::AsyncWriteExt;

        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

        assert!(provider
            .open_write_resume(requester(), &HlPath::default(), "missing.txt", 0)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn set_comment_round_trips_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        {
            let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let reopened = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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

        let reopened = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
    async fn set_finder_info_sets_the_listed_type() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        let root = HlPath::default();
        let info = FinderInfo {
            file_type: *b"APPL",
            creator: *b"aust",
            flags: 0x0400,
        };
        provider
            .set_finder_info(requester(), &root, "a.txt", info)
            .await
            .unwrap();

        let entries = provider.list(requester(), &root).await.unwrap();
        assert_eq!(&entries[0].file_type, b"APPL");
        assert_eq!(&entries[0].creator, b"aust");
        let stored = provider.finder_info(requester(), &root, "a.txt").await;
        assert_eq!(stored, Some(info));

        let zero_type = FinderInfo {
            flags: 0x0400,
            ..FinderInfo::default()
        };
        provider
            .set_finder_info(requester(), &root, "a.txt", zero_type)
            .await
            .unwrap();
        let info = provider.info(requester(), &root, "a.txt").await.unwrap();
        assert_eq!(&info.file_type, b"TEXT");
        assert_eq!(&info.creator, b"ttxt");
    }

    #[tokio::test]
    async fn clearing_the_only_comment_removes_the_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        tokio::fs::write(dir.path().join("a.txt"), b"hi")
            .await
            .unwrap();
        let root = HlPath::default();
        provider
            .set_comment(requester(), &root, "a.txt", "a note")
            .await
            .unwrap();
        assert!(dir.path().join("._a.txt").is_file());

        provider
            .set_comment(requester(), &root, "a.txt", "")
            .await
            .unwrap();
        assert!(!dir.path().join("._a.txt").exists());
    }

    #[tokio::test]
    async fn set_comment_on_a_missing_item_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
        let result = provider
            .set_comment(requester(), &HlPath::default(), "a.txt", "a note")
            .await;
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        assert!(!dir.path().join("._a.txt").exists());
    }

    #[tokio::test]
    async fn a_comment_sidecar_never_appears_in_a_directory_listing() {
        let dir = tempfile::tempdir().unwrap();
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();
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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

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
        let provider = DiskFileProvider::open(dir.path()).await.unwrap();

        // Authorized as a file (is_dir = false); must not fall back to
        // deleting the real directory on disk.
        let result = provider
            .delete(requester(), &HlPath::default(), "actually_a_folder", false)
            .await;
        assert!(result.is_err());
        assert!(dir.path().join("actually_a_folder").is_dir());
    }
}
