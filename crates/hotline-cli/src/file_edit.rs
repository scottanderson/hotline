//! The text a file operation asks for.

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
