//! [`FileDownload`]: a file download the server has accepted.

use std::net::SocketAddr;

use hotline_proto::{HtxfHeader, HtxfType};
use tokio::io::{AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;
use crate::flat_file::read_flat_file;

/// A download the server accepted with [`myTran_DownloadFile`] (202).
///
/// [`myTran_DownloadFile`]: hotline_proto::Opcode::DownloadFile
#[derive(Debug)]
pub struct FileDownload {
    /// The size of the file's data in bytes.
    pub file_size: u32,
    /// The downloads ahead of this one in the server's queue.
    pub waiting_count: u32,
    pub(crate) reference: u32,
    /// The server's `HTXF` address, one port above the transaction port.
    pub(crate) transfer_addr: SocketAddr,
}

impl FileDownload {
    /// Open the `HTXF` connection and write the file's data fork to `out`.
    /// Returns the number of bytes written. A queued download waits on the
    /// open connection until the server starts it.
    pub async fn receive<W: AsyncWrite + Unpin>(self, out: &mut W) -> Result<u64> {
        let mut stream = TcpStream::connect(self.transfer_addr).await?;
        let header = HtxfHeader {
            reference: self.reference,
            len: 0,
            transfer_type: HtxfType::File,
        };
        stream.write_all(&header.encode()).await?;
        read_flat_file(&mut stream, out).await
    }
}
