//! The choices a transfer asks for before it starts, and its sizes.

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

/// `bytes` as openline's `UText::SizeToText` shows them: `0K`, the bytes
/// under 1K, whole kilobytes, or megabytes with a tenth.
pub fn size_text(bytes: u64) -> String {
    size_to_text(bytes, false)
}

/// `bytes` with `<1K` for 1 to 1023 bytes, as openline's file list shows
/// a size (`kDontShowBytes`).
pub fn file_size_text(bytes: u64) -> String {
    size_to_text(bytes, true)
}

/// openline's `UText::SizeToText`. Each fraction drops its remainder.
fn size_to_text(bytes: u64, dont_show_bytes: bool) -> String {
    const K: u64 = 1 << 10;
    const M: u64 = 1 << 20;
    match bytes {
        0 => "0K".to_string(),
        1..K if dont_show_bytes => "<1K".to_string(),
        1..K => bytes.to_string(),
        K..M => format!("{}K", bytes / K),
        _ => match (bytes / M, (bytes % M) / 104_858) {
            (whole, 0) => format!("{whole}M"),
            (whole, tenth) => format!("{whole}.{tenth}M"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_text_follows_openline_size_to_text() {
        assert_eq!(size_text(0), "0K");
        assert_eq!(size_text(512), "512");
        assert_eq!(size_text(1536), "1K");
        assert_eq!(size_text(1 << 20), "1M");
        assert_eq!(size_text((3 << 20) + (1 << 19)), "3.4M");
        assert_eq!(size_text(u32::MAX.into()), "4095.9M");
    }

    #[test]
    fn file_size_text_hides_the_bytes_under_1k() {
        assert_eq!(file_size_text(0), "0K");
        assert_eq!(file_size_text(300), "<1K");
        assert_eq!(file_size_text(2047), "1K");
    }
}
