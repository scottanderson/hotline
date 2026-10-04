//! The choices a transfer asks for before it starts.

/// How a transfer treats a copy that is already there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// Ask first if a copy is there.
    Ask,
    /// Continue the copy that is there.
    Resume,
    /// Start again over the copy that is there.
    Replace,
}

/// A transfer that waits for the user to choose how to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conflict {
    /// The local file `save_as` exists, so the download of `name` needs a new name.
    LocalFileExists { name: Box<str>, save_as: Box<str> },
    /// `save_as.hpf` holds part of an earlier download of `name`.
    PartialDownload { name: Box<str>, save_as: Box<str> },
    /// The download folder already has the folder `name`.
    LocalFolderExists { name: Box<str> },
    /// The server lists a partial upload of the local file at `local`.
    PartialUpload { local: Box<str> },
    /// The server has a folder with the name of the local folder at `local`.
    RemoteFolderExists { local: Box<str> },
}

impl Conflict {
    /// The question to show.
    pub fn question(&self) -> String {
        match self {
            Self::LocalFileExists { save_as, .. } => format!("{save_as} exists. Save as:"),
            Self::PartialDownload { name, .. } => format!("Part of {name} is downloaded."),
            Self::LocalFolderExists { name } => format!("The folder {name} is downloaded."),
            Self::PartialUpload { local } => format!("The server has part of {local}."),
            Self::RemoteFolderExists { local } => format!("The server has the folder {local}."),
        }
    }

    /// `true` if the transfer can start again over the copy that is there.
    pub const fn can_replace(&self) -> bool {
        matches!(
            self,
            Self::PartialDownload { .. } | Self::LocalFolderExists { .. }
        )
    }
}
