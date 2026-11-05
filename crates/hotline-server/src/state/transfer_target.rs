use std::sync::Arc;

use hotline_proto::types::HlPath;

use crate::file::{FileProvider, FileRequester};

/// The login, files store, and path a pending `HTXF` file or folder
/// transfer acts on.
#[derive(Clone)]
pub struct TransferTarget {
    /// The requesting session's account login.
    pub login: Box<str>,
    /// The files store of the login at the request, kept for the whole
    /// transfer.
    pub store: Arc<dyn FileProvider>,
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

impl std::fmt::Debug for TransferTarget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransferTarget")
            .field("login", &self.login)
            .field("dir", &self.dir)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}
