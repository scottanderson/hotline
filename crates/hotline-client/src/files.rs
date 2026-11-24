//! File and folder operations other than transfers and listings.

use hotline_proto::types::HlPath;
use hotline_proto::{Field, FieldId, Opcode};

use crate::client::HotlineClient;
use crate::error::Result;

/// A file or folder's [`myTran_GetFileInfo`] (206) reply.
///
/// [`myTran_GetFileInfo`]: hotline_proto::Opcode::GetFileInfo
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileInfo {
    /// The name.
    pub name: Box<str>,
    /// The size in bytes.
    pub size: u32,
    /// The file type as text, if the server sends it.
    pub type_text: Option<Box<str>>,
    /// The creator as text, if the server sends it.
    pub creator_text: Option<Box<str>>,
    /// The comment, empty if there is none.
    pub comment: Box<str>,
}

impl HotlineClient {
    /// The file or folder `name` in the folder at `path` and its path field.
    pub(crate) fn name_fields(&self, path: &HlPath, name: &str) -> Result<Vec<Field>> {
        let mut fields = vec![Field::string(FieldId::FileName, name.to_owned())];
        fields.extend(self.file_path_field(path)?);
        Ok(fields)
    }

    /// Delete the file or folder `name` in the folder at `path`, with
    /// everything inside it, [`myTran_DeleteFile`] (204).
    ///
    /// [`myTran_DeleteFile`]: hotline_proto::Opcode::DeleteFile
    pub async fn delete_file(&self, path: &HlPath, name: &str) -> Result<()> {
        let fields = self.name_fields(path, name)?;
        self.request(Opcode::DeleteFile, fields).await?;
        Ok(())
    }

    /// Create the folder `name` in the folder at `path`, [`myTran_NewFolder`] (205).
    ///
    /// [`myTran_NewFolder`]: hotline_proto::Opcode::NewFolder
    pub async fn new_folder(&self, path: &HlPath, name: &str) -> Result<()> {
        let fields = self.name_fields(path, name)?;
        self.request(Opcode::NewFolder, fields).await?;
        Ok(())
    }

    /// The file or folder `name` in the folder at `path`, [`myTran_GetFileInfo`] (206).
    ///
    /// [`myTran_GetFileInfo`]: hotline_proto::Opcode::GetFileInfo
    pub async fn get_file_info(&self, path: &HlPath, name: &str) -> Result<FileInfo> {
        let fields = self.name_fields(path, name)?;
        let reply = self.request(Opcode::GetFileInfo, fields).await?;
        let text = |id| reply.string(id).map(Into::into);
        Ok(FileInfo {
            name: text(FieldId::FileName).unwrap_or_else(|| name.into()),
            size: reply
                .field(FieldId::FileSize)
                .map(Field::as_u32)
                .unwrap_or_default(),
            type_text: text(FieldId::FileTypeString),
            creator_text: text(FieldId::FileCreatorString),
            comment: text(FieldId::FileComment).unwrap_or_default(),
        })
    }

    /// Rename the file or folder `name` in the folder at `path` to `new_name`,
    /// and set its `comment`, [`myTran_SetFileInfo`] (207). An empty
    /// `new_name` is not sent. `None` keeps the comment, and an empty
    /// comment clears it.
    ///
    /// [`myTran_SetFileInfo`]: hotline_proto::Opcode::SetFileInfo
    pub async fn set_file_info(
        &self,
        path: &HlPath,
        name: &str,
        new_name: &str,
        comment: Option<&str>,
    ) -> Result<()> {
        let mut fields = self.name_fields(path, name)?;
        if !new_name.is_empty() {
            fields.push(Field::string(FieldId::FileNewName, new_name.to_owned()));
        }
        match comment {
            Some("") => fields.push(Field::bytes(FieldId::FileComment, vec![0])),
            Some(comment) => fields.push(Field::string(FieldId::FileComment, comment.to_owned())),
            None => {}
        }
        self.request(Opcode::SetFileInfo, fields).await?;
        Ok(())
    }

    /// Move the file or folder `name` in the folder at `path` into the folder
    /// at `new_path`, [`myTran_MoveFile`] (208).
    ///
    /// [`myTran_MoveFile`]: hotline_proto::Opcode::MoveFile
    pub async fn move_file(&self, path: &HlPath, name: &str, new_path: &HlPath) -> Result<()> {
        let mut fields = self.name_fields(path, name)?;
        fields.extend(self.path_field(FieldId::FileNewPath, new_path)?);
        self.request(Opcode::MoveFile, fields).await?;
        Ok(())
    }

    /// Make an alias of the file or folder `name` in the folder at `path`, in
    /// the folder at `new_path`, [`myTran_MakeFileAlias`] (209).
    ///
    /// [`myTran_MakeFileAlias`]: hotline_proto::Opcode::MakeFileAlias
    pub async fn make_file_alias(
        &self,
        path: &HlPath,
        name: &str,
        new_path: &HlPath,
    ) -> Result<()> {
        let mut fields = self.name_fields(path, name)?;
        fields.extend(self.path_field(FieldId::FileNewPath, new_path)?);
        self.request(Opcode::MakeFileAlias, fields).await?;
        Ok(())
    }
}
