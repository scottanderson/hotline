//! [`FolderSender`]: the open connection of a folder upload.

use std::io;

use hotline_proto::htxf::{FolderAction, FolderItemHeader, FLAT_FILE_HEADER_LEN};
use hotline_proto::types::ResumeData;
use hotline_proto::Encoding;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::{ClientError, Result};
use crate::flat_file::write_flat_file;

/// Sends the items of a [`FolderUpload`](crate::FolderUpload), in the order
/// and number given to
/// [`HotlineClient::upload_folder`](crate::HotlineClient::upload_folder).
/// A folder comes before the items inside it.
#[derive(Debug)]
pub struct FolderSender {
    stream: TcpStream,
    remaining: u32,
    encoding: &'static Encoding,
    /// The server sends Next File before the next item header.
    action_due: bool,
}

impl FolderSender {
    pub(crate) const fn new(
        stream: TcpStream,
        remaining: u32,
        encoding: &'static Encoding,
    ) -> Self {
        Self {
            stream,
            remaining,
            encoding,
            action_due: true,
        }
    }

    /// Send the next item's header. Its path runs from inside the uploaded
    /// folder to the item. Returns the offset to send a file's data from,
    /// or `None` if the server wants no data for the item.
    pub async fn send_item(&mut self, item: &FolderItemHeader) -> Result<Option<u64>> {
        if self.remaining == 0 {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "more items than declared").into(),
            );
        }
        self.remaining = self.remaining.saturating_sub(1);
        if self.action_due {
            match self.read_action().await? {
                FolderAction::NextFile => {}
                other => return Err(unexpected(other)),
            }
        }
        self.stream.write_all(&item.encode(self.encoding)?).await?;
        match self.read_action().await? {
            FolderAction::NextFile => {
                self.action_due = false;
                Ok(None)
            }
            FolderAction::SendFile => {
                self.action_due = self.remaining > 0;
                Ok(Some(0))
            }
            FolderAction::ResumeFile => {
                self.action_due = self.remaining > 0;
                let mut len = [0u8; 2];
                self.stream.read_exact(&mut len).await?;
                let mut buf = vec![0u8; usize::from(u16::from_be_bytes(len))];
                self.stream.read_exact(&mut buf).await?;
                Ok(Some(u64::from(ResumeData::decode(&buf)?.data)))
            }
            other => Err(unexpected(other)),
        }
    }

    /// Send the file data that [`Self::send_item`] asked for: the next
    /// `size` bytes of `data`. Returns the number of bytes sent.
    pub async fn send_file<R: AsyncRead + Unpin>(
        &mut self,
        data: &mut R,
        size: u64,
    ) -> Result<u64> {
        let too_large = || io::Error::new(io::ErrorKind::InvalidInput, "the file is too large");
        let size = u32::try_from(size).map_err(|_| too_large())?;
        let flat_size = size
            .checked_add(FLAT_FILE_HEADER_LEN as u32)
            .ok_or_else(too_large)?;
        self.stream.write_all(&flat_size.to_be_bytes()).await?;
        write_flat_file(&mut self.stream, data, size).await
    }

    /// Wait for the server to close the connection after the last item.
    pub async fn finish(mut self) -> Result<()> {
        self.stream.shutdown().await?;
        tokio::io::copy(&mut self.stream, &mut tokio::io::sink()).await?;
        Ok(())
    }

    async fn read_action(&mut self) -> Result<FolderAction> {
        let mut buf = [0u8; 2];
        self.stream.read_exact(&mut buf).await?;
        Ok(FolderAction::from(u16::from_be_bytes(buf)))
    }
}

fn unexpected(action: FolderAction) -> ClientError {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("unexpected folder action {action:?}"),
    )
    .into()
}
