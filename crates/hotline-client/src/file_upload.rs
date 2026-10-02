//! [`FileUpload`]: a file upload the server has accepted.

use std::net::SocketAddr;

use hotline_proto::htxf::FLAT_FILE_HEADER_LEN;
use hotline_proto::{HtxfHeader, HtxfType};
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;
use crate::flat_file::write_flat_file;

/// An upload the server accepted with [`myTran_UploadFile`] (203).
///
/// [`myTran_UploadFile`]: hotline_proto::Opcode::UploadFile
#[derive(Debug)]
pub struct FileUpload {
    pub(crate) reference: u32,
    /// The size of the file's data in bytes.
    pub(crate) file_size: u32,
    /// The size of the flat file object in bytes.
    pub(crate) transfer_size: u32,
    /// The server's `HTXF` address, one port above the transaction port.
    pub(crate) transfer_addr: SocketAddr,
}

impl FileUpload {
    /// The size of the flat file object that carries `file_size` data bytes.
    pub(crate) const fn transfer_size(file_size: u32) -> Option<u32> {
        file_size.checked_add(FLAT_FILE_HEADER_LEN as u32)
    }

    /// Open the `HTXF` connection and send `data` as the file's data fork.
    /// `data` must hold the file size given to
    /// [`HotlineClient::upload_file`](crate::HotlineClient::upload_file).
    /// Returns once the server closes the connection.
    pub async fn send<R: AsyncRead + Unpin>(self, data: &mut R) -> Result<u64> {
        let mut stream = TcpStream::connect(self.transfer_addr).await?;
        let header = HtxfHeader {
            reference: self.reference,
            len: self.transfer_size,
            transfer_type: HtxfType::File,
        };
        stream.write_all(&header.encode()).await?;
        let sent = write_flat_file(&mut stream, data, self.file_size).await?;
        stream.shutdown().await?;
        tokio::io::copy(&mut stream, &mut tokio::io::sink()).await?;
        Ok(sent)
    }
}
