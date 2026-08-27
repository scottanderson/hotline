use crate::state::TransferTarget;

/// A pending `HTXF` transfer's file identity, from [`crate::FileProvider`],
/// framed as a flattened file object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    /// The file's login and path.
    pub target: TransferTarget,
    /// `myTran_DownloadFile` (202) with `myField_FileXferOptions` (204)
    /// `== 2`: raw file bytes, with no flattened file object and no resume.
    pub raw: bool,
}
