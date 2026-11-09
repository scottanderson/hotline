//! [`FolderDownload`]: a folder download the server has accepted.

use std::net::SocketAddr;

use hotline_proto::{Encoding, HtxfHeader, HtxfType};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::client::open_transfer;
use crate::error::Result;
use crate::folder_receiver::FolderReceiver;

/// A download the server accepted with [`myTran_DownloadFldr`] (210).
///
/// [`myTran_DownloadFldr`]: hotline_proto::Opcode::DownloadFldr
#[derive(Debug)]
pub struct FolderDownload {
    /// The files and folders inside the folder, at every depth.
    pub item_count: u32,
    /// The bytes that the transfer connection carries.
    pub transfer_size: u32,
    /// The downloads ahead of this one in the server's queue.
    pub waiting_count: u32,
    /// `None` for an empty folder, which has no transfer.
    pub(crate) reference: Option<u32>,
    /// The server's `HTXF` address, one port above the transaction port.
    pub(crate) transfer_addr: SocketAddr,
    pub(crate) encoding: &'static Encoding,
}

impl FolderDownload {
    /// The server's reference for this download, `None` for an empty folder.
    pub const fn reference(&self) -> Option<u32> {
        self.reference
    }

    /// Open the `HTXF` connection. The [`FolderReceiver`] then steps
    /// through the items. A queued download waits on the open connection
    /// until the server starts it.
    pub async fn open(self) -> Result<FolderReceiver> {
        self.open_with(std::convert::identity).await
    }

    /// Open the `HTXF` connection, and read and write it through the stream
    /// that `wrap` makes, such as one that counts the bytes.
    pub async fn open_with<S>(self, wrap: impl FnOnce(TcpStream) -> S) -> Result<FolderReceiver<S>>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let Some(reference) = self.reference else {
            return Ok(FolderReceiver::new(None, 0, self.encoding));
        };
        let header = HtxfHeader {
            reference,
            len: 0,
            transfer_type: HtxfType::Folder,
        };
        let stream = open_transfer(self.transfer_addr, header).await?;
        Ok(FolderReceiver::new(
            Some(wrap(stream)),
            self.item_count,
            self.encoding,
        ))
    }
}
