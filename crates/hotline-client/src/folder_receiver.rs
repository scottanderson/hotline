//! [`FolderReceiver`]: the open connection of a folder download.

use std::io;

use hotline_proto::htxf::{FolderAction, FolderItemHeader};
use hotline_proto::types;
use hotline_proto::wire::VecWriter;
use hotline_proto::Encoding;
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::error::Result;
use crate::flat_file::read_flat_file;

/// Steps through the items of a [`FolderDownload`](crate::FolderDownload).
/// Call [`Self::next_item`] for each item, and [`Self::receive_file`] or
/// [`Self::resume_file`] for each file to keep. A file that is not received
/// is skipped.
#[derive(Debug)]
pub struct FolderReceiver {
    stream: Option<TcpStream>,
    remaining: u32,
    encoding: &'static Encoding,
}

impl FolderReceiver {
    pub(crate) const fn new(
        stream: Option<TcpStream>,
        remaining: u32,
        encoding: &'static Encoding,
    ) -> Self {
        Self {
            stream,
            remaining,
            encoding,
        }
    }

    /// The next item's header, `None` after the last item. Its path runs
    /// from inside the downloaded folder to the item.
    pub async fn next_item(&mut self) -> Result<Option<FolderItemHeader>> {
        let Some(stream) = self.stream.as_mut().filter(|_| self.remaining > 0) else {
            self.stream = None;
            return Ok(None);
        };
        self.remaining -= 1;
        stream
            .write_all(&u16::from(FolderAction::NextFile).to_be_bytes())
            .await?;
        let mut size = [0u8; 2];
        stream.read_exact(&mut size).await?;
        let mut body = vec![0u8; usize::from(u16::from_be_bytes(size))];
        stream.read_exact(&mut body).await?;
        Ok(Some(FolderItemHeader::decode_body(&body, self.encoding)?))
    }

    /// Receive the file that [`Self::next_item`] just returned and write its
    /// data fork to `out`. Returns the number of bytes written.
    pub async fn receive_file<W: AsyncWrite + Unpin>(&mut self, out: &mut W) -> Result<u64> {
        let action = u16::from(FolderAction::SendFile).to_be_bytes();
        self.request_file(&action, out).await
    }

    /// Receive the file that [`Self::next_item`] just returned from `offset`
    /// on, and write that part of its data fork to `out`. Returns the number
    /// of bytes written.
    pub async fn resume_file<W: AsyncWrite + Unpin>(
        &mut self,
        out: &mut W,
        offset: u32,
    ) -> Result<u64> {
        let resume_data = types::ResumeData {
            data: offset,
            resource: 0,
        }
        .encode();
        let request = VecWriter::new()
            .u16(FolderAction::ResumeFile.into())
            .u16(resume_data.len() as u16)
            .bytes(&resume_data)
            .finish();
        self.request_file(&request, out).await
    }

    /// Send `request`, then write the file's data fork to `out`.
    async fn request_file<W: AsyncWrite + Unpin>(
        &mut self,
        request: &[u8],
        out: &mut W,
    ) -> Result<u64> {
        let stream = self
            .stream
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotConnected))?;
        stream.write_all(request).await?;
        let mut size = [0u8; 4];
        stream.read_exact(&mut size).await?;
        let mut flat = stream.take(u64::from(u32::from_be_bytes(size)));
        let received =
            read_flat_file(&mut flat, out, &mut tokio::io::sink(), self.encoding).await?;
        Ok(received.data_len)
    }
}
