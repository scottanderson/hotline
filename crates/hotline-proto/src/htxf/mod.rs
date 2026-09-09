//! The `HTXF` file transfer handshake, sent as the first bytes on the
//! second (data) connection a client opens after `FILE_GET`/`FILE_PUT`
//! reports its `port + 1` transfer port, plus the flattened file object
//! format both sides frame file bytes in on that connection.

mod fork_header;
mod header;

pub use fork_header::ForkHeader;
pub use header::{HtxfHeader, HtxfType};

use crate::error::Result;
use crate::wire::{PacketReader, PacketWriter, VecWriter};

pub(crate) const MAGIC: &[u8; 4] = b"HTXF";

/// Byte length of the `INFO` fork's data (platform through comment size,
/// no name or comment): `4+4+4+4+4+32+8+8+2+2+2`.
pub(crate) const INFO_DATA_LEN: usize = 74;

/// Fixed byte length of [`flat_file_header`]'s output: `FILP` header
/// (24 bytes) + `INFO` fork header (16) + `INFO` fork data (74, no name
/// or comment) + `DATA` fork header (16).
pub const FLAT_FILE_HEADER_LEN: usize = 24 + 16 + INFO_DATA_LEN + 16;

/// Build the flattened-file-object header written before a file's raw
/// bytes on a download: a `FILP` container with an `INFO` fork (type and
/// creator zeroed out, no real Mac file metadata) and a `DATA` fork
/// header. No resource fork.
///
/// `data_size` is the number of raw file bytes that follow this header
/// on the wire.
pub fn flat_file_header(data_size: u32) -> [u8; FLAT_FILE_HEADER_LEN] {
    PacketWriter::new()
        // Flat file header.
        .bytes(b"FILP")
        .u16(1) // version
        .bytes(&[0; 16]) // reserved
        .u16(2) // fork count
        // INFO fork header.
        .bytes(b"INFO")
        .u32(0) // compression type
        .u32(0) // reserved
        .u32(INFO_DATA_LEN as u32) // INFO fork data size
        // INFO fork data.
        .bytes(b"AMAC") // platform
        .u32(0) // type signature
        .u32(0) // creator signature
        .u32(0) // flags
        .u32(0) // platform flags
        .bytes(&[0; 32]) // reserved
        .u64(0) // create date
        .u64(0) // modify date
        .u16(0) // name script
        .u16(0) // name size (name omitted, matching openline's
        // placeholder-only INFO fork for a plain file transfer)
        .u16(0) // comment size (comment omitted)
        // DATA fork header.
        .bytes(b"DATA")
        .u32(0) // compression type
        .u32(0) // reserved
        .u32(data_size)
        .finish()
}

/// Encode a complete flattened file object: [`flat_file_header`] followed
/// by the raw file bytes.
pub fn encode_flat_file(data: &[u8]) -> Vec<u8> {
    let mut w = VecWriter::with_capacity(FLAT_FILE_HEADER_LEN + data.len());
    w = w.bytes(&flat_file_header(data.len() as u32));
    w.bytes(data).finish()
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

    if buf.len() < FLAT_FILE_PREFIX_LEN {
        return Err(ProtoError::UnexpectedEof {
            needed: FLAT_FILE_PREFIX_LEN,
            available: buf.len(),
        });
    }
    let prefix: [u8; FLAT_FILE_PREFIX_LEN] = buf[..FLAT_FILE_PREFIX_LEN].try_into().unwrap();
    let mut r = PacketReader::new(prefix);
    let magic = r.take::<4>();
    let version = r.u16();
    let reserved = r.take::<16>();
    let fork_count = r.u16();
    r.finish();
    debug_assert_eq!(version, 1);
    debug_assert_eq!(reserved, [0; 16]);
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
        let header = flat_file_header(5);
        assert_eq!(&header[0..4], b"FILP");
        assert_eq!(u16::from_be_bytes([header[22], header[23]]), 2);
        assert_eq!(&header[24..28], b"INFO");
        assert_eq!(&header[114..118], b"DATA");
        assert_eq!(u32::from_be_bytes(header[126..130].try_into().unwrap()), 5);
    }

    #[test]
    fn encode_flat_file_appends_raw_data_after_the_header() {
        let encoded = encode_flat_file(b"hello");
        assert_eq!(encoded.len(), FLAT_FILE_HEADER_LEN + 5);
        assert_eq!(&encoded[FLAT_FILE_HEADER_LEN..], b"hello");
    }

    #[test]
    fn decode_flat_file_prefix_reads_back_fork_count() {
        let header = flat_file_header(5);
        assert_eq!(decode_flat_file_prefix(&header).unwrap(), 2);
    }

    #[test]
    fn decode_flat_file_prefix_rejects_bad_magic() {
        let mut header = flat_file_header(0);
        header[0] = b'X';
        assert!(matches!(
            decode_flat_file_prefix(&header),
            Err(ProtoError::BadMagic { .. })
        ));
    }
}
