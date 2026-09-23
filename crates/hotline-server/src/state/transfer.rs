use std::net::IpAddr;

use crate::state::{FileTransfer, FolderTransfer};

/// Which way a pending `HTXF` transfer moves file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Client downloading a file from the server (`FILE_GET`).
    Get,
    /// Client uploading a file to the server (`FILE_PUT`).
    Put,
}

/// Which resource a pending `HTXF` transfer moves bytes for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferKind {
    /// A file, see [`FileTransfer`].
    File(FileTransfer),
    /// A folder and everything under it, see [`FolderTransfer`].
    Folder(FolderTransfer),
    /// The server banner image from [`crate::BannerProvider`], sent as a plain
    /// unframed byte stream.
    Banner,
}

/// A transfer awaiting its data connection: allocated when `FILE_GET`/
/// `FILE_PUT`/`myTran_DownloadBanner` replies with a reference number,
/// consumed once the client opens the second connection and sends that
/// reference back. A [`TransferKind::File`] identifies the file by
/// [`crate::FileRequester`] login plus directory/name, not a raw path, so
/// completing the transfer still goes through the [`crate::FileProvider`]
/// rather than assuming filesystem storage.
#[derive(Debug, Clone)]
pub struct PendingTransfer {
    /// Which storage backs this transfer, and its kind-specific data.
    pub kind: TransferKind,
    /// Whether this is a download or upload.
    pub direction: TransferDirection,
    /// Download: the file's size in bytes. Upload: the size the client
    /// declared it will send.
    pub size: u64,
    /// Bytes already stored, to be skipped rather than resent: for a
    /// download, what the client already has; for an upload, the
    /// destination file's current size. Always `0` for a non-resumed
    /// transfer.
    pub resume_offset: u64,
    /// The control connection's remote address; the data connection must
    /// arrive from the same IP or the transfer is refused.
    pub client_ip: IpAddr,
}
