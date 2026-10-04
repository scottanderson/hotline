//! [`FolderUpload`]: a folder upload the server has accepted.

use std::net::SocketAddr;

use hotline_proto::{Encoding, HtxfHeader, HtxfType};

use crate::client::open_transfer;
use crate::error::Result;
use crate::folder_sender::FolderSender;

/// An upload the server accepted with [`myTran_UploadFldr`] (213).
///
/// [`myTran_UploadFldr`]: hotline_proto::Opcode::UploadFldr
#[derive(Debug)]
pub struct FolderUpload {
    /// `None` for an empty folder, which has no transfer.
    pub(crate) reference: Option<u32>,
    pub(crate) item_count: u32,
    /// The server's `HTXF` address, one port above the transaction port.
    pub(crate) transfer_addr: SocketAddr,
    pub(crate) encoding: &'static Encoding,
}

impl FolderUpload {
    /// Open the `HTXF` connection. The [`FolderSender`] then sends the items.
    /// `None` for an empty folder.
    pub async fn open(self) -> Result<Option<FolderSender>> {
        let Some(reference) = self.reference else {
            return Ok(None);
        };
        let header = HtxfHeader {
            reference,
            len: 0,
            transfer_type: HtxfType::Folder,
        };
        let stream = open_transfer(self.transfer_addr, header).await?;
        Ok(Some(FolderSender::new(
            stream,
            self.item_count,
            self.encoding,
        )))
    }
}
