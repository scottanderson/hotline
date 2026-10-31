//! The text a file operation asks for.

use hotline_proto::Access;

/// A file operation that takes one line of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileEdit {
    /// Create a folder with the typed name.
    NewFolder,
    /// Give the file the typed name.
    Rename,
    /// Set the file's comment.
    Comment,
    /// Move the file into the typed folder.
    Move,
    /// Make an alias of the file in the typed folder.
    Alias,
}

impl FileEdit {
    /// The title of the box that asks for the text, for the file `name`.
    pub fn title(self, name: &str) -> String {
        match self {
            Self::NewFolder => "new folder".to_string(),
            Self::Rename => format!("rename {name} to"),
            Self::Comment => format!("comment of {name}"),
            Self::Move => format!("move {name} into the folder"),
            Self::Alias => format!("make an alias of {name} in the folder"),
        }
    }

    /// The privilege that turns on the operation on a file, or on a folder
    /// if `folder`.
    pub const fn enabled_by(self, folder: bool) -> Access {
        match (self, folder) {
            (Self::NewFolder, _) => Access::CREATE_FOLDER,
            (Self::Rename, false) => Access::RENAME_FILE,
            (Self::Rename, true) => Access::RENAME_FOLDER,
            (Self::Comment, false) => Access::SET_FILE_COMMENT,
            (Self::Comment, true) => Access::SET_FOLDER_COMMENT,
            (Self::Move, false) => Access::MOVE_FILE,
            (Self::Move, true) => Access::MOVE_FOLDER,
            (Self::Alias, _) => Access::MAKE_ALIAS,
        }
    }

    /// The verb for an error line.
    pub const fn verb(self) -> &'static str {
        match self {
            Self::NewFolder => "create",
            Self::Rename => "rename",
            Self::Comment => "set the comment of",
            Self::Move => "move",
            Self::Alias => "make an alias of",
        }
    }
}
