use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use hotline_proto::types::{FileNameWithInfo, HlPath};

use crate::file::writer::MemWriter;
use crate::file::{
    partial_upload_name, type_and_creator_for_name, BoxedReader, BoxedWriter, Dir, FileProvider,
    FileRequester, Node,
};

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
        d.entries.insert(name.to_string(), Node::File(data.into()));
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
                    Node::Dir(_) => (*b"fldr", *b"\0\0\0\0", 0),
                    Node::File(f) => {
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
            Node::File(data) => Some(data.len() as u64),
            Node::Dir(_) => None,
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
            Some(Node::File(data)) => Ok(Box::new(io::Cursor::new(data.clone()))),
            Some(Node::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
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
            partial_upload_name(name),
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
        let mut root = self.root.lock().unwrap();
        let d = root
            .get_dir_mut_existing(&dir.components)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
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
                Ok(Box::new(MemWriter::append(
                    self.root.clone(),
                    dir.components.clone(),
                    partial_name,
                )))
            }
            Some(Node::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
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
            Node::Dir(_) => FileNameWithInfo {
                file_type: *b"fldr",
                creator: *b"\0\0\0\0",
                size: 0,
                name: name.to_string(),
            },
            Node::File(data) => {
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
    use crate::file::{PARTIAL_CREATOR, PARTIAL_TYPE};
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
        assert_eq!(entries[0].name, "b.txt.hpf");
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
        assert_eq!(entries[0].name, "b.txt");
        assert_ne!(entries[0].file_type, PARTIAL_TYPE);
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
