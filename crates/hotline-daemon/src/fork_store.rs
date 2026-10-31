//! Where a [`DiskFileProvider`](crate::file::DiskFileProvider) keeps each
//! item's Finder info, comment, and resource fork.

use std::io;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use clap::ValueEnum;
use hotline_server::{BoxedReader, BoxedWriter, FinderInfo};

use crate::apple_double::{self, Sidecars};

/// Stores the Mac metadata of the items on disk. Each `item` is the path
/// of a file or folder. A store that cannot keep the Finder info or the
/// comment returns `Unsupported` when it is set.
#[async_trait]
pub trait ForkStore: Send + Sync {
    /// Whether the store keeps `name` for itself. A listing hides it, and
    /// a path cannot name it.
    fn is_reserved(&self, _name: &str) -> bool {
        false
    }

    /// `name`, changed so that the store does not keep it for itself.
    fn unreserved_name(&self, name: String) -> String {
        name
    }

    /// The stored Finder info, zero when there is none.
    async fn finder_info(&self, item: &Path) -> FinderInfo;

    /// Store the Finder info of `item`, which exists.
    async fn set_finder_info(&self, item: &Path, info: FinderInfo) -> io::Result<()>;

    /// The stored comment, `None` when there is none.
    async fn comment(&self, item: &Path) -> Option<String>;

    /// Store the comment of `item`, which exists.
    async fn set_comment(&self, item: &Path, comment: &str) -> io::Result<()>;

    /// The byte length of the resource fork.
    async fn resource_fork_size(&self, item: &Path) -> u64;

    /// Read the resource fork from `at` onward.
    async fn open_resource_read(&self, item: &Path, at: u64) -> io::Result<BoxedReader>;

    /// Write the resource fork from `at` onward, and cut it to `at`.
    async fn open_resource_write(&self, item: &Path, at: u64) -> io::Result<BoxedWriter>;

    /// Drop the stored data of `item`, for a new file there.
    async fn clear(&self, item: &Path) -> io::Result<()>;

    /// Delete `item` and its stored data.
    async fn remove(&self, item: &Path, is_dir: bool) -> io::Result<()> {
        remove_item(item, is_dir).await
    }

    /// Move `from` and its stored data to `to`.
    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        tokio::fs::rename(from, to).await
    }
}

/// The [`ForkStore`] that `--fork-store` selects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ForkStoreKind {
    /// An AppleDouble file, `._name`, beside each item.
    AppleDouble,
    /// No storage. Each item has no Finder info, comment, or resource fork.
    #[value(name = "none")]
    NoStore,
}

impl ForkStoreKind {
    /// A new store of this kind.
    pub fn store(self) -> Box<dyn ForkStore> {
        match self {
            Self::AppleDouble => Box::new(AppleDoubleStore::default()),
            Self::NoStore => Box::new(NoForkStore),
        }
    }
}

/// Delete the file or folder at `item`.
async fn remove_item(item: &Path, is_dir: bool) -> io::Result<()> {
    if is_dir {
        tokio::fs::remove_dir_all(item).await
    } else {
        tokio::fs::remove_file(item).await
    }
}

/// Keeps each item's data in its AppleDouble sidecar (see [`apple_double`]).
#[derive(Default)]
pub struct AppleDoubleStore {
    sidecars: Arc<Sidecars>,
}

impl AppleDoubleStore {
    /// Change the sidecar of `item` with `change`. `NotFound` if the item
    /// does not exist, and `ResourceBusy` while an upload writes its
    /// resource fork.
    async fn update(
        &self,
        item: &Path,
        change: impl FnOnce(&mut apple_double::Sidecar),
    ) -> io::Result<()> {
        tokio::fs::symlink_metadata(item).await?;
        let path = apple_double::path_of(item);
        let _sidecars = self.sidecars.lock().await;
        if self.sidecars.contains(&path) {
            return Err(io::ErrorKind::ResourceBusy.into());
        }
        let mut sidecar = apple_double::read(&path).await?;
        change(&mut sidecar);
        apple_double::write(&path, &sidecar).await
    }
}

#[async_trait]
impl ForkStore for AppleDoubleStore {
    fn is_reserved(&self, name: &str) -> bool {
        name.starts_with(apple_double::PREFIX)
    }

    fn unreserved_name(&self, mut name: String) -> String {
        if self.is_reserved(&name) {
            name.replace_range(..1, "-");
        }
        name
    }

    async fn finder_info(&self, item: &Path) -> FinderInfo {
        let sidecar = apple_double::read(&apple_double::path_of(item)).await;
        sidecar.unwrap_or_default().finder_info()
    }

    async fn set_finder_info(&self, item: &Path, info: FinderInfo) -> io::Result<()> {
        self.update(item, |sidecar| sidecar.set_finder_info(info))
            .await
    }

    async fn comment(&self, item: &Path) -> Option<String> {
        let sidecar = apple_double::read(&apple_double::path_of(item)).await;
        let comment = sidecar.ok()?.comment;
        (!comment.is_empty()).then_some(comment)
    }

    async fn set_comment(&self, item: &Path, comment: &str) -> io::Result<()> {
        self.update(item, |sidecar| sidecar.comment = comment.into())
            .await
    }

    async fn resource_fork_size(&self, item: &Path) -> u64 {
        apple_double::read(&apple_double::path_of(item))
            .await
            .map_or(0, |sidecar| sidecar.resource_fork_len())
    }

    async fn open_resource_read(&self, item: &Path, at: u64) -> io::Result<BoxedReader> {
        apple_double::open_resource_read(&apple_double::path_of(item), at).await
    }

    async fn open_resource_write(&self, item: &Path, at: u64) -> io::Result<BoxedWriter> {
        tokio::fs::symlink_metadata(item).await?;
        let _sidecars = self.sidecars.lock().await;
        let path = apple_double::path_of(item);
        let claim = self
            .sidecars
            .claim(&path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::ResourceBusy))?;
        let writer = apple_double::open_resource_write(&path, at, claim).await?;
        Ok(Box::new(writer))
    }

    async fn clear(&self, item: &Path) -> io::Result<()> {
        let _sidecars = self.sidecars.lock().await;
        apple_double::remove(&apple_double::path_of(item)).await
    }

    async fn remove(&self, item: &Path, is_dir: bool) -> io::Result<()> {
        let _sidecars = self.sidecars.lock().await;
        remove_item(item, is_dir).await?;
        if let Err(error) = apple_double::remove(&apple_double::path_of(item)).await {
            tracing::debug!(%error, ?item, "could not remove the sidecar of a deleted entry");
        }
        Ok(())
    }

    async fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let _sidecars = self.sidecars.lock().await;
        tokio::fs::rename(from, to).await?;
        apple_double::rename(&apple_double::path_of(from), &apple_double::path_of(to)).await
    }
}

/// Keeps nothing. A change to Finder info or a comment is `Unsupported`,
/// and a written resource fork is discarded.
pub struct NoForkStore;

#[async_trait]
impl ForkStore for NoForkStore {
    async fn finder_info(&self, _item: &Path) -> FinderInfo {
        FinderInfo::default()
    }

    async fn set_finder_info(&self, _item: &Path, _info: FinderInfo) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    async fn comment(&self, _item: &Path) -> Option<String> {
        None
    }

    async fn set_comment(&self, _item: &Path, _comment: &str) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    async fn resource_fork_size(&self, _item: &Path) -> u64 {
        0
    }

    async fn open_resource_read(&self, _item: &Path, _at: u64) -> io::Result<BoxedReader> {
        Ok(Box::new(tokio::io::empty()))
    }

    async fn open_resource_write(&self, _item: &Path, _at: u64) -> io::Result<BoxedWriter> {
        Ok(Box::new(tokio::io::sink()))
    }

    async fn clear(&self, _item: &Path) -> io::Result<()> {
        Ok(())
    }
}
