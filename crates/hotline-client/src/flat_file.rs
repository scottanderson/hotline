//! Reading and writing a flattened file object on an `HTXF` connection.

use std::io;

use hotline_proto::ForkHeader;
use hotline_proto::htxf::{
    FLAT_FILE_PREFIX_LEN, FlatFileInfo, decode_flat_file_prefix, empty_info, flat_file_header,
};
use hotline_proto::text_encoding::Encoding;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::Result;
use crate::file_download::ReceivedFile;

/// Read one flattened file object from `stream`, writing its `DATA` fork
/// to `data` and its `MACR` (resource) fork to `rsrc`, and decoding the
/// first [`FlatFileInfo::MAX_LEN`] bytes of its `INFO` fork with
/// `encoding`.
pub(crate) async fn read_flat_file<R, D, F>(
    stream: &mut R,
    data: &mut D,
    rsrc: &mut F,
    encoding: &'static Encoding,
) -> Result<ReceivedFile>
where
    R: AsyncRead + Unpin,
    D: AsyncWrite + Unpin,
    F: AsyncWrite + Unpin,
{
    let mut prefix = [0u8; FLAT_FILE_PREFIX_LEN];
    stream.read_exact(&mut prefix).await?;
    let fork_count = decode_flat_file_prefix(&prefix)?;
    let mut received = ReceivedFile::default();
    for _ in 0..fork_count {
        let mut buf = [0u8; ForkHeader::LEN];
        stream.read_exact(&mut buf).await?;
        let fork = ForkHeader::decode(&buf)?;
        let size = u64::from(fork.data_size);
        let mut fork_data = (&mut *stream).take(size);
        let copied = match &fork.fork_type {
            b"INFO" => {
                let len = size.min(FlatFileInfo::MAX_LEN as u64);
                let mut info = Vec::with_capacity(len as usize);
                let n = (&mut fork_data).take(len).read_to_end(&mut info).await?;
                received.info = Some(FlatFileInfo::decode(&info, encoding));
                let skipped = tokio::io::copy(&mut fork_data, &mut tokio::io::sink()).await?;
                (n as u64).saturating_add(skipped)
            }
            b"DATA" => {
                received.data_len = tokio::io::copy(&mut fork_data, data).await?;
                received.data_len
            }
            b"MACR" => {
                received.rsrc_len = tokio::io::copy(&mut fork_data, rsrc).await?;
                received.rsrc_len
            }
            _ => tokio::io::copy(&mut fork_data, &mut tokio::io::sink()).await?,
        };
        if copied < size {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
    }
    data.flush().await?;
    rsrc.flush().await?;
    Ok(received)
}

/// Write a flattened file object to `stream` whose `DATA` fork is the next
/// `size` bytes of `data`. Returns the number of bytes sent.
pub(crate) async fn write_flat_file<R, W>(stream: &mut W, data: &mut R, size: u32) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    stream
        .write_all(&flat_file_header(&empty_info(), size))
        .await?;
    let size = u64::from(size);
    let sent = tokio::io::copy(&mut data.take(size), stream).await?;
    if sent < size {
        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
    }
    stream.flush().await?;
    Ok(sent)
}

#[cfg(test)]
mod tests {
    use hotline_proto::text_encoding;
    use hotline_proto::wire::VecWriter;

    use super::*;

    fn fork(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        VecWriter::with_capacity(16 + data.len())
            .bytes(tag)
            .u32(0)
            .u32(0)
            .u32(data.len() as u32)
            .bytes(data)
            .finish()
    }

    fn info_data() -> Vec<u8> {
        VecWriter::with_capacity(80)
            .bytes(b"AMAC")
            .bytes(b"APPL")
            .bytes(b"HTLC")
            .u32(0)
            .u32(0)
            .bytes(&[0; 32])
            .bytes(&[0; 16])
            .u16(0)
            .u16(7)
            .bytes(b"Hotline")
            .u16(0)
            .finish()
    }

    fn flat(forks: &[Vec<u8>]) -> Vec<u8> {
        let mut w = VecWriter::with_capacity(256)
            .bytes(b"FILP")
            .u16(1)
            .bytes(&[0; 16])
            .u16(forks.len() as u16);
        for f in forks {
            w = w.bytes(f);
        }
        w.finish()
    }

    #[tokio::test]
    async fn read_flat_file_keeps_the_resource_fork_and_info() {
        let wire = flat(&[
            fork(b"INFO", &info_data()),
            fork(b"DATA", b"data fork"),
            fork(b"MACR", b"rsrc"),
        ]);
        let (mut data, mut rsrc) = (Vec::new(), Vec::new());
        let received = read_flat_file(&mut &wire[..], &mut data, &mut rsrc, text_encoding::DEFAULT)
            .await
            .unwrap();
        assert_eq!(data, b"data fork");
        assert_eq!(rsrc, b"rsrc");
        assert_eq!((received.data_len, received.rsrc_len), (9, 4));
        let info = received.info.unwrap();
        assert_eq!((&info.file_type, &info.creator), (b"APPL", b"HTLC"));
        assert_eq!(&*info.name, "Hotline");
    }

    #[tokio::test]
    async fn read_flat_file_decodes_the_start_of_a_long_info_fork() {
        let mut info = info_data();
        info.resize(FlatFileInfo::MAX_LEN + 100, 0);
        let wire = flat(&[fork(b"INFO", &info), fork(b"DATA", b"data fork")]);
        let (mut data, mut rsrc) = (Vec::new(), Vec::new());
        let received = read_flat_file(&mut &wire[..], &mut data, &mut rsrc, text_encoding::DEFAULT)
            .await
            .unwrap();
        assert_eq!(data, b"data fork");
        assert_eq!(&*received.info.unwrap().name, "Hotline");
    }

    #[tokio::test]
    async fn read_flat_file_fails_on_a_cut_off_fork() {
        let mut wire = flat(&[fork(b"DATA", b"data fork")]);
        wire.truncate(wire.len() - 3);
        let (mut data, mut rsrc) = (Vec::new(), Vec::new());
        assert!(
            read_flat_file(&mut &wire[..], &mut data, &mut rsrc, text_encoding::DEFAULT)
                .await
                .is_err()
        );
    }
}
