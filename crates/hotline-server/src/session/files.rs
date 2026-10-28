//! File listing, transfer setup, and file/folder maintenance.

use std::sync::Arc;
use std::time::SystemTime;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::htxf::{self, ResumedFork};
use hotline_proto::types::{self, HlPath};
use hotline_proto::FieldId;
use hotline_proto::{Field, ForkHeader, Received, Transaction};

use crate::account::AccessExtra;
use crate::activity::Activity;
use crate::file::{creator_name, type_name, FileRequester};
use crate::state::{
    FileTransfer, FolderTransfer, PendingTransfer, SharedState, TransferDirection, TransferKind,
    TransferRef, TransferTarget,
};

use super::connection::Session;

impl Session {
    /// [`myTran_GetFileNameList`] (200)
    ///
    /// [`myTran_GetFileNameList`]: hotline_proto::Opcode::GetFileNameList
    pub(super) async fn on_get_file_name_list(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::FILE_LIST) {
            return vec![tx.reply_error(reason)];
        }

        // `myField_FilePath` (202) is optional in the request; root is assumed
        // when absent.
        let encoding = self.encoding();
        let dir = match tx.file_path(FieldId::FilePath) {
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
            .map(|entry| Field::bytes(FieldId::FileNameWithInfo, entry.encode(encoding)))
            .collect();
        vec![tx.reply(fields)]
    }

    /// Decode a path field, [`myField_FilePath`] (202) or [`myField_FileNewPath`]
    /// (212).
    /// An absent field is the root directory.
    ///
    /// [`myField_FilePath`]: hotline_proto::FieldId::FilePath
    /// [`myField_FileNewPath`]: hotline_proto::FieldId::FileNewPath
    pub(super) fn path_field(
        &self,
        tx: &Transaction<Received>,
        id: FieldId,
    ) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx
            .field(id)
            .map(|field| HlPath::decode(field.as_bytes(), encoding))
        {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("malformed file path"),
            None => Ok(HlPath::default()),
        }
    }

    /// A request's [`myField_FileName`] (201) and [`myField_FilePath`] (202)
    /// fields.
    ///
    /// [`myField_FileName`]: hotline_proto::FieldId::FileName
    /// [`myField_FilePath`]: hotline_proto::FieldId::FilePath
    pub(super) fn file_name_and_dir(
        &self,
        tx: &Transaction<Received>,
    ) -> std::result::Result<(String, HlPath), &'static str> {
        let name = tx.string(FieldId::FileName).ok_or("missing file name")?;
        let dir = self.path_field(tx, FieldId::FilePath)?;
        Ok((name, dir))
    }

    /// Like `file_name_and_dir`, but when [`myField_FileName`] (201) is
    /// absent, derives the target name and containing directory from
    /// [`myField_FilePath`] (202)'s last component instead of erroring.
    ///
    /// [`myField_FileName`]: hotline_proto::FieldId::FileName
    /// [`myField_FilePath`]: hotline_proto::FieldId::FilePath
    pub(super) fn file_name_and_dir_from_path_tail(
        &self,
        tx: &Transaction<Received>,
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

    /// [`myTran_DownloadFile`] (202): start a download. Allocates a transfer
    /// reference and tells the client the total byte count (flattened-object
    /// overhead plus the data and resource forks) to expect on the data
    /// connection. A client resuming an interrupted download sends
    /// [`myField_FileResumeData`] (203) with the byte count of each fork it
    /// already has; those bytes are skipped, though the flattened-object header
    /// is always resent. A raw request for an empty file gets only a zero
    /// [`myField_TransferSize`] (108).
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    /// [`myField_TransferSize`]: hotline_proto::FieldId::TransferSize
    /// [`myField_FileResumeData`]: hotline_proto::FieldId::FileResumeData
    pub(super) async fn on_download_file(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
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

        // `myField_FileXferOptions` (204) == 2 requests the raw file bytes with
        // no resume support.
        let raw = tx.field(FieldId::FileXferOptions).and_then(Field::as_u32) == Some(2);
        if raw && data_size == 0 {
            return vec![tx.reply(vec![Field::u32(FieldId::TransferSize, 0)])];
        }
        let info = if raw {
            Vec::new()
        } else {
            state
                .info_fork(requester, &dir, &name, self.encoding())
                .await
        };
        let resource_size = if raw {
            0
        } else {
            state.resource_fork_size(requester, &dir, &name).await
        };
        let (data, resource) = match tx.field(FieldId::FileResumeData) {
            Some(field) if !raw => match types::ResumeData::decode(field.as_bytes()) {
                Ok(resume) => (
                    ResumedFork::new(data_size, resume.data.into()),
                    ResumedFork::new(resource_size, resume.resource.into()),
                ),
                Err(_) => return vec![tx.reply_error("Resume data is invalid.")],
            },
            _ => (
                ResumedFork::whole(data_size),
                ResumedFork::whole(resource_size),
            ),
        };
        let total_size = if raw {
            data_size
        } else {
            let header_len = htxf::flat_file_header_len(info.len()) + ForkHeader::LEN;
            header_len as u64 + data.len + resource.len
        };
        let kind = TransferKind::File(FileTransfer {
            target: TransferTarget {
                login: self.login.clone(),
                dir,
                name: name.as_str().into(),
            },
            raw,
            encoding: self.encoding(),
            info,
            resource_size,
            resource_offset: resource.offset,
        });
        let mut fields = vec![
            Field::u32(FieldId::TransferSize, total_size as u32),
            Field::u32(FieldId::FileSize, data_size as u32),
        ];
        fields.extend(
            self.register_download(kind, data_size, data.offset, state)
                .await,
        );
        self.record_activity(Activity::Download { name: name.into() }, state)
            .await;
        vec![tx.reply(fields)]
    }

    /// Register and admit a download. Returns its [`myField_RefNum`] (107)
    /// field, then [`myField_WaitingCount`] (116) if it is queued.
    ///
    /// [`myField_RefNum`]: hotline_proto::FieldId::RefNum
    /// [`myField_WaitingCount`]: hotline_proto::FieldId::WaitingCount
    async fn register_download(
        &self,
        kind: TransferKind,
        size: u64,
        resume_offset: u64,
        state: &Arc<SharedState>,
    ) -> Vec<Field> {
        let reference = state
            .register_transfer(PendingTransfer {
                kind,
                direction: TransferDirection::Get,
                size,
                resume_offset,
                client_ip: self.remote_addr.ip(),
                user_id: self.user_id,
            })
            .await;
        let waiting_count = state.admit_download(reference, self.user_id).await;
        let mut fields = vec![Field::u32(FieldId::RefNum, reference.0)];
        if waiting_count > 0 {
            fields.push(Field::u32(FieldId::WaitingCount, waiting_count));
        }
        fields
    }

    /// [`myTran_DownloadBanner`] (212): request the server banner image over
    /// `HTXF`, the same download-reference-and-second-connection dance as
    /// [`myTran_DownloadFile`] (202). `Err` if no image banner is configured (a
    /// URL banner has nothing to fetch this way).
    ///
    /// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    pub(super) async fn on_download_banner(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access_extra.require(AccessExtra::BANNER_GET) {
            return vec![tx.reply_error(reason)];
        }
        let (size, reference) = match state
            .start_banner_transfer(self.remote_addr.ip(), self.user_id)
            .await
        {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        vec![tx.reply(vec![
            Field::u32(FieldId::RefNum, reference.0),
            Field::u32(FieldId::TransferSize, size as u32),
        ])]
    }

    /// [`myTran_UploadFile`] (203): start an upload. The destination must be
    /// under a folder named "Upload" or "Drop Box" unless the account can
    /// upload anywhere. [`myField_FileXferOptions`] (204) == 1 resumes an
    /// interrupted upload instead of rejecting an existing file at the
    /// destination; the reply then also carries [`myField_FileResumeData`] (203)
    /// telling the client how many bytes of each fork to skip.
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    /// [`myField_FileXferOptions`]: hotline_proto::FieldId::FileXferOptions
    /// [`myField_FileResumeData`]: hotline_proto::FieldId::FileResumeData
    pub(super) async fn on_upload_file(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::UPLOAD_FILE) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let name = match state.stored_file_name(&name) {
            Ok(name) => name,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let resume = tx.field(FieldId::FileXferOptions).and_then(Field::as_u32) == Some(1);
        let size = tx
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        let resume_offset = match state
            .authorize_upload(requester, &dir, &name, self.access, resume, size.into())
            .await
        {
            Ok(offset) => offset,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let resource_offset = if resume {
            state
                .partial_resource_fork_size(requester, &dir, &name)
                .await
        } else {
            0
        };

        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::File(FileTransfer {
                    target: TransferTarget {
                        login: self.login.clone(),
                        dir,
                        name: name.as_str().into(),
                    },
                    raw: false,
                    encoding: self.encoding(),
                    info: Vec::new(),
                    resource_size: 0,
                    resource_offset,
                }),
                direction: TransferDirection::Put,
                size: size as u64,
                resume_offset,
                client_ip: self.remote_addr.ip(),
                user_id: self.user_id,
            })
            .await;

        let mut fields = vec![Field::u32(FieldId::RefNum, reference.0)];
        if resume {
            fields.push(Field::bytes(
                FieldId::FileResumeData,
                types::ResumeData {
                    data: resume_offset as u32,
                    resource: resource_offset as u32,
                }
                .encode()
                .to_vec(),
            ));
        }
        self.record_activity(Activity::Upload { name: name.into() }, state)
            .await;
        vec![tx.reply(fields)]
    }

    /// [`myTran_DownloadFldr`] (210): start a folder download. Replies with the
    /// total byte count of the folder's files and its item count. An empty
    /// folder gets no transfer reference.
    ///
    /// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
    pub(super) async fn on_download_fldr(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::DOWNLOAD_FOLDER) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let items = match state.walk_folder(requester, &dir, &name, self.access).await {
            Ok(items) => items,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let total_size: u64 = items.iter().map(|item| item.size).sum();
        let item_count = items.len() as u32;
        let mut fields = vec![
            Field::u32(FieldId::TransferSize, total_size as u32),
            Field::u32(FieldId::FldrItemCount, item_count),
        ];
        if items.is_empty() {
            return vec![tx.reply(fields)];
        }

        let kind = TransferKind::Folder(FolderTransfer {
            target: TransferTarget {
                login: self.login.clone(),
                dir,
                name: name.as_str().into(),
            },
            items,
            item_count,
            encoding: self.encoding(),
        });
        fields.extend(self.register_download(kind, total_size, 0, state).await);
        self.record_activity(Activity::Download { name: name.into() }, state)
            .await;
        vec![tx.reply(fields)]
    }

    /// [`myTran_UploadFldr`] (213): start a folder upload, creating the folder
    /// right away. The destination rule matches [`myTran_UploadFile`] (203).
    /// [`myField_FileXferOptions`] (204) == 1 resumes into an existing folder
    /// instead. A zero [`myField_FldrItemCount`] (220) gets no transfer
    /// reference.
    ///
    /// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    /// [`myField_FileXferOptions`]: hotline_proto::FieldId::FileXferOptions
    /// [`myField_FldrItemCount`]: hotline_proto::FieldId::FldrItemCount
    pub(super) async fn on_upload_fldr(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::UPLOAD_FOLDER) {
            return vec![tx.reply_error(reason)];
        }

        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let name = match state.stored_file_name(&name) {
            Ok(name) => name,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        let resume = tx.field(FieldId::FileXferOptions).and_then(Field::as_u32) == Some(1);
        let size = tx
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        if let Err(reason) = state
            .start_folder_upload(requester, &dir, &name, self.access, resume, size.into())
            .await
        {
            return vec![tx.reply_error(reason)];
        }

        let item_count = tx
            .field(FieldId::FldrItemCount)
            .and_then(Field::as_u32)
            .unwrap_or(0);
        if item_count == 0 {
            self.record_activity(Activity::Upload { name: name.into() }, state)
                .await;
            return vec![tx.reply(vec![])];
        }
        let reference = state
            .register_transfer(PendingTransfer {
                kind: TransferKind::Folder(FolderTransfer {
                    target: TransferTarget {
                        login: self.login.clone(),
                        dir,
                        name: name.as_str().into(),
                    },
                    items: Vec::new(),
                    item_count,
                    encoding: self.encoding(),
                }),
                direction: TransferDirection::Put,
                size: size as u64,
                resume_offset: 0,
                client_ip: self.remote_addr.ip(),
                user_id: self.user_id,
            })
            .await;
        self.record_activity(Activity::Upload { name: name.into() }, state)
            .await;
        vec![tx.reply(vec![Field::u32(FieldId::RefNum, reference.0)])]
    }

    /// [`myTran_KillDownload`] (214): cancel one of this session's downloads,
    /// queued or running.
    ///
    /// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
    pub(super) async fn on_kill_download(
        &self,
        tx: &Transaction<Received>,
        state: &Arc<SharedState>,
    ) -> Vec<Transaction> {
        if let Some(reference) = tx
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .map(TransferRef)
        {
            state.kill_download(reference, self.user_id).await;
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_DeleteFile`] (204): delete a file, or a folder and everything
    /// under it.
    ///
    /// [`myTran_DeleteFile`]: hotline_proto::Opcode::DeleteFile
    pub(super) async fn on_delete_file(
        &self,
        tx: &Transaction<Received>,
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

    /// [`myTran_NewFolder`] (205): create a folder.
    ///
    /// [`myTran_NewFolder`]: hotline_proto::Opcode::NewFolder
    pub(super) async fn on_new_folder(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::CREATE_FOLDER) {
            return vec![tx.reply_error(reason)];
        }
        let (name, dir) = match self.file_name_and_dir_from_path_tail(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let name = match state.stored_file_name(&name) {
            Ok(name) => name,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state.mkdir(requester, &dir, &name, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_GetFileInfo`] (206): fetch a file or folder's metadata. A
    /// type or creator code with a display name sends the name, and any
    /// other sends the code. Only a file sends its size. Comment is sent
    /// when one is stored.
    ///
    /// [`myTran_GetFileInfo`]: hotline_proto::Opcode::GetFileInfo
    pub(super) async fn on_get_file_info(
        &self,
        tx: &Transaction<Received>,
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
        let times = state.file_times(requester, &dir, &name).await;
        let date = |time: Option<SystemTime>| state.wire_date(time).encode().to_vec();
        let mut fields = if info.is_folder() {
            vec![
                Field::string(FieldId::FileTypeString, "Folder"),
                Field::string(FieldId::FileCreatorString, "n/a"),
                Field::bytes(FieldId::FileType, info.file_type),
            ]
        } else {
            vec![
                Field::bytes(FieldId::FileType, info.file_type),
                code_field(FieldId::FileTypeString, info.file_type, type_name),
                code_field(FieldId::FileCreatorString, info.creator, creator_name),
                Field::u32(FieldId::FileSize, info.size),
            ]
        };
        fields.push(Field::string(FieldId::FileName, info.name));
        if times.created.is_some() {
            fields.push(Field::bytes(FieldId::FileCreateDate, date(times.created)));
        }
        fields.push(Field::bytes(FieldId::FileModifyDate, date(times.modified)));
        if let Some(comment) = comment {
            fields.push(Field::string(FieldId::FileComment, comment));
        }
        vec![tx.reply(fields)]
    }

    /// [`myTran_SetFileInfo`] (207): set a file or folder's
    /// [`myField_FileComment`] (210), then rename it. A comment sent with a
    /// refused rename is still set.
    ///
    /// [`myTran_SetFileInfo`]: hotline_proto::Opcode::SetFileInfo
    /// [`myField_FileComment`]: hotline_proto::FieldId::FileComment
    pub(super) async fn on_set_file_info(
        &self,
        tx: &Transaction<Received>,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let (name, dir) = match self.file_name_and_dir(tx) {
            Ok(result) => result,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let new_name = match tx
            .string(FieldId::FileNewName)
            .filter(|new_name| !new_name.is_empty() && *new_name != name)
            .map(|new_name| state.stored_file_name(&new_name))
            .transpose()
        {
            Ok(new_name) => new_name,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        // A comment of one zero byte clears the comment.
        let comment = tx.string(FieldId::FileComment).map(|comment| {
            if comment == "\0" {
                String::new()
            } else {
                comment
            }
        });
        if new_name.is_none() && comment.is_none() {
            return vec![tx.reply(vec![])];
        }
        let requester = FileRequester { login: &self.login };
        if let Some(comment) = &comment {
            if let Err(reason) = state
                .set_file_comment(requester, &dir, &name, comment, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
        }
        if let Some(new_name) = &new_name {
            if let Err(reason) = state
                .rename_file(requester, &dir, &name, new_name, self.access)
                .await
            {
                return vec![tx.reply_error(reason)];
            }
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_MoveFile`] (208): move a file or folder into another folder.
    ///
    /// [`myTran_MoveFile`]: hotline_proto::Opcode::MoveFile
    pub(super) async fn on_move_file(
        &self,
        tx: &Transaction<Received>,
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

    /// [`myTran_MakeFileAlias`] (209): create an alias pointing at another file.
    ///
    /// [`myTran_MakeFileAlias`]: hotline_proto::Opcode::MakeFileAlias
    pub(super) async fn on_make_file_alias(
        &self,
        tx: &Transaction<Received>,
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
        let link_name = tx
            .string(FieldId::FileNewName)
            .unwrap_or_else(|| name.clone());
        let link_name = match state.stored_file_name(&link_name) {
            Ok(link_name) => link_name,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let requester = FileRequester { login: &self.login };
        if let Err(reason) = state
            .symlink_file(requester, &dir, &name, &link_dir, &link_name, self.access)
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }
}

/// A field with the display name of `code`, or with `code` itself when it
/// has none.
fn code_field(id: FieldId, code: [u8; 4], name: fn([u8; 4]) -> Option<&'static str>) -> Field {
    name(code).map_or_else(|| Field::bytes(id, code), |name| Field::string(id, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::providers::Providers;
    use crate::session::test_support::*;
    use hotline_proto::opcode::Opcode;
    use hotline_proto::text_encoding;

    #[tokio::test]
    async fn download_banner_denied_without_access_extra_banner_get() {
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hi".to_vec(),
        }));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::BANNER_GET);

        let get = request(Opcode::DownloadBanner, 1, vec![]);
        let [reply] = session
            .on_download_banner(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn download_banner_replies_with_ref_and_size_and_registers_a_banner_transfer() {
        let state = state_with_banner(std::sync::Arc::new(TestImageBanner {
            format: *b"JPEG",
            bytes: b"hello".to_vec(),
        }));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::BANNER_GET);

        let get = request(Opcode::DownloadBanner, 1, vec![]);
        let [reply] = session
            .on_download_banner(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let reference = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();
        let size = reply
            .received(text_encoding::DEFAULT)
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
    async fn download_banner_denied_when_only_a_url_banner_is_configured() {
        let state = state_with_banner(std::sync::Arc::new(crate::banner::UrlBannerProvider::new(
            "https://example.com/banner.jpg",
        )));
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::BANNER_GET);

        let get = request(Opcode::DownloadBanner, 1, vec![]);
        let [reply] = session
            .on_download_banner(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    fn state_with_files(files: crate::file::InMemoryFileProvider) -> Arc<SharedState> {
        SharedState::new(Providers {
            files: std::sync::Arc::new(files),
            ..Providers::default()
        })
    }

    /// The bytes of a download of `a.txt` with no comment, other than its forks.
    const A_TXT_HEADER_LEN: u32 =
        (htxf::FLAT_FILE_HEADER_LEN + "a.txt".len() + ForkHeader::LEN) as u32;

    fn state_with_files_and_policy(
        files: crate::file::InMemoryFileProvider,
        policy: impl crate::ServerPolicy + 'static,
    ) -> Arc<SharedState> {
        SharedState::new(Providers {
            files: std::sync::Arc::new(files),
            policy: std::sync::Arc::new(policy),
            ..Providers::default()
        })
    }

    struct CheckDropBoxes;

    impl crate::ServerPolicy for CheckDropBoxes {
        fn error(&self, _case: crate::Silent) -> Option<std::borrow::Cow<'static, str>> {
            Some("refused".into())
        }
    }

    #[tokio::test]
    async fn download_file_denied_without_download_files() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::empty());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn download_file_replies_with_ref_and_total_size() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        let reference = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();
        let total_size = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, 5 + A_TXT_HEADER_LEN);
        let file_size = reply
            .received(text_encoding::DEFAULT)
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
    async fn download_file_sends_the_finder_info_and_the_comment() {
        use crate::file::FileProvider;

        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let requester = FileRequester { login: "Alice" };
        let finder_info = crate::file::FinderInfo {
            file_type: *b"ttro",
            creator: *b"ttxt",
            flags: 0x0400,
        };
        files
            .set_finder_info(requester, &HlPath::default(), "a.txt", finder_info)
            .await
            .unwrap();
        files
            .set_comment(requester, &HlPath::default(), "a.txt", "a note")
            .await
            .unwrap();
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        let reply = reply.received(text_encoding::DEFAULT);
        let total_size = reply.field(FieldId::TransferSize).and_then(Field::as_u32);
        assert_eq!(total_size, Some(5 + A_TXT_HEADER_LEN + 6));
        let reference = reply.field(FieldId::RefNum).and_then(Field::as_u32);
        let transfer = state
            .take_transfer(TransferRef(reference.unwrap()))
            .await
            .unwrap();
        let TransferKind::File(file) = transfer.kind else {
            panic!("expected a file transfer");
        };
        let info = htxf::FlatFileInfo::decode(&file.info, text_encoding::DEFAULT);
        assert_eq!(&info.file_type, b"ttro");
        assert_eq!(&info.creator, b"ttxt");
        assert_eq!(info.platform_flags, 0x0400);
        assert_eq!(info.name, "a.txt");
        assert_eq!(info.comment, "a note");
    }

    #[tokio::test]
    async fn download_file_reply_carries_no_waiting_count_under_the_download_cap() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.field(FieldId::WaitingCount).is_none());
    }

    /// openline's `ProcessTran_KillDownload` always replies with no error.
    #[tokio::test]
    async fn kill_download_cancels_this_sessions_download() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());
        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        let reference = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();

        let kill = request(
            Opcode::KillDownload,
            2,
            vec![Field::u32(FieldId::RefNum, reference)],
        );
        let [reply] = session
            .on_kill_download(&kill, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        assert!(state.take_transfer(TransferRef(reference)).await.is_none());
    }

    #[tokio::test]
    async fn download_file_past_the_download_cap_queues_and_reports_waiting_count() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        files.seed(&HlPath::default(), "b.txt", *b"world");
        let state = state_with_files(files);
        state.set_download_limits(1, 10);
        let mut first_session = test_session("Alice", Access::all());
        first_session.user_id = 1;
        let mut second_session = test_session("Bob", Access::all());
        second_session.user_id = 2;

        let first_get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [first_reply] = first_session
            .on_download_file(&first_get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(first_reply.field(FieldId::WaitingCount).is_none());

        let second_get = request(
            Opcode::DownloadFile,
            2,
            vec![Field::string(FieldId::FileName, "b.txt")],
        );
        let [second_reply] = second_session
            .on_download_file(&second_get, &state)
            .await
            .try_into()
            .unwrap();
        let waiting_count = second_reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::WaitingCount)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(waiting_count, 1);

        let mut second_recipient = state.register_recipient(2).await;

        let first_reference = first_reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();
        state
            .release_download_slot(TransferRef(first_reference))
            .await;

        let second_reference = second_reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();

        let push = match second_recipient.recv().await.unwrap() {
            crate::state::RecipientMessage::Push(tx) => tx,
            other => panic!("expected a DownloadInfo push, got {other:?}"),
        };
        assert_eq!(
            push.r#type(),
            hotline_proto::opcode::TransactionType::Push(Opcode::DownloadInfo)
        );
        assert_eq!(
            push.received(text_encoding::DEFAULT)
                .field(FieldId::RefNum)
                .and_then(Field::as_u32),
            Some(second_reference)
        );
        assert_eq!(
            push.received(text_encoding::DEFAULT)
                .field(FieldId::WaitingCount)
                .and_then(Field::as_u32),
            Some(0)
        );

        let promoted = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            state.wait_for_download_slot(TransferRef(second_reference)),
        )
        .await
        .expect("second download should have been promoted once the first slot freed");
        assert!(promoted);
    }

    #[tokio::test]
    async fn download_file_with_resume_data_shrinks_the_transfer_and_records_the_offset() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileResumeData,
                    types::ResumeData {
                        data: 6,
                        resource: 0,
                    }
                    .encode()
                    .to_vec(),
                ),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, 5 + A_TXT_HEADER_LEN);
        let file_size = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::FileSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(file_size, 11);

        let reference = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();
        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.size, 11);
        assert_eq!(transfer.resume_offset, 6);
    }

    #[tokio::test]
    async fn download_file_counts_and_resumes_the_resource_fork() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello world");
        files.seed_resource_fork(&HlPath::default(), "a.txt", *b"rsrc");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let resume = types::ResumeData {
            data: 6,
            resource: 1,
        };
        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(FieldId::FileResumeData, resume.encode().to_vec()),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        let reply = reply.received(text_encoding::DEFAULT);
        let total_size = reply.field(FieldId::TransferSize).and_then(Field::as_u32);
        assert_eq!(total_size, Some(5 + A_TXT_HEADER_LEN + 3));
        let reference = reply.field(FieldId::RefNum).and_then(Field::as_u32);
        let transfer = state
            .take_transfer(TransferRef(reference.unwrap()))
            .await
            .unwrap();
        let TransferKind::File(file) = transfer.kind else {
            panic!("not a file transfer");
        };
        assert_eq!((file.resource_size, file.resource_offset), (4, 1));
    }

    #[tokio::test]
    async fn download_file_with_resume_data_past_eof_clamps_to_zero_remaining() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FileResumeData,
                    types::ResumeData {
                        data: 999,
                        resource: 0,
                    }
                    .encode()
                    .to_vec(),
                ),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        assert!(!reply.is_error());
        let total_size = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::TransferSize)
            .and_then(Field::as_u32)
            .unwrap();
        assert_eq!(total_size, A_TXT_HEADER_LEN);
    }

    #[tokio::test]
    async fn download_file_with_malformed_resume_data_is_an_error() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(FieldId::FileResumeData, vec![1, 2, 3]),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();

        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn download_file_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "missing.txt")],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn get_file_name_list_of_a_dropbox_shows_only_partial_uploads_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        files.seed(&HlPath::new(["Drop Box"]), "mine.txt.hpf", *b"h");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let list = request(
            Opcode::GetFileNameList,
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
        let names: Vec<_> = reply
            .received(encoding)
            .fields
            .iter()
            .map(|f| {
                hotline_proto::types::FileNameWithInfo::decode(f.as_bytes(), encoding)
                    .unwrap()
                    .name
            })
            .collect();
        assert_eq!(names, [Box::from("mine.txt.hpf")]);
    }

    #[tokio::test]
    async fn file_list_of_a_dropbox_is_visible_with_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let list = request(
            Opcode::GetFileNameList,
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
    async fn download_file_from_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let get = request(
            Opcode::DownloadFile,
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
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn delete_file_in_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let delete = request(
            Opcode::DeleteFile,
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
            .on_delete_file(&delete, &state)
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
    async fn move_file_out_of_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files_and_policy(files, CheckDropBoxes);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let r#move = request(
            Opcode::MoveFile,
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
            .on_move_file(&r#move, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Drop Box"]), "secret.txt")
                .await
        );
    }

    #[tokio::test]
    async fn make_file_alias_of_a_dropbox_file_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files_and_policy(files, CheckDropBoxes);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let alias = request(
            Opcode::MakeFileAlias,
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
            .on_make_file_alias(&alias, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), "secret.txt")
                .await
        );
    }

    #[tokio::test]
    async fn move_file_and_make_file_alias_of_a_dropbox_file_succeed_by_default() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Drop Box"]), "b.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);
        let request_for = |opcode, name: &str| {
            request(
                opcode,
                1,
                vec![
                    Field::string(FieldId::FileName, name),
                    Field::bytes(
                        FieldId::FilePath,
                        HlPath::new(["Drop Box"]).encode(encoding).unwrap(),
                    ),
                    Field::bytes(
                        FieldId::FileNewPath,
                        HlPath::default().encode(encoding).unwrap(),
                    ),
                ],
            )
        };

        let moved = session
            .on_move_file(&request_for(Opcode::MoveFile, "a.txt"), &state)
            .await;
        let aliased = session
            .on_make_file_alias(&request_for(Opcode::MakeFileAlias, "b.txt"), &state)
            .await;
        assert!(!moved[0].is_error());
        assert!(!aliased[0].is_error());
        let requester = FileRequester { login: "" };
        let root = HlPath::default();
        assert!(state.file_exists(requester, &root, "a.txt").await);
        assert!(state.file_exists(requester, &root, "b.txt").await);
    }

    #[tokio::test]
    async fn get_file_info_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let mut session = test_session("Alice", access);
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
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
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn new_folder_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let mkdir = request(
            Opcode::NewFolder,
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
            .on_new_folder(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn set_file_info_in_a_dropbox_is_denied_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Drop Box"]), "secret.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let setinfo = request(
            Opcode::SetFileInfo,
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
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn move_file_into_a_dropbox_succeeds_without_view_drop_boxes() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Drop Box"]), "placeholder.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::VIEW_DROP_BOXES);
        let session = test_session("Alice", access);

        let move_tx = request(
            Opcode::MoveFile,
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
            .on_move_file(&move_tx, &state)
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

    fn folder_request(
        opcode: Opcode,
        dir: &[&str],
        name: &str,
        extra: Vec<Field>,
    ) -> Transaction<Received> {
        let encoding = text_encoding::DEFAULT;
        let mut fields = vec![
            Field::string(FieldId::FileName, name),
            Field::bytes(
                FieldId::FilePath,
                HlPath::new(dir.iter().copied()).encode(encoding).unwrap(),
            ),
        ];
        fields.extend(extra);
        request(opcode, 1, fields)
    }

    #[tokio::test]
    async fn download_fldr_denied_without_download_folders() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::DOWNLOAD_FOLDER);
        let session = test_session("Alice", access);

        let get = folder_request(Opcode::DownloadFldr, &[], "Top", vec![]);
        let [reply] = session
            .on_download_fldr(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn download_fldr_replies_with_size_count_and_ref_and_registers_the_items() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        files.seed(&HlPath::new(["Top", "Sub"]), "b.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = folder_request(Opcode::DownloadFldr, &[], "Top", vec![]);
        let [reply] = session
            .on_download_fldr(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let u32_field = |id| {
            reply
                .received(text_encoding::DEFAULT)
                .field(id)
                .and_then(Field::as_u32)
                .unwrap()
        };
        assert_eq!(u32_field(FieldId::TransferSize), 7);
        assert_eq!(u32_field(FieldId::FldrItemCount), 3);
        assert!(reply.field(FieldId::WaitingCount).is_none());

        let transfer = state
            .take_transfer(TransferRef(u32_field(FieldId::RefNum)))
            .await
            .unwrap();
        assert_eq!(transfer.direction, TransferDirection::Get);
        assert_eq!(transfer.size, 7);
        let TransferKind::Folder(folder) = transfer.kind else {
            panic!("expected a folder transfer");
        };
        assert_eq!(&*folder.target.name, "Top");
        assert_eq!(folder.item_count, 3);
        assert_eq!(folder.items.len(), 3);
    }

    #[tokio::test]
    async fn download_fldr_of_an_empty_folder_replies_without_a_ref() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());
        let requester = FileRequester { login: "Alice" };
        state
            .mkdir(requester, &HlPath::default(), "Empty", Access::all())
            .await
            .unwrap();

        let get = folder_request(Opcode::DownloadFldr, &[], "Empty", vec![]);
        let [reply] = session
            .on_download_fldr(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply
                .received(text_encoding::DEFAULT)
                .field(FieldId::FldrItemCount)
                .and_then(Field::as_u32),
            Some(0)
        );
        assert!(reply.field(FieldId::RefNum).is_none());
    }

    #[tokio::test]
    async fn download_file_raw_of_an_empty_file_replies_without_a_ref() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "empty.txt", *b"");
        let (state, log) = state_recording_activity(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "empty.txt"),
                Field::u16(FieldId::FileXferOptions, 2),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply
                .received(text_encoding::DEFAULT)
                .field(FieldId::TransferSize)
                .and_then(Field::as_u32),
            Some(0)
        );
        assert!(reply.field(FieldId::RefNum).is_none());
        assert!(reply.field(FieldId::FileSize).is_none());
        assert_eq!(log.activities(), []);
    }

    #[tokio::test]
    async fn download_file_reads_four_byte_transfer_options() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "empty.txt", *b"");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "empty.txt"),
                Field::u32(FieldId::FileXferOptions, 2),
            ],
        );
        let [reply] = session
            .on_download_file(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.field(FieldId::RefNum).is_none());
    }

    #[tokio::test]
    async fn download_file_records_the_download() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let (state, log) = state_recording_activity(files);
        let session = test_session("Alice", Access::all());

        let get = request(
            Opcode::DownloadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        session.on_download_file(&get, &state).await;
        let missing = request(
            Opcode::DownloadFile,
            2,
            vec![Field::string(FieldId::FileName, "b.txt")],
        );
        session.on_download_file(&missing, &state).await;

        assert_eq!(
            log.activities(),
            [Activity::Download {
                name: "a.txt".into()
            }]
        );
    }

    #[tokio::test]
    async fn download_fldr_of_an_empty_folder_records_nothing() {
        let (state, log) = state_recording_activity(Default::default());
        let session = test_session("Alice", Access::all());
        let requester = FileRequester { login: "Alice" };
        state
            .mkdir(requester, &HlPath::default(), "Empty", Access::all())
            .await
            .unwrap();

        let get = folder_request(Opcode::DownloadFldr, &[], "Empty", vec![]);
        session.on_download_fldr(&get, &state).await;
        assert_eq!(log.activities(), []);
    }

    #[tokio::test]
    async fn download_fldr_records_the_download() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        let (state, log) = state_recording_activity(files);
        let session = test_session("Alice", Access::all());

        let get = folder_request(Opcode::DownloadFldr, &[], "Top", vec![]);
        session.on_download_fldr(&get, &state).await;
        assert_eq!(
            log.activities(),
            [Activity::Download { name: "Top".into() }]
        );
    }

    #[tokio::test]
    async fn upload_file_records_the_upload() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "taken.txt", *b"hello");
        let (state, log) = state_recording_activity(files);
        let session = test_session("Alice", Access::all());

        let put = |name| {
            request(
                Opcode::UploadFile,
                1,
                vec![Field::string(FieldId::FileName, name)],
            )
        };
        session.on_upload_file(&put("new.txt"), &state).await;
        let taken = session.on_upload_file(&put("taken.txt"), &state).await;
        assert!(taken[0].is_error());

        assert_eq!(
            log.activities(),
            [Activity::Upload {
                name: "new.txt".into()
            }]
        );
    }

    #[tokio::test]
    async fn download_fldr_of_a_missing_folder_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let get = folder_request(Opcode::DownloadFldr, &[], "Missing", vec![]);
        let [reply] = session
            .on_download_fldr(&get, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_fldr_denied_without_upload_folders() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_FOLDER);
        let session = test_session("Alice", access);

        let put = folder_request(Opcode::UploadFldr, &[], "New", vec![]);
        let [reply] = session
            .on_upload_fldr(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_fldr_outside_a_dropbox_is_denied_without_upload_anywhere() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = folder_request(Opcode::UploadFldr, &[], "New", vec![]);
        let [reply] = session
            .on_upload_fldr(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_fldr_creates_the_folder_and_replies_with_a_ref() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Uploads"]), "other.txt", *b"x");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);
        let requester = FileRequester { login: "Alice" };

        let put = folder_request(
            Opcode::UploadFldr,
            &["Uploads"],
            "New",
            vec![
                Field::u32(FieldId::FldrItemCount, 2),
                Field::u32(FieldId::TransferSize, 10),
            ],
        );
        let [reply] = session
            .on_upload_fldr(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let info = state
            .file_info(requester, &HlPath::new(["Uploads"]), "New", Access::all())
            .await
            .unwrap();
        assert_eq!(info.file_type, *b"fldr");

        let reference = reply
            .received(text_encoding::DEFAULT)
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();
        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.direction, TransferDirection::Put);
        assert_eq!(transfer.size, 10);
        let TransferKind::Folder(folder) = transfer.kind else {
            panic!("expected a folder transfer");
        };
        assert_eq!(folder.item_count, 2);
    }

    #[tokio::test]
    async fn upload_fldr_with_no_items_creates_the_folder_without_a_ref() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());
        let requester = FileRequester { login: "Alice" };

        let put = folder_request(Opcode::UploadFldr, &[], "New", vec![]);
        let [reply] = session
            .on_upload_fldr(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(FieldId::RefNum).is_none());
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "New")
                .await
        );
    }

    #[tokio::test]
    async fn upload_fldr_with_no_items_records_the_upload() {
        let (state, log) = state_recording_activity(Default::default());
        let session = test_session("Alice", Access::all());

        let put = folder_request(Opcode::UploadFldr, &[], "New", vec![]);
        session.on_upload_fldr(&put, &state).await;
        assert_eq!(log.activities(), [Activity::Upload { name: "New".into() }]);
    }

    #[tokio::test]
    async fn upload_fldr_rejects_an_existing_name_unless_resuming_into_a_folder() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"x");
        files.seed(&HlPath::default(), "file.txt", *b"x");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());
        let resume = || vec![Field::u16(FieldId::FileXferOptions, 1)];
        let put = |name, extra| folder_request(Opcode::UploadFldr, &[], name, extra);
        let is_error = |tx: Transaction<Received>| {
            let session = &session;
            let state = &state;
            async move {
                let [reply] = session.on_upload_fldr(&tx, state).await.try_into().unwrap();
                reply.is_error()
            }
        };

        assert!(is_error(put("Top", vec![])).await);
        assert!(!is_error(put("Top", resume())).await);
        assert!(is_error(put("file.txt", resume())).await);
        assert!(is_error(put("Missing", resume())).await);
    }

    #[tokio::test]
    async fn upload_file_outside_a_dropbox_is_denied_without_upload_anywhere() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = request(
            Opcode::UploadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_file_into_a_dropbox_is_allowed_without_upload_anywhere() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Uploads"]), "b.txt", *b"");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::UPLOAD_ANYWHERE);
        let session = test_session("Alice", access);

        let put = request(
            Opcode::UploadFile,
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
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(FieldId::RefNum).is_some());
    }

    #[tokio::test]
    async fn upload_file_needs_the_size_plus_a_margin_free() {
        let files = crate::file::InMemoryFileProvider::default();
        files.set_free_space(Some(512_100));
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());
        let put = |size| {
            request(
                Opcode::UploadFile,
                1,
                vec![
                    Field::string(FieldId::FileName, "a.txt"),
                    Field::u32(FieldId::TransferSize, size),
                ],
            )
        };

        let [reply] = session
            .on_upload_file(&put(101), &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            reply
                .received(text_encoding::DEFAULT)
                .string(FieldId::ErrorText)
                .as_deref(),
            Some(
                "Cannot accept upload of the file \"a.txt\" because there is not enough free disk space."
            )
        );
        let [reply] = session
            .on_upload_file(&put(100), &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn upload_fldr_needs_the_size_plus_a_margin_free_even_to_resume() {
        let files = crate::file::InMemoryFileProvider::default();
        files.set_free_space(Some(512_100));
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"x");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());
        let put = |name, size, resume| {
            let mut fields = vec![
                Field::u32(FieldId::FldrItemCount, 1),
                Field::u32(FieldId::TransferSize, size),
            ];
            if resume {
                fields.push(Field::u16(FieldId::FileXferOptions, 1));
            }
            folder_request(Opcode::UploadFldr, &[], name, fields)
        };
        let is_error = |tx: Transaction<Received>| {
            let session = &session;
            let state = &state;
            async move {
                let [reply] = session.on_upload_fldr(&tx, state).await.try_into().unwrap();
                reply.is_error()
            }
        };

        assert!(is_error(put("New", 101, false)).await);
        assert!(
            !state
                .file_exists(FileRequester { login: "Alice" }, &HlPath::default(), "New")
                .await
        );
        assert!(is_error(put("Top", 101, true)).await);
        assert!(!is_error(put("Top", 100, true)).await);
    }

    #[tokio::test]
    async fn upload_file_rejects_an_existing_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"existing");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_file_rejects_a_new_upload_over_a_partial_upload() {
        let files = crate::file::InMemoryFileProvider::default();
        let partial = crate::file::partial_upload_name("a.txt");
        files.seed(&HlPath::default(), &partial, *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), &partial)
                .await
        );
    }

    #[tokio::test]
    async fn upload_file_resumes_into_a_partial_upload() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(
            &HlPath::default(),
            &crate::file::partial_upload_name("a.txt"),
            *b"hello",
        );
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let reply = reply.received(text_encoding::DEFAULT);
        let reference = reply
            .field(FieldId::RefNum)
            .and_then(Field::as_u32)
            .unwrap();

        let resume_field = reply.field(FieldId::FileResumeData).unwrap();
        let already_have = types::ResumeData::decode(resume_field.as_bytes())
            .unwrap()
            .data;
        assert_eq!(already_have, 5);

        let transfer = state.take_transfer(TransferRef(reference)).await.unwrap();
        assert_eq!(transfer.direction, TransferDirection::Put);
        assert_eq!(transfer.resume_offset, 5);
    }

    #[tokio::test]
    async fn upload_file_resume_of_a_completed_upload_is_an_error() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_file_resume_reports_the_resource_fork_size() {
        let files = crate::file::InMemoryFileProvider::default();
        let partial = crate::file::partial_upload_name("a.txt");
        files.seed(&HlPath::default(), &partial, *b"hello");
        files.seed_resource_fork(&HlPath::default(), &partial, *b"rs");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        let reply = reply.received(text_encoding::DEFAULT);
        let resume_field = reply.field(FieldId::FileResumeData).unwrap();
        assert_eq!(
            types::ResumeData::decode(resume_field.as_bytes()).unwrap(),
            types::ResumeData {
                data: 5,
                resource: 2
            }
        );
        let reference = reply.field(FieldId::RefNum).and_then(Field::as_u32);
        let transfer = state
            .take_transfer(TransferRef(reference.unwrap()))
            .await
            .unwrap();
        let TransferKind::File(file) = transfer.kind else {
            panic!("not a file transfer");
        };
        assert_eq!(file.resource_offset, 2);
    }

    #[tokio::test]
    async fn upload_file_resume_of_an_empty_partial_upload_still_sends_resume_data() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(
            &HlPath::default(),
            &crate::file::partial_upload_name("a.txt"),
            *b"",
        );
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(reply.field(FieldId::FileResumeData).is_some());
    }

    #[tokio::test]
    async fn upload_file_resumes_an_upload_left_partial_by_an_earlier_attempt() {
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

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let reply = reply.received(text_encoding::DEFAULT);
        let resume_field = reply.field(FieldId::FileResumeData).unwrap();
        let already_have = types::ResumeData::decode(resume_field.as_bytes())
            .unwrap()
            .data;
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
    async fn upload_file_resume_of_a_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::u16(FieldId::FileXferOptions, 1),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_file_to_a_missing_folder_is_an_error() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["Nope"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn upload_file_into_a_file_is_an_error() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let put = request(
            Opcode::UploadFile,
            1,
            vec![
                Field::string(FieldId::FileName, "b.txt"),
                Field::bytes(
                    FieldId::FilePath,
                    HlPath::new(["a.txt"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_upload_file(&put, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn delete_file_removes_a_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let delete = request(
            Opcode::DeleteFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_delete_file(&delete, &state)
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
    async fn delete_file_denied_without_delete_files() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"bye");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::DELETE_FILE);
        let session = test_session("Alice", access);

        let delete = request(
            Opcode::DeleteFile,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_delete_file(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn delete_file_missing_file_is_an_error() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let delete = request(
            Opcode::DeleteFile,
            1,
            vec![Field::string(FieldId::FileName, "missing.txt")],
        );
        let [reply] = session
            .on_delete_file(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn delete_file_removes_a_partial_upload_by_its_listed_name() {
        let partial = crate::file::partial_upload_name("a.txt");
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"done");
        files.seed(&HlPath::default(), &partial, *b"half");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let delete = request(
            Opcode::DeleteFile,
            1,
            vec![Field::string(FieldId::FileName, &*partial)],
        );
        let [reply] = session
            .on_delete_file(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), &partial)
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn new_folder_creates_a_folder() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = request(
            Opcode::NewFolder,
            1,
            vec![Field::string(FieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_new_folder(&mkdir, &state)
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
    async fn new_folder_stores_the_validated_name() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = request(
            Opcode::NewFolder,
            1,
            vec![Field::string(FieldId::FileName, "AC/DC")],
        );
        let [reply] = session
            .on_new_folder(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "AC-DC")
                .await
        );
    }

    #[tokio::test]
    async fn new_folder_denied_without_create_folders() {
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let mut access = Access::all();
        access.remove(Access::CREATE_FOLDER);
        let session = test_session("Alice", access);

        let mkdir = request(
            Opcode::NewFolder,
            1,
            vec![Field::string(FieldId::FileName, "NewFolder")],
        );
        let [reply] = session
            .on_new_folder(&mkdir, &state)
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

        let list = request(Opcode::GetFileNameList, 1, vec![]);
        let [reply] = session
            .on_get_file_name_list(&list, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn get_file_info_denied_without_access_extra() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.remove(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn get_file_info_returns_name_and_size() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply.received(encoding).string(FieldId::FileName).unwrap(),
            "a.txt"
        );
        assert_eq!(
            reply
                .received(encoding)
                .field(FieldId::FileSize)
                .and_then(Field::as_u32),
            Some(5)
        );
        let reply = reply.received(encoding);
        assert_eq!(reply.string(FieldId::FileType).as_deref(), Some("TEXT"));
        assert_eq!(
            reply.string(FieldId::FileTypeString).as_deref(),
            Some("Text File")
        );
        assert_eq!(
            reply.string(FieldId::FileCreatorString).as_deref(),
            Some("SimpleText")
        );
        assert!(reply.field(FieldId::FileCreateDate).is_none());
        assert!(reply.field(FieldId::FileComment).is_none());
    }

    #[tokio::test]
    async fn get_file_info_of_a_folder_sends_its_kind_and_no_size() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Top"]), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "Top")],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        let reply = reply.received(encoding);
        assert_eq!(
            reply.string(FieldId::FileTypeString).as_deref(),
            Some("Folder")
        );
        assert_eq!(
            reply.string(FieldId::FileCreatorString).as_deref(),
            Some("n/a")
        );
        assert_eq!(reply.string(FieldId::FileType).as_deref(), Some("fldr"));
        assert!(reply.field(FieldId::FileSize).is_none());
    }

    #[tokio::test]
    async fn get_file_info_sends_a_code_with_no_display_name_as_is() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.gif", *b"GIF89a");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "a.gif")],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        let reply = reply.received(encoding);
        assert_eq!(
            reply.string(FieldId::FileTypeString).as_deref(),
            Some("GIFf")
        );
        assert_eq!(
            reply.string(FieldId::FileCreatorString).as_deref(),
            Some("GKON")
        );
    }

    #[tokio::test]
    async fn get_file_info_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hello");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["a.txt"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(
            reply.received(encoding).string(FieldId::FileName).unwrap(),
            "a.txt"
        );
    }

    #[tokio::test]
    async fn set_file_info_renames_a_file() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
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
    async fn set_file_info_renames_after_a_failed_comment_write_only_by_policy() {
        struct StopOnCommentError;
        impl crate::ServerPolicy for StopOnCommentError {
            fn error(&self, _case: crate::Silent) -> Option<std::borrow::Cow<'static, str>> {
                Some("refused".into())
            }
        }
        let session = test_session("Alice", Access::all());
        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let files = || {
            let files = crate::file::InMemoryFileProvider::default();
            files.seed(&HlPath::default(), "a.txt", *b"hi");
            files.set_comment_writes_fail(true);
            files
        };

        for (state, renamed) in [
            (state_with_files(files()), true),
            (
                state_with_files_and_policy(files(), StopOnCommentError),
                false,
            ),
        ] {
            let [reply] = session
                .on_set_file_info(&setinfo, &state)
                .await
                .try_into()
                .unwrap();
            assert_eq!(reply.is_error(), !renamed);
            let requester = FileRequester { login: "" };
            assert_eq!(
                state
                    .file_exists(requester, &HlPath::default(), "b.txt")
                    .await,
                renamed
            );
        }
    }

    #[tokio::test]
    async fn set_file_info_renames_to_the_validated_name() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b/c.txt"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert!(
            state
                .file_exists(FileRequester { login: "" }, &HlPath::default(), "b-c.txt")
                .await
        );
    }

    #[tokio::test]
    async fn set_file_info_renames_a_partial_upload_by_its_listed_name() {
        let partial = crate::file::partial_upload_name("a.txt");
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"done");
        files.seed(&HlPath::default(), &partial, *b"half");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, &*partial),
                Field::string(FieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            !state
                .file_exists(requester, &HlPath::default(), &partial)
                .await
        );
        assert!(
            state
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
    async fn move_file_moves_a_partial_upload_by_its_listed_name() {
        let encoding = text_encoding::DEFAULT;
        let partial = crate::file::partial_upload_name("a.txt");
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"done");
        files.seed(&HlPath::default(), &partial, *b"half");
        files.seed(&HlPath::new(["Sub"]), "placeholder.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = request(
            Opcode::MoveFile,
            1,
            vec![
                Field::string(FieldId::FileName, &*partial),
                Field::bytes(
                    FieldId::FileNewPath,
                    HlPath::new(["Sub"]).encode(encoding).unwrap(),
                ),
            ],
        );
        let [reply] = session
            .on_move_file(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let requester = FileRequester { login: "" };
        assert!(
            state
                .file_exists(requester, &HlPath::new(["Sub"]), &partial)
                .await
        );
        assert!(
            state
                .file_exists(requester, &HlPath::default(), "a.txt")
                .await
        );
    }

    #[tokio::test]
    async fn set_file_info_denied_without_rename_files() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::RENAME_FILE);
        let session = test_session("Alice", access);

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn set_file_info_without_a_new_name_is_a_no_op() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
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
    async fn set_file_info_sets_a_comment_returned_by_getinfo() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut session = test_session("Alice", Access::all());
        session.access_extra.insert(AccessExtra::FILE_GETINFO);

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let getinfo = request(
            Opcode::GetFileInfo,
            1,
            vec![Field::string(FieldId::FileName, "a.txt")],
        );
        let [reply] = session
            .on_get_file_info(&getinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(
            reply.received(encoding).string(FieldId::FileComment),
            Some("a note".to_string())
        );
    }

    #[tokio::test]
    async fn set_file_info_sets_the_comment_when_the_rename_is_denied() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"x");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::RENAME_FILE);
        let session = test_session("Alice", access);
        let requester = FileRequester { login: "Alice" };

        let set = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileNewName, "b.txt"),
                Field::string(FieldId::FileComment, "kept"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&set, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
        assert_eq!(
            state
                .file_comment(requester, &HlPath::default(), "a.txt", Access::all())
                .await
                .unwrap()
                .as_deref(),
            Some("kept")
        );
    }

    #[tokio::test]
    async fn set_file_info_comment_denied_without_set_file_comment() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FILE_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "a.txt"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn set_file_info_folder_comment_denied_without_set_folder_comment() {
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Folder"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::SET_FOLDER_COMMENT);
        let session = test_session("Alice", access);

        let setinfo = request(
            Opcode::SetFileInfo,
            1,
            vec![
                Field::string(FieldId::FileName, "Folder"),
                Field::string(FieldId::FileComment, "a note"),
            ],
        );
        let [reply] = session
            .on_set_file_info(&setinfo, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn new_folder_derives_name_from_file_path_when_file_name_is_absent() {
        let encoding = text_encoding::DEFAULT;
        let state = state_with_files(crate::file::InMemoryFileProvider::default());
        let session = test_session("Alice", Access::all());

        let mkdir = request(
            Opcode::NewFolder,
            1,
            vec![Field::bytes(
                FieldId::FilePath,
                HlPath::new(["NewFolder"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = session
            .on_new_folder(&mkdir, &state)
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
    async fn move_file_relocates_a_file() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Sub"]), ".keep", *b"");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = request(
            Opcode::MoveFile,
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
            .on_move_file(&move_tx, &state)
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
    async fn move_file_to_root_treats_an_absent_new_path_as_root() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::new(["Sub"]), "a.txt", *b"hi");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let move_tx = request(
            Opcode::MoveFile,
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
            .on_move_file(&move_tx, &state)
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
    async fn move_file_denied_without_move_files() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MOVE_FILE);
        let session = test_session("Alice", access);

        let move_tx = request(
            Opcode::MoveFile,
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
            .on_move_file(&move_tx, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn make_file_alias_denied_without_make_aliases() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        let state = state_with_files(files);
        let mut access = Access::all();
        access.remove(Access::MAKE_ALIAS);
        let session = test_session("Alice", access);

        let symlink = request(
            Opcode::MakeFileAlias,
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
            .on_make_file_alias(&symlink, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn make_file_alias_creates_an_alias_reachable_at_the_new_location() {
        let encoding = text_encoding::DEFAULT;
        let files = crate::file::InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"hi");
        files.seed(&HlPath::new(["Sub"]), "b.txt", *b"");
        let state = state_with_files(files);
        let session = test_session("Alice", Access::all());

        let symlink = request(
            Opcode::MakeFileAlias,
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
            .on_make_file_alias(&symlink, &state)
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
