use hotline_proto::text_encoding::Encoding;

use crate::state::TransferTarget;

/// A pending `HTXF` transfer's file identity, from [`crate::FileProvider`],
/// framed as a flattened file object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTransfer {
    /// The file's login and path.
    pub target: TransferTarget,
    /// [`myTran_DownloadFile`] (202) with [`myField_FileXferOptions`] (204)
    /// `== 2`: raw file bytes, with no flattened file object and no resume.
    ///
    /// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
    /// [`myField_FileXferOptions`]: hotline_proto::FieldId::FileXferOptions
    pub raw: bool,
    /// The control connection's text encoding, for an uploaded `INFO` fork.
    pub encoding: &'static Encoding,
    /// Download: the `INFO` fork data. Empty for an upload or a raw download.
    pub info: Vec<u8>,
    /// Download: the byte length of the resource fork.
    pub resource_size: u64,
    /// Bytes of the resource fork that the receiver already has.
    pub resource_offset: u64,
}
