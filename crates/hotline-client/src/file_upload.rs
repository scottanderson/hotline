//! [`FileUpload`]: a file upload the server has accepted.

use std::net::SocketAddr;

use hotline_proto::htxf::FLAT_FILE_HEADER_LEN;
use hotline_proto::{HtxfHeader, HtxfType};
use tokio::io::{AsyncRead, AsyncWriteExt};

use crate::client::open_transfer;
use crate::error::Result;
use crate::flat_file::write_flat_file;

/// An upload the server accepted with [`myTran_UploadFile`] (203).
///
/// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
#[derive(Debug)]
pub struct FileUpload {
    pub(crate) offset: u32,
    pub(crate) reference: u32,
    /// The size of the file's data in bytes.
    pub(crate) file_size: u32,
    /// The server's `HTXF` address, one port above the transaction port.
    pub(crate) transfer_addr: SocketAddr,
}

impl FileUpload {
    /// The bytes the server already has, from a partial upload.
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// The size of the flat file object that carries `file_size` data bytes.
    pub(crate) const fn transfer_size(file_size: u32) -> Option<u32> {
        file_size.checked_add(FLAT_FILE_HEADER_LEN as u32)
    }

    /// Open the `HTXF` connection and send `data` as the file's data fork.
    /// `data` must hold the file's bytes from [`Self::offset`] on.
    /// Returns once the server closes the connection.
    pub async fn send<R: AsyncRead + Unpin>(self, data: &mut R) -> Result<u64> {
        let remaining = self.file_size.saturating_sub(self.offset);
        let header = HtxfHeader {
            reference: self.reference,
            len: Self::transfer_size(remaining).unwrap_or(u32::MAX),
            transfer_type: HtxfType::File,
        };
        let mut stream = open_transfer(self.transfer_addr, header).await?;
        let sent = write_flat_file(&mut stream, data, remaining).await?;
        stream.shutdown().await?;
        tokio::io::copy(&mut stream, &mut tokio::io::sink()).await?;
        Ok(sent)
    }
}
