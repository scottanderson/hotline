//! File and folder operations in the current folder, other than transfers.

use hotline_client::{FileInfo, HotlineClient};
use hotline_proto::types::HlPath;

use super::{ClientSession, Request};
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
    pub fn get_file_info(&self, client: &HotlineClient, name: &str) -> Request<Option<FileInfo>> {
        let client = client.clone();
        let path = self.file_path.clone();
        let name = name.to_string();
        Request::report(
            async move { client.get_file_info(&path, &name).await },
            "failed to get info",
            |_, info| Some(info),
        )
    }

    /// Delete the file or folder `name` in the current folder,
    /// [`myTran_DeleteFile`] (204).
    ///
    /// [`myTran_DeleteFile`]: hotline_proto::Opcode::DeleteFile
    pub fn delete_file(&self, client: &HotlineClient, name: &str) -> Request<()> {
        let path = self.file_path.clone();
        let name = name.to_string();
        Request::wait({
            let client = client.clone();
            async move {
                let deleted = client.delete_file(&path, &name).await;
                (deleted, name)
            }
        })
        .and_then({
            let client = client.clone();
            move |session, (deleted, name)| match deleted {
                Ok(()) => session.refresh_files(&client),
                Err(e) => {
                    session.push(format!("failed to delete {name}: {e}"), LineKind::Error);
                    Request::ready(())
                }
            }
        })
    }

    /// Apply `edit` with `text` to the file or folder `name` in the current
    /// folder. A folder in `text` is `/`-separated from the root.
    pub fn edit_file(
        &self,
        client: &HotlineClient,
        edit: FileEdit,
        name: &str,
        text: &str,
    ) -> Request<()> {
        let path = self.file_path.clone();
        let label = if name.is_empty() { text } else { name }.to_string();
        let (name, text) = (name.to_string(), text.to_string());
        Request::wait({
            let client = client.clone();
            async move {
                match edit {
                    FileEdit::NewFolder => client.new_folder(&path, &text).await,
                    FileEdit::Rename => client.set_file_info(&path, &name, &text, None).await,
                    FileEdit::Comment => client.set_file_info(&path, &name, "", Some(&text)).await,
                    FileEdit::Move => client.move_file(&path, &name, &parse_folder(&text)).await,
                    FileEdit::Alias => {
                        client
                            .make_file_alias(&path, &name, &parse_folder(&text))
                            .await
                    }
                }
            }
        })
        .and_then({
            let client = client.clone();
            move |session, done| match done {
                Ok(()) => session.refresh_files(&client),
                Err(e) => {
                    session.push(
                        format!("failed to {} {label}: {e}", edit.verb()),
                        LineKind::Error,
                    );
                    Request::ready(())
                }
            }
        })
    }

    /// List the current folder again.
    fn refresh_files(&self, client: &HotlineClient) -> Request<()> {
        Self::get_file_name_list(client, self.file_path.clone()).map(|_, _| ())
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
        let mut session = ClientSession::new("me", 0, client.encoding());
        let names = |session: &ClientSession| -> Vec<String> {
            let files = session.files.as_deref().unwrap_or_default();
            files.iter().map(|f| f.name.to_string()).collect()
        };

        session
            .edit_file(&client, FileEdit::NewFolder, "", "Docs")
            .run(&mut session)
            .await;
        assert!(names(&session).contains(&"Docs".to_string()));
        session
            .edit_file(&client, FileEdit::Move, "a.txt", "/Docs")
            .run(&mut session)
            .await;
        assert_eq!(names(&session), ["Docs"]);
        session.delete_file(&client, "Docs").run(&mut session).await;
        assert_eq!(names(&session), Vec::<String>::new());
        assert_eq!(session.folder_text(), "/");
    }
}
