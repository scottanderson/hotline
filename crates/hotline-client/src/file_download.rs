//! [`FileDownload`]: a file download the server has accepted.

use std::io;
use std::net::SocketAddr;

use hotline_proto::htxf::{decode_flat_file_prefix, FLAT_FILE_PREFIX_LEN};
use hotline_proto::{ForkHeader, HtxfHeader, HtxfType};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;

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

        let mut prefix = [0u8; FLAT_FILE_PREFIX_LEN];
        stream.read_exact(&mut prefix).await?;
        let fork_count = decode_flat_file_prefix(&prefix)?;
        let mut written = 0;
        for _ in 0..fork_count {
            let mut buf = [0u8; ForkHeader::LEN];
            stream.read_exact(&mut buf).await?;
            let fork = ForkHeader::decode(&buf)?;
            let size = u64::from(fork.data_size);
            let mut data = (&mut stream).take(size);
            let copied = if &fork.fork_type == b"DATA" {
                let copied = tokio::io::copy(&mut data, out).await?;
                written = copied;
                copied
            } else {
                tokio::io::copy(&mut data, &mut tokio::io::sink()).await?
            };
            if copied < size {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
        }
        out.flush().await?;
        Ok(written)
    }
}
