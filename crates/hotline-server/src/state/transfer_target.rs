use hotline_proto::types::HlPath;

use crate::file::FileRequester;

/// The login and path a pending `HTXF` file or folder transfer acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferTarget {
    /// The requesting session's account login.
    pub login: Box<str>,
    /// Directory the file or folder is in.
    pub dir: HlPath,
    /// File or folder name.
    pub name: Box<str>,
}

impl TransferTarget {
    /// [`Self::login`] as a [`FileRequester`].
    pub fn requester(&self) -> FileRequester<'_> {
        FileRequester { login: &self.login }
    }
}
