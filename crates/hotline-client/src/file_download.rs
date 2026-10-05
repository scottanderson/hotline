//! [`FileDownload`]: a file download the server has accepted.

use std::net::SocketAddr;

use hotline_proto::htxf::FlatFileInfo;
use hotline_proto::text_encoding::Encoding;
use hotline_proto::{HtxfHeader, HtxfType};
use tokio::io::AsyncWrite;

use crate::client::open_transfer;
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
    /// The connection's text encoding, for the `INFO` fork's name and comment.
    pub(crate) encoding: &'static Encoding,
}

/// What [`FileDownload::receive_forks`] read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReceivedFile {
    /// The `INFO` fork: type, creator, dates, name and comment. `None`
    /// when the server sent no `INFO` fork.
    pub info: Option<FlatFileInfo>,
    /// Bytes written from the `DATA` fork.
    pub data_len: u64,
    /// Bytes written from the `MACR` (resource) fork.
    pub rsrc_len: u64,
}

impl FileDownload {
    /// The server's reference for this download.
    pub const fn reference(&self) -> u32 {
        self.reference
    }

    /// Open the `HTXF` connection and write the file's data fork to `out`.
    /// Returns the number of bytes written. A queued download waits on the
    /// open connection until the server starts it.
    pub async fn receive<W: AsyncWrite + Unpin>(self, out: &mut W) -> Result<u64> {
        let received = self.receive_forks(out, &mut tokio::io::sink()).await?;
        Ok(received.data_len)
    }

    /// Like [`FileDownload::receive`], but also keeps the rest of the
    /// file: the resource fork goes to `rsrc`, and the `INFO` fork's
    /// type, creator, dates and comment come back in [`ReceivedFile`].
    /// Mac files often need these to open.
    pub async fn receive_forks<D, F>(self, data: &mut D, rsrc: &mut F) -> Result<ReceivedFile>
    where
        D: AsyncWrite + Unpin,
        F: AsyncWrite + Unpin,
    {
        let header = HtxfHeader {
            reference: self.reference,
            len: 0,
            transfer_type: HtxfType::File,
        };
        let mut stream = open_transfer(self.transfer_addr, header).await?;
        read_flat_file(&mut stream, data, rsrc, self.encoding).await
    }
}
