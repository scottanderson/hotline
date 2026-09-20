//! File listing, transfer setup, and file/folder maintenance.

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::htxf;
use hotline_proto::types::{self, HlDate, HlPath};
use hotline_proto::FieldId;
use hotline_proto::{Field, Transaction};

use crate::account::AccessExtra;
use crate::file::FileRequester;
use crate::state::{
    FileTransfer, PendingTransfer, SharedState, TransferDirection, TransferKind, TransferRef,
    TransferTarget,
};

use super::connection::Session;

impl Session {
    /// 200
    pub(super) async fn on_get_file_name_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::FILE_LIST) {
            return vec![tx.reply_error(reason)];
        }

        // FILE_PATH (202) is optional in the request; root is assumed
        // when absent.
        let encoding = self.encoding();
        let dir = match tx.file_path(FieldId::FilePath, encoding) {
            Some(Ok(dir)) => dir,
            Some(Err(_)) => return vec![tx.reply_error("bad path")],
            None => HlPath::default(),
        };

        let requester = FileRequester { login: &self.login };
        let Some(entries) = state.list_files(requester, &dir, self.access).await else {
            return vec![tx.reply_error("file or folder not found.")];
        };
        let fields = entries
            .into_iter()
            .map(|entry| Field::bytes(FieldId::FileList, entry.encode(encoding)))
            .collect();
        vec![tx.reply(fields)]
    }

    /// Decode a `FILE_PATH`-shaped field (`FilePath` or `FileNewPath`).
    /// An absent field is the root directory.
    pub(super) fn path_field(
        &self,
        tx: &Transaction,
        id: FieldId,
    ) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx
            .field(id)
            .map(|field| HlPath::decode(&field.as_bytes(encoding), encoding))
        {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("malformed file path"),
            None => Ok(HlPath::default()),
        }
    }

    /// A request's `FILE_NAME` (201) and `FILE_PATH` (202) fields.
    pub(super) fn file_name_and_dir(
        &self,
        tx: &Transaction,
    ) -> std::result::Result<(String, HlPath), &'static str> {
        let encoding = self.encoding();
        let name = tx
            .string(FieldId::FileName, encoding)
            .ok_or("missing file name")?;
        let dir = self.path_field(tx, FieldId::FilePath)?;
        Ok((name, dir))
    }

    /// Like `file_name_and_dir`, but when `FILE_NAME` (201) is absent,
    /// derives the target name and containing directory from `FILE_PATH`
    /// (202)'s last component instead of erroring.
    pub(super) fn file_name_and_dir_from_path_tail(
        &self,
        tx: &Transaction,
    ) -> std::result::Result<(String, HlPath), &'static str> {
        if tx.field(FieldId::FileName).is_some() {
            return self.file_name_and_dir(tx);
        }
        let path = self.path_field(tx, FieldId::FilePath)?;
        let (name, dir) = path
            .components
            .split_last()
            .ok_or("missing file name or path")?;
        Ok((name.clone(), HlPath::new(dir.to_vec())))
    }

    /// 202: start a download. Allocates a transfer reference and tells
    /// the client the total byte count (flattened-object overhead plus
    /// the file's own bytes) to expect on the data connection. A client
    /// resuming an interrupted download sends `myField_FileResumeData` (203) with
    /// the byte count it already has; that portion of the data fork is
    /// skipped, though the flattened-object header is always resent.
    pub(super) async fn on_file_get(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DOWNLOAD_FILE) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let data_size = match state.file_size(requester, &dir, &name, self.access).await {
            Ok(size) => size,
            Err(reason) => return vec![tx.reply_error(reason)],
        };

        // `FileXferOptions` (204) == 2 requests the raw file bytes with
        // no resume support.
        let raw = tx.field(FieldId::FileXferOptions).and_then(Field::as_u16) == Some(2);
        let encoding = self.encoding();
        let (resume_offset, total_size) = if raw {
            (0, data_size)
        } else {
            let resume_offset = match tx.field(FieldId::ResumeFileTransfer) {
                Some(field) => match types::decode_resume_data(&field.as_bytes(encoding)) {
                    Ok(already_have) => (already_have as u64).min(data_size),
                    Err(_) => return vec![tx.reply_error("Resume data is invalid.")],
                },
                None => 0,
            };
            let remaining = data_size - resume_offset;
            (resume_offset, remaining + htxf::FLAT_FILE_HEADER_LEN as u64)
        };
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(FileTransfer {
                    target: TransferTarget {
                        login: self.login.clone(),
                        dir,
                        name,
                    },
                    raw,
                }),
                direction: TransferDirection::Get,
                size: data_size,
                resume_offset,
                client_ip: self.remote_addr.ip(),
            })
            .await;

        vec![tx.reply(vec![
            Field::u32(FieldId::TransferId, reference.0),
            Field::u32(FieldId::TransferSize, total_size as u32),
            Field::u32(FieldId::FileSize, data_size as u32),
        ])]
    }

    /// 212: request the server banner image over `HTXF`, the same
    /// download-reference-and-second-connection dance as `FILE_GET`.
    /// `Err` if no image banner is configured (a URL banner has nothing
    /// to fetch this way).
    pub(super) async fn on_banner_get(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::BANNER_GET) {
            return vec![tx.reply_error(reason)];
        }
        let (size, reference) = match state.start_banner_transfer(self.remote_addr.ip()).await {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        vec![tx.reply(vec![
            Field::u32(FieldId::TransferId, reference.0),
            Field::u32(FieldId::TransferSize, size as u32),
        ])]
    }

    /// 203: start an upload. The destination must be under a folder
    /// named "Upload" or "Drop Box" unless the account can upload
    /// anywhere. `FileXferOptions` (204) == 1 resumes an interrupted
    /// upload instead of rejecting an existing file at the destination;
    /// the reply then also carries `FileResumeData` (203) telling the
    /// client how many bytes to skip.
    pub(super) async fn on_file_put(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::UPLOAD_FILE) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let resume = tx.field(FieldId::FileXferOptions).and_then(Field::as_u16) == Some(1);
        let resume_offset = match state
            .authorize_upload(requester, &dir, &name, self.access, resume)
            .await
        {
            Ok(offset) => offset,
            Err(reason) => return vec![tx.reply_error(reason)],
        };

        let size = tx
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(FileTransfer {
                    target: TransferTarget {
                        login: self.login.clone(),
                        dir,
                        name,
                    },
                    raw: false,
                }),
                direction: TransferDirection::Put,
                size: size as u64,
                resume_offset,
                client_ip: self.remote_addr.ip(),
            })
            .await;

        let mut fields = vec![Field::u32(FieldId::TransferId, reference.0)];
        if resume {
            fields.push(Field::bytes(
                FieldId::ResumeFileTransfer,
                types::encode_resume_data(resume_offset as u32).to_vec(),
            ));
        }
        vec![tx.reply(fields)]
    }

    /// 204: delete a file, or a folder and everything under it.
    pub(super) async fn on_file_delete(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state.delete_file(requester, &dir, &name, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 205: create a folder.
    pub(super) async fn on_file_mkdir(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_FOLDER) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir_from_path_tail(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state.mkdir(requester, &dir, &name, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 206: fetch a file or folder's metadata. Type/creator/creation date
    /// are omitted; nothing in this server persists real HFS metadata for
    /// them (see `HTXF`'s `INFO` fork). Comment is sent when one is stored.
    pub(super) async fn on_file_getinfo(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::FILE_GETINFO) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir_from_path_tail(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let info = match state.file_info(requester, &dir, &name, self.access).await {
            Ok(info) => info,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let comment = match state
            .file_comment(requester, &dir, &name, self.access)
            .await
        {
            Ok(comment) => comment,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let zero_date = HlDate {
            year: 0,
            milliseconds: 0,
            seconds: 0,
        }
        .encode()
        .to_vec();
        let mut fields = vec![
            Field::string(FieldId::FileName, info.name),
            Field::u32(FieldId::FileSize, info.size),
            Field::bytes(FieldId::FileDateModify, zero_date),
        ];
        if let Some(comment) = comment {
            fields.push(Field::string(FieldId::FileComment, comment));
        }
        vec![tx.reply(fields)]
    }

    /// 207: rename a file or folder, and/or set its `FILE_COMMENT`. A
    /// rename, if present, applies before the comment, so a comment sent
    /// alongside a rename lands on the renamed entry.
    pub(super) async fn on_file_setinfo(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let new_name = tx.string(FieldId::FileNewName, encoding);
        let comment = tx.string(FieldId::FileComment, encoding);
        if new_name.is_none() && comment.is_none() {
            return vec![tx.reply(vec![])];
        }
        let requester = FileRequester { login: &self.login };
        let mut name = name;
        if let Some(new_name) = &new_name {
            if let Err(reason) = state
                .rename_file(requester, &dir, &name, new_name, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
            name = new_name.clone();
        }
        if let Some(comment) = &comment {
            if let Err(reason) = state
                .set_file_comment(requester, &dir, &name, comment, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
        }
        vec![tx.reply(vec![])]
    }

    /// 208: move a file or folder into another folder.
    pub(super) async fn on_file_move(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let to_dir = match self.path_field(tx, FieldId::FileNewPath) {
            Ok(dir) => dir,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .move_file(requester, &dir, &name, &to_dir, self.access)
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// 209: create an alias pointing at another file.
    pub(super) async fn on_file_symlink(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::MAKE_ALIAS) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let link_dir = match self.path_field(tx, FieldId::FileNewPath) {
            Ok(dir) => dir,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let link_name = tx
            .string(FieldId::FileNewName, encoding)
            .unwrap_or_else(|| name.clone());
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .symlink_file(requester, &dir, &name, &link_dir, &link_name)
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Providers;
    use crate::session::test_support::*;
    use hotline_proto::opcode::Opcode;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn banner_get_denied_without_access_extra_banner_get() {
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::BANNER_GET);

        let get = Transaction::request(Opcode::BannerGet, 1, vec![]);
        let [reply] = session
            .on_banner_get(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn banner_get_replies_with_ref_and_size_and_registers_a_banner_transfer() {
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hello".to_vec(),
        }));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::BANNER_GET);

        let get = Transaction::request(Opcode::BannerGet, 1, vec![]);
        let [reply] = session
            .on_banner_get(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let reference = reply
            .field(FieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();
        let size = reply
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(size, 5);

        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.kind, TransferKind::Banner);
        assert_eq!(transfer.direction, TransferDirection::Get);
        assert_eq!(transfer.size, 5);
    }

    #[tokio::test]
    async fn banner_get_denied_when_only_a_url_banner_is_configured() {
        let state = state_with_banner(std::sync::Arc::new(crate::banner::UrlBannerProvider::new(
            "https://example.com/banner.jpg",
        )));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::BANNER_GET);

        let get = Transaction::request(Opcode::BannerGet, 1, vec![]);
        let [reply] = session
            .on_banner_get(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    fn state_with_files(files: crate::file::InMemoryFileProvider) -> SharedState {
        SharedState::new(Providers {
            accounts: std::sync::Arc::new(crate::account::InMemoryAccountProvider::default()),
            files: std::sync::Arc::new(files),
            flat_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
            threaded_news: std::sync::Arc::new(crate::news::InMemoryNewsProvider::default()),
            bans: std::sync::Arc::new(crate::ban::InMemoryBanProvider::default()),
            flood: std::sync::Arc::new(crate::flood::NoFloodPolicy),
            banner: std::sync::Arc::new(crate::banner::NoBanner),
            commands: std::sync::Arc::new(crate::console::DefaultChatCommands),
        })
    }

    #[tokio::test]
    async fn file_get_denied_without_download_files() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::empty());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_get_replies_with_ref_and_total_size() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let reference = reply
            .field(FieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();
        let total_size = reply
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, 5 + htxf::FLAT_FILE_HEADER_LEN as u32);
        let file_size = reply
            .field(FieldId::FileSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(file_size, 5);
        assert_ne!(file_size, total_size);

        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.direction, TransferDirection::Get);
        assert_eq!(transfer.size, 5);
        assert_eq!(transfer.resume_offset, 0);
    }

    #[tokio::test]
    async fn file_get_with_resume_data_shrinks_the_transfer_and_records_the_offset() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(FieldId::ResumeFileTransfer, types::encode_resume_data(6)),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, 5 + htxf::FLAT_FILE_HEADER_LEN as u32);
        let file_size = reply
            .field(FieldId::FileSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(file_size, 11);

        let reference = reply
            .field(FieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();
        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.size, 11);
        assert_eq!(transfer.resume_offset, 6);
    }

    #[tokio::test]
    async fn file_get_with_resume_data_past_eof_clamps_to_zero_remaining() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(FieldId::ResumeFileTransfer, types::encode_resume_data(999)),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, htxf::FLAT_FILE_HEADER_LEN as u32);
    }

    #[tokio::test]
    async fn file_get_with_malformed_resume_data_is_an_error() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(FieldId::ResumeFileTransfer, vec![1, 2, 3]),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();

        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_get_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![Field::string(FieldId::FileName, "missing.txt")],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_list_of_a_dropbox_is_empty_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let list = Transaction::request(
            Opcode::FileList,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(reply.fields.is_empty());
    }

    #[tokio::test]
    async fn file_list_of_a_dropbox_is_visible_with_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let list = Transaction::request(
            Opcode::FileList,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 1);
    }

    #[tokio::test]
    async fn file_get_from_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let get = Transaction::request(
            Opcode::FileGet,
            1,
            vec![
                Field::string(FieldId::FileName, "secret.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session.on_file_get(&get, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_in_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let delete = Transaction::request(
            Opcode::FileDelete,
            1,
            vec![
                Field::string(FieldId::FileName, "secret.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::new(["Drop Box"]), "secret.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_getinfo_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let mut session = test_session("Alice", access);
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            Opcode::FileGetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "secret.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            Opcode::FileMkdir,
            1,
            vec![
                Field::string(FieldId::FileName, "Drop Box"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::default().encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "secret.txt"),
                Field::string(FieldId::FileNewName, "renamed.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_move_into_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Drop Box"]), "placeholder.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let move_tx = Transaction::request(
            Opcode::FileMove,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Drop Box"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_symlink_of_a_dropbox_target_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let symlink = Transaction::request(
            Opcode::FileSymlink,
            1,
            vec![
                Field::string(FieldId::FileName, "secret.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                ),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::default().encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn file_put_outside_a_dropbox_is_denied_without_upload_anywhere() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_put_into_a_dropbox_is_allowed_without_upload_anywhere() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    hotline_proto::types::HlPath::new(["Uploads"])
                        .encode(encoding)
                        .unwrap(),
                ),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(FieldId::TransferId).is_some());
    }

    #[tokio::test]
    async fn file_put_rejects_an_existing_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"existing");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_put_resumes_into_a_partial_upload() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(
            &HlPath::default(),
            &crate::file::partial_upload_name("a.txt"),
            *b"hello",
        );
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(!reply.is_error());
        let reference = reply
            .field(FieldId::TransferId)
            .and_then(Field::as_u32)
            .unwrap();

        let resume_field = reply.field(FieldId::ResumeFileTransfer).unwrap();
        let already_have =
            types::decode_resume_data(&resume_field.as_bytes(text_encoding::DEFAULT)).unwrap();
        assert_eq!(already_have, 5);

        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.direction, TransferDirection::Put);
        assert_eq!(transfer.resume_offset, 5);
    }

    #[tokio::test]
    async fn file_put_resume_of_a_completed_upload_is_an_error() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_put_resume_of_an_empty_partial_upload_still_sends_resume_data() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(
            &HlPath::default(),
            &crate::file::partial_upload_name("a.txt"),
            *b"",
        );
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(FieldId::ResumeFileTransfer).is_some());
    }

    #[tokio::test]
    async fn file_put_resumes_an_upload_left_partial_by_an_earlier_attempt() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());
        let requester = crate::file::FileRequester { login: "Alice" };

        // Simulate an interrupted first attempt: bytes were written but
        // the upload never finished, so it's still under its `.hpf`
        // working name.
        {
            let mut writer = state
                .create_file_for_write(requester, &HlPath::default(), "a.txt")
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut writer, b"hello")
                .await
                .unwrap();
        }

        let info = state
            .file_info(requester, &HlPath::default(), "a.txt.hpf", Access::all())
            .await
            .unwrap();
        assert_eq!(info.file_type, crate::file::PARTIAL_TYPE);
        assert_eq!(info.creator, crate::file::PARTIAL_CREATOR);

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(!reply.is_error());
        let already_have = types::decode_resume_data(
            &reply
                .field(FieldId::ResumeFileTransfer)
                .unwrap()
                .as_bytes(text_encoding::DEFAULT),
        )
        .unwrap();
        assert_eq!(already_have, 5);

        state
            .finish_upload(requester, &HlPath::default(), "a.txt")
            .await
            .unwrap();
        let info = state
            .file_info(requester, &HlPath::default(), "a.txt", Access::all())
            .await
            .unwrap();
        assert_ne!(info.file_type, crate::file::PARTIAL_TYPE);
    }

    #[tokio::test]
    async fn file_put_resume_of_a_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let put = Transaction::request(
            Opcode::FilePut,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session.on_file_put(&put, &state).await.try_into().unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_removes_a_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let delete = Transaction::request(
            Opcode::FileDelete,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            !state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_delete_denied_without_delete_files() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::DELETE_FILE);
        let session = test_session("Alice", access);

        let delete = Transaction::request(
            Opcode::FileDelete,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_delete_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let delete = Transaction::request(
            Opcode::FileDelete,
            1,
            vec![Field::string(FieldId::FileName, "missing.txt")],
        );
        let [reply] = session
            .on_file_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_creates_a_folder() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            Opcode::FileMkdir,
            1,
            vec![Field::string(FieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "NewFolder")
                .await
        );
    }

    #[tokio::test]
    async fn file_mkdir_denied_without_create_folders() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::CREATE_FOLDER);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            Opcode::FileMkdir,
            1,
            vec![Field::string(FieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_list_denied_without_access_extra() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::FILE_LIST);

        let list = Transaction::request(Opcode::FileList, 1, vec![]);
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_getinfo_denied_without_access_extra() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            Opcode::FileGetinfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_getinfo_returns_name_and_size() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            Opcode::FileGetinfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.string(FieldId::FileName, encoding).unwrap(), "a.txt");
        assert_eq!(
            reply.field(FieldId::FileSize).and_then(Field::as_u32),
            Some(5)
        );
        // No real HFS metadata is persisted, so these are omitted rather
        // than sent zeroed.
        assert!(reply.field(FieldId::FileType).is_none());
        assert!(reply.field(FieldId::FileCreator).is_none());
        assert!(reply.field(FieldId::FileDateCreate).is_none());
        assert!(reply.field(FieldId::FileComment).is_none());
    }

    #[tokio::test]
    async fn file_getinfo_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = Transaction::request(
            Opcode::FileGetinfo,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["a.txt"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.string(FieldId::FileName, encoding).unwrap(), "a.txt");
    }

    #[tokio::test]
    async fn file_setinfo_renames_a_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "b.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_setinfo_denied_without_rename_files() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::RENAME_FILE);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_without_a_new_name_is_a_no_op() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_setinfo_sets_a_comment_returned_by_getinfo() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let getinfo = Transaction::request(
            Opcode::FileGetinfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_file_getinfo(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            reply.string(FieldId::FileComment, encoding),
            Some("a note".to_string())
        );
    }

    #[tokio::test]
    async fn file_setinfo_comment_denied_without_set_file_comment() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FILE_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_setinfo_folder_comment_denied_without_set_folder_comment() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Folder"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FOLDER_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![
                Field::string(FieldId::FileName, "Folder"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_file_setinfo(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_mkdir_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            Opcode::FileMkdir,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["NewFolder"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_file_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "NewFolder")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_relocates_a_file() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Sub"]), ".keep", *b"");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = Transaction::request(
            Opcode::FileMove,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_to_root_treats_an_absent_new_path_as_root() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Sub"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = Transaction::request(
            Opcode::FileMove,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
                // No FileNewPath field: the destination is the root directory.
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn file_move_denied_without_move_files() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MOVE_FILE);
        let session = test_session("Alice", access);

        let move_tx = Transaction::request(
            Opcode::FileMove,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_move(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_symlink_denied_without_make_aliases() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MAKE_ALIAS);
        let session = test_session("Alice", access);

        let symlink = Transaction::request(
            Opcode::FileSymlink,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn file_symlink_creates_an_alias_reachable_at_the_new_location() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let symlink = Transaction::request(
            Opcode::FileSymlink,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_file_symlink(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::new(["Sub"]), "a.txt")
                .await
        );
    }
}
