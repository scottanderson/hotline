use std::io;

use async_trait::async_trait;
use hotline_proto::types::{FileNameWithInfo, HlPath};

use super::{partial_upload_name, BoxedReader, BoxedWriter, FileRequester, FolderItem};

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

    /// Every file and folder under `dir`/`name`, in pre-order, each folder
    /// just before its contents. Each file's size comes from
    /// [`Self::size`], skipping any entry it rejects. Empty if `name` is a
    /// file; `None` if it doesn't exist.
    async fn walk(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<Vec<FolderItem>> {
        let info = self.info(requester, dir, name).await?;
        let mut items = Vec::new();
        if info.file_type != *b"fldr" {
            return Some(items);
        }
        let mut root = dir.components.clone();
        root.push(name.to_string());
        let mut stack: Vec<(Vec<String>, FileNameWithInfo)> = Vec::new();
        let mut next_dir = Some(Vec::new());
        loop {
            if let Some(parent) = next_dir.take() {
                let full = HlPath::new(root.iter().chain(&parent));
                let entries = self.list(requester, &full).await.unwrap_or_default();
                stack.extend(entries.into_iter().rev().map(|e| (parent.clone(), e)));
            }
            let Some((mut path, entry)) = stack.pop() else {
                break;
            };
            let is_folder = entry.file_type == *b"fldr";
            let size = if is_folder {
                0
            } else {
                let dir = HlPath::new(root.iter().chain(&path));
                match self.size(requester, &dir, &entry.name).await {
                    Some(size) => size,
                    None => continue,
                }
            };
            path.push(entry.name);
            if is_folder {
                next_dir = Some(path.clone());
            }
            items.push(FolderItem {
                path: HlPath::new(path),
                is_folder,
                size,
            });
        }
        Some(items)
    }

    /// Whether a file or folder already exists at this path.
    async fn exists(&self, requester: FileRequester<'_>, dir: &HlPath, name: &str) -> bool;

    /// Open a file for reading, for a `FILE_GET` download.
    async fn open_read(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedReader>;

    /// Create a file for writing, for a `FILE_PUT` upload. Writes to
    /// [`partial_upload_name`] until [`Self::finish_upload`] renames it
    /// to `name`.
    async fn create_write(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<BoxedWriter>;

    /// Open an in-progress upload for writing from `at` onward, resuming
    /// an interrupted `FILE_PUT`. Seeks to `at`, truncating anything past
    /// it. `Err` if there isn't an in-progress upload to resume.
    async fn open_write_resume(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
        at: u64,
    ) -> io::Result<BoxedWriter>;

    /// Bytes already written to `name`'s in-progress upload, or `None` if
    /// there isn't one (nothing there, or a completed file).
    async fn partial_upload_size(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> Option<u64> {
        self.size(requester, dir, &partial_upload_name(name)).await
    }

    /// Finish `name`'s upload, once every byte has arrived: rename it
    /// from [`partial_upload_name`] to its real name.
    async fn finish_upload(
        &self,
        requester: FileRequester<'_>,
        dir: &HlPath,
        name: &str,
    ) -> io::Result<()> {
        self.rename(requester, dir, &partial_upload_name(name), name)
            .await
    }

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
