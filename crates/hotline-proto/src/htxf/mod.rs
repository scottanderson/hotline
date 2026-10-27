//! The `HTXF` file transfer handshake, sent as the first bytes on the second
//! (data) connection a client opens after [`myTran_DownloadFile`] (202) or
//! [`myTran_UploadFile`] (203) reports its `port + 1` transfer port, plus the
//! flattened file object format both sides frame file bytes in on that
//! connection.
//!
//! [`myTran_DownloadFile`]: crate::Opcode::DownloadFile
//! [`myTran_UploadFile`]: crate::Opcode::UploadFile

mod flat_file_info;
mod folder;
mod fork_header;
mod header;

pub use flat_file_info::FlatFileInfo;
pub use folder::{FolderAction, FolderItemHeader};
pub use fork_header::ForkHeader;
pub use header::{HtxfHeader, HtxfType};

use crate::error::Result;
use crate::wire::{PacketReader, PacketWriter, SliceReader, VecWriter};

pub(crate) const MAGIC: &[u8; 4] = b"HTXF";

/// Byte length of [`empty_info`]: [`FlatFileInfo::FIXED_LEN`] and a
/// comment size.
pub const INFO_DATA_LEN: usize = FlatFileInfo::FIXED_LEN + 2;

/// The `INFO` fork data of [`FlatFileInfo::default`]: platform `AMAC`,
/// with no type, creator, name, or comment.
pub fn empty_info() -> [u8; INFO_DATA_LEN] {
    PacketWriter::new()
        .bytes(b"AMAC")
        .bytes(&[0; INFO_DATA_LEN - 4])
        .finish()
}

/// Byte length of [`flat_file_header`]'s output for `info_len` bytes of
/// `INFO` fork data.
pub const fn flat_file_header_len(info_len: usize) -> usize {
    FLAT_FILE_PREFIX_LEN + ForkHeader::LEN + info_len + ForkHeader::LEN
}

/// Byte length of [`flat_file_header`]'s output with [`empty_info`].
pub const FLAT_FILE_HEADER_LEN: usize = flat_file_header_len(INFO_DATA_LEN);

/// The bytes before a file's data in a flattened file object: the `FILP`
/// header, an `INFO` fork with `info` as its data, and the `DATA` fork
/// header for `data_size` bytes.
pub fn flat_file_header(info: &[u8], data_size: u32) -> Vec<u8> {
    flat_header(2, info, data_size)
}

/// [`flat_file_header`] with a fork count of 3. The `MACR` fork follows
/// the data.
pub fn flat_file_header_with_resource(info: &[u8], data_size: u32) -> Vec<u8> {
    flat_header(3, info, data_size)
}

fn flat_header(fork_count: u16, info: &[u8], data_size: u32) -> Vec<u8> {
    let info_header = ForkHeader {
        fork_type: *b"INFO",
        data_size: info.len() as u32,
    };
    let data_header = ForkHeader {
        fork_type: *b"DATA",
        data_size,
    };
    VecWriter::with_capacity(flat_file_header_len(info.len()))
        .bytes(b"FILP")
        .u16(1) // version
        .bytes(&[0; 16]) // reserved
        .u16(fork_count)
        .bytes(&info_header.encode())
        .bytes(info)
        .bytes(&data_header.encode())
        .finish()
}

/// Encode a complete flattened file object with [`empty_info`]:
/// [`flat_file_header`] followed by the raw file bytes.
pub fn encode_flat_file(data: &[u8]) -> Vec<u8> {
    let mut flat = flat_file_header(&empty_info(), data.len() as u32);
    flat.extend_from_slice(data);
    flat
}

/// Byte length of the `FILP` header alone (format, version, reserved,
/// fork count) before the per-fork headers/data start.
pub const FLAT_FILE_PREFIX_LEN: usize = 24;

/// Decode a flattened file object's leading `FILP` header, returning the
/// fork count that follows. Doesn't read the forks themselves, since an
/// upload's fork data can be arbitrarily large and arrives incrementally
/// over the wire.
pub fn decode_flat_file_prefix(buf: &[u8]) -> Result<u16> {
    use crate::error::ProtoError;

    let prefix: [u8; FLAT_FILE_PREFIX_LEN] = SliceReader::new(buf).array()?;
    let mut r = PacketReader::new(prefix);
    let magic = r.take::<4>();
    let _version = r.u16();
    let _reserved = r.take::<16>();
    let fork_count = r.u16();
    r.finish();
    if magic != *b"FILP" {
        return Err(ProtoError::BadMagic {
            expected: *b"FILP",
            got: magic,
        });
    }
    Ok(fork_count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ProtoError;

    #[test]
    fn flat_file_header_has_filp_magic_and_two_forks() {
        let header = flat_file_header(&empty_info(), 5);
        assert_eq!(header.len(), FLAT_FILE_HEADER_LEN);
        assert_eq!(&header[0..4], b"FILP");
        assert_eq!(u16::from_be_bytes([header[22], header[23]]), 2);
        assert_eq!(&header[24..28], b"INFO");
        assert_eq!(&header[114..118], b"DATA");
        assert_eq!(u32::from_be_bytes(header[126..130].try_into().unwrap()), 5);
    }

    #[test]
    fn flat_file_header_carries_the_info_data() {
        let header = flat_file_header(b"info", 5);
        assert_eq!(header.len(), flat_file_header_len(4));
        assert_eq!(u32::from_be_bytes(header[36..40].try_into().unwrap()), 4);
        assert_eq!(&header[40..44], b"info");
        assert_eq!(&header[44..48], b"DATA");
    }

    #[test]
    fn flat_file_header_with_resource_has_three_forks() {
        let header = flat_file_header_with_resource(b"info", 5);
        assert_eq!(header.len(), flat_file_header_len(4));
        assert_eq!(u16::from_be_bytes([header[22], header[23]]), 3);
        assert_eq!(&header[44..48], b"DATA");
    }

    #[test]
    fn encode_flat_file_appends_raw_data_after_the_header() {
        let encoded = encode_flat_file(b"hello");
        assert_eq!(encoded.len(), FLAT_FILE_HEADER_LEN + 5);
        assert_eq!(&encoded[FLAT_FILE_HEADER_LEN..], b"hello");
    }

    #[test]
    fn decode_flat_file_prefix_reads_back_fork_count() {
        let header = flat_file_header(&empty_info(), 5);
        assert_eq!(decode_flat_file_prefix(&header).unwrap(), 2);
    }

    #[test]
    fn decode_flat_file_prefix_rejects_bad_magic() {
        let mut header = flat_file_header(&empty_info(), 0);
        header[0] = b'X';
        assert!(matches!(
            decode_flat_file_prefix(&header),
            Err(ProtoError::BadMagic { .. })
        ));
    }
}
