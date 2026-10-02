//! Reading and writing a flattened file object on an `HTXF` connection.

use std::io;

use hotline_proto::htxf::{decode_flat_file_prefix, flat_file_header, FLAT_FILE_PREFIX_LEN};
use hotline_proto::ForkHeader;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::Result;

/// Read one flattened file object from `stream` and write its `DATA` fork
/// to `out`. Returns the number of bytes written.
pub(crate) async fn read_flat_file<R, W>(stream: &mut R, out: &mut W) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut prefix = [0u8; FLAT_FILE_PREFIX_LEN];
    stream.read_exact(&mut prefix).await?;
    let fork_count = decode_flat_file_prefix(&prefix)?;
    let mut written = 0;
    for _ in 0..fork_count {
        let mut buf = [0u8; ForkHeader::LEN];
        stream.read_exact(&mut buf).await?;
        let fork = ForkHeader::decode(&buf)?;
        let size = u64::from(fork.data_size);
        let mut data = (&mut *stream).take(size);
        let copied = if &fork.fork_type == b"DATA" {
            written = tokio::io::copy(&mut data, out).await?;
            written
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

/// Write a flattened file object to `stream` whose `DATA` fork is the next
/// `size` bytes of `data`. Returns the number of bytes sent.
pub(crate) async fn write_flat_file<R, W>(stream: &mut W, data: &mut R, size: u32) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    stream.write_all(&flat_file_header(size)).await?;
    let size = u64::from(size);
    let sent = tokio::io::copy(&mut data.take(size), stream).await?;
    if sent < size {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    stream.flush().await?;
    Ok(sent)
}
