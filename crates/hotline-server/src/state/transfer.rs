use std::net::IpAddr;

use hotline_proto::htxf::HtxfType;

use crate::state::{FileTransfer, FolderTransfer};

/// Which way a pending `HTXF` transfer moves file bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Client downloading a file from the server, [`myTran_DownloadFile`] (202).
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    Get,
    /// Client uploading a file to the server, [`myTran_UploadFile`] (203).
    ///
    /// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
    Put,
}

/// Which resource a pending `HTXF` transfer moves bytes for.
#[derive(Debug, Clone)]
pub enum TransferKind {
    /// A file, see [`FileTransfer`].
    File(FileTransfer),
    /// A folder and everything under it, see [`FolderTransfer`].
    Folder(FolderTransfer),
    /// The server banner image from [`crate::BannerProvider`], sent as a plain
    /// unframed byte stream.
    Banner,
}

/// A transfer awaiting its data connection: allocated when
/// [`myTran_DownloadFile`] (202), [`myTran_UploadFile`] (203), or
/// [`myTran_DownloadBanner`] (212) replies with a reference number, consumed
/// once the client opens the second connection and sends that reference
/// back. A [`TransferKind::File`] identifies the file by the
/// [`crate::FileProvider`] of the request, the login, the directory, and
/// the name. The transfer reads and writes the file through that provider.
///
/// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
/// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
/// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
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
    /// The session that asked for this transfer.
    pub user_id: u16,
}

impl PendingTransfer {
    /// Whether an `HTXF` header of `transfer_type` finds this transfer.
    pub(crate) fn accepts(&self, transfer_type: HtxfType) -> bool {
        match (&self.kind, self.direction) {
            (TransferKind::Banner, _) => transfer_type == HtxfType::Banner,
            (TransferKind::Folder(_), _) => transfer_type == HtxfType::Folder,
            (TransferKind::File(_), TransferDirection::Get) => {
                !matches!(transfer_type, HtxfType::Folder | HtxfType::Banner)
            }
            (TransferKind::File(_), TransferDirection::Put) => transfer_type != HtxfType::Folder,
        }
    }
}
