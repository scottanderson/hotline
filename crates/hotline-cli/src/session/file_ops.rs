//! File and folder operations in the current folder, other than transfers.

use hotline_client::{FileInfo, HotlineClient};
use hotline_proto::types::HlPath;

use super::ClientSession;
use crate::file_edit::FileEdit;
use crate::line::LineKind;

impl ClientSession {
    /// The current folder as `/`-separated text.
    pub fn folder_text(&self) -> String {
        format!("/{}", self.file_path.components.join("/"))
    }

    /// The file or folder `name` in the current folder, [`myTran_GetFileInfo`] (206).
    ///
    /// [`myTran_GetFileInfo`]: hotline_proto::Opcode::GetFileInfo
    pub async fn get_file_info(&mut self, client: &HotlineClient, name: &str) -> Option<FileInfo> {
        client
            .get_file_info(&self.file_path, name)
            .await
            .inspect_err(|e| self.push(format!("failed to get info: {e}"), LineKind::Error))
            .ok()
    }

    /// Delete the file or folder `name` in the current folder,
    /// [`myTran_DeleteFile`] (204).
    ///
    /// [`myTran_DeleteFile`]: hotline_proto::Opcode::DeleteFile
    pub async fn delete_file(&mut self, client: &HotlineClient, name: &str) {
        match client.delete_file(&self.file_path, name).await {
            Ok(()) => self.refresh_files(client).await,
            Err(e) => self.push(format!("failed to delete {name}: {e}"), LineKind::Error),
        }
    }

    /// Apply `edit` with `text` to the file or folder `name` in the current
    /// folder. A folder in `text` is `/`-separated from the root.
    pub async fn edit_file(
        &mut self,
        client: &HotlineClient,
        edit: FileEdit,
        name: &str,
        text: &str,
    ) {
        let path = &self.file_path;
        let done = match edit {
            FileEdit::NewFolder => client.new_folder(path, text).await,
            FileEdit::Rename => client.set_file_info(path, name, text, None).await,
            FileEdit::Comment => client.set_file_info(path, name, "", Some(text)).await,
            FileEdit::Move => client.move_file(path, name, &parse_folder(text)).await,
            FileEdit::Alias => {
                client
                    .make_file_alias(path, name, &parse_folder(text))
                    .await
            }
        };
        match done {
            Ok(()) => self.refresh_files(client).await,
            Err(e) => {
                let name = if name.is_empty() { text } else { name };
                self.push(
                    format!("failed to {} {name}: {e}", edit.verb()),
                    LineKind::Error,
                );
            }
        }
    }

    /// List the current folder again.
    async fn refresh_files(&mut self, client: &HotlineClient) {
        let path = self.file_path.clone();
        self.get_file_name_list(client, path).await;
    }
}

/// The folder at the `/`-separated `text`.
fn parse_folder(text: &str) -> HlPath {
    HlPath::new(
        text.split('/')
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support;

    #[test]
    fn parse_folder_skips_empty_names() {
        assert_eq!(parse_folder("/"), HlPath::default());
        assert_eq!(
            parse_folder("/Games//Doom/"),
            HlPath::new(["Games", "Doom"])
        );
    }

    #[tokio::test]
    async fn edit_file_lists_the_folder_again() {
        let files = hotline_server::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"a");
        let providers = hotline_server::Providers {
            files: std::sync::Arc::new(files),
            ..hotline_server::Providers::default()
        };
        let addr = test_support::start_server(providers).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());
        let names = |session: &ClientSession| -> Vec<String> {
            let files = session.files.as_deref().unwrap_or_default();
            files.iter().map(|f| f.name.to_string()).collect()
        };

        session
            .edit_file(&client, FileEdit::NewFolder, "", "Docs")
            .await;
        assert!(names(&session).contains(&"Docs".to_string()));
        session
            .edit_file(&client, FileEdit::Move, "a.txt", "/Docs")
            .await;
        assert_eq!(names(&session), ["Docs"]);
        session.delete_file(&client, "Docs").await;
        assert_eq!(names(&session), Vec::<String>::new());
        assert_eq!(session.folder_text(), "/");
    }
}
