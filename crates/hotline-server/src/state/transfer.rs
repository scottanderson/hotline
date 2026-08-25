use std::net::IpAddr;

use hotline_proto::types::HlPath;

/// Which way a pending `HTXF` transfer moves file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Client downloading a file from the server (`FILE_GET`).
    Get,
    /// Client uploading a file to the server (`FILE_PUT`).
    Put,
}

/// Which resource a pending `HTXF` transfer moves bytes for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferKind {
    /// A file from [`crate::FileProvider`], framed as a flattened file object.
    File,
    /// The server banner image from [`crate::BannerProvider`], sent as a plain
    /// unframed byte stream.
    Banner,
}

/// A transfer awaiting its data connection: allocated when `FILE_GET`/
/// `FILE_PUT`/`myTran_DownloadBanner` replies with a reference number,
/// consumed once the client opens the second connection and sends that
/// reference back. Identifies the file by [`crate::FileRequester`] login plus
/// directory/name, not a raw path, so completing the transfer still goes
/// through the [`crate::FileProvider`] rather than assuming filesystem storage.
/// `dir`/`name`/`login` are unused for [`TransferKind::Banner`].
#[derive(Debug, Clone)]
pub struct PendingTransfer {
    /// Which storage backs this transfer.
    pub kind: TransferKind,
    /// Whether this is a download or upload.
    pub direction: TransferDirection,
    /// The requesting session's account login.
    pub login: String,
    /// Directory the file is in.
    pub dir: HlPath,
    /// File name.
    pub name: String,
    /// Download: the file's size in bytes. Upload: the size the client
    /// declared it will send.
    pub size: u64,
    /// Bytes already stored, to be skipped rather than resent: for a
    /// download, what the client already has; for an upload, the
    /// destination file's current size. Always `0` for a non-resumed
    /// transfer.
    pub resume_offset: u64,
    /// `FILE_GET` only: `FileXferOptions` (204) `== 2`, requesting the
    /// raw file bytes with no flattened-file-object framing and no
    /// resume support. Always `false` for an upload or the banner.
    pub raw: bool,
    /// The control connection's remote address; the data connection must
    /// arrive from the same IP or the transfer is refused.
    pub client_ip: IpAddr,
}
