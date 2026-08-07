//! The `HTXF` file transfer handshake, sent as the first bytes on the
//! second (data) connection a client opens after `FILE_GET`/`FILE_PUT`
//! reports its `port + 1` transfer port, plus the flattened file object
//! format both sides frame file bytes in on that connection.

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::error::{ProtoError, Result};
use crate::wire::{PacketReader, PacketWriter, VecWriter};

const MAGIC: &[u8; 4] = b"HTXF";

/// Transfer type carried in [`HtxfHeader::transfer_type`]. Folder and
/// banner transfers use other wire values but aren't implemented here.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum HtxfType {
    /// A single file, the only type this server sends/accepts.
    FileTransfer = 0,

    /// Not recognized by this build. Carries the raw wire value so
    /// callers can still log/forward it.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// The 16-byte header sent first on the transfer connection, both
/// directions: `"HTXF" | ref | len | type | reserved`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HtxfHeader {
    /// Reference number from the `FILE_GET`/`FILE_PUT` reply this
    /// connection is completing.
    pub reference: u32,
    /// `0` for a download; the file's byte count for an upload.
    pub len: u32,
    /// Transfer type, e.g. [`HtxfType::FileTransfer`].
    pub transfer_type: HtxfType,
}

impl HtxfHeader {
    /// Wire length in bytes: `magic(4) | reference(4) | len(4) |
    /// transfer_type(2) | reserved(2)`.
    pub const LEN: usize = 16;

    /// Encode to the wire format.
    pub fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .bytes(MAGIC)
            .u32(self.reference)
            .u32(self.len)
            .u16(u16::from(self.transfer_type))
            .u16(0)
            .finish()
    }

    /// Decode a 16-byte header.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            });
        }
        let buf: [u8; Self::LEN] = buf[..Self::LEN].try_into().unwrap();
        let mut r = PacketReader::new(buf);
        let magic = r.take::<4>();
        let reference = r.u32();
        let len = r.u32();
        let transfer_type = r.u16();
        let reserved = r.u16();
        r.finish();
        if magic != *MAGIC {
            return Err(ProtoError::BadMagic {
                expected: *MAGIC,
                got: magic,
            });
        }
        if reserved != 0 {
            return Err(ProtoError::Malformed(format!(
                "reserved value is set: {reserved}"
            )));
        }
        let transfer_type = HtxfType::from(transfer_type);
        let htxf_header = Self {
            reference,
            len,
            transfer_type,
        };
        Ok(htxf_header)
    }
}

/// Byte length of the `INFO` fork's data (platform through name size, no
/// name): `4+4+4+4+4+32+8+8+2+2`.
const INFO_DATA_LEN: usize = 72;

/// Fixed byte length of [`flat_file_header`]'s output: `FILP` header
/// (24 bytes) + `INFO` fork header (16) + `INFO` fork data (72, no name)
/// + `DATA` fork header (16).
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
        .u16(0) // name size (name omitted, matching the reference's
        // placeholder-only INFO fork for a plain file transfer)
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

/// One fork's header, read between the `FILP` prefix and its data
/// (`INFO`, `DATA`, or `MACR`). Compression is always `0` (uncompressed)
/// in every flattened object this server has seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForkHeader {
    /// Fork tag, e.g. `b"DATA"`.
    pub fork_type: [u8; 4],
    /// Number of bytes of fork data that follow this header.
    pub data_size: u32,
}

impl ForkHeader {
    /// Wire length in bytes: `fork_type(4) | compression_type(4) |
    /// reserved(4) | data_size(4)`.
    pub const LEN: usize = 16;

    /// Decode one fork header.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            });
        }
        let buf: [u8; Self::LEN] = buf[..Self::LEN].try_into().unwrap();
        let mut r = PacketReader::new(buf);
        let fork_type = r.take::<4>();
        let compression_type = r.u32();
        let reserved = r.u32();
        let data_size = r.u32();
        r.finish();
        debug_assert_eq!(compression_type, 0);
        debug_assert_eq!(reserved, 0);
        Ok(Self {
            fork_type,
            data_size,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn htxf_header_round_trips() {
        let header = HtxfHeader {
            reference: 0xdead_beef,
            len: 1234,
            transfer_type: HtxfType::FileTransfer,
        };
        let bytes = header.encode();
        assert_eq!(HtxfHeader::decode(&bytes).unwrap(), header);
    }

    #[test]
    fn htxf_header_rejects_bad_magic() {
        let mut bytes = HtxfHeader {
            reference: 1,
            len: 0,
            transfer_type: HtxfType::FileTransfer,
        }
        .encode();
        bytes[0] = b'X';
        assert!(matches!(
            HtxfHeader::decode(&bytes),
            Err(ProtoError::BadMagic { .. })
        ));
    }

    #[test]
    fn flat_file_header_has_filp_magic_and_two_forks() {
        let header = flat_file_header(5);
        assert_eq!(&header[0..4], b"FILP");
        assert_eq!(u16::from_be_bytes([header[22], header[23]]), 2);
        assert_eq!(&header[24..28], b"INFO");
        assert_eq!(&header[112..116], b"DATA");
        assert_eq!(u32::from_be_bytes(header[124..128].try_into().unwrap()), 5);
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

    #[test]
    fn fork_header_round_trips_through_flat_file_header() {
        let header = flat_file_header(42);
        let info = ForkHeader::decode(&header[FLAT_FILE_PREFIX_LEN..]).unwrap();
        assert_eq!(&info.fork_type, b"INFO");
        assert_eq!(info.data_size, INFO_DATA_LEN as u32);

        let data_fork_offset = FLAT_FILE_PREFIX_LEN + ForkHeader::LEN + INFO_DATA_LEN;
        let data = ForkHeader::decode(&header[data_fork_offset..]).unwrap();
        assert_eq!(&data.fork_type, b"DATA");
        assert_eq!(data.data_size, 42);
    }
}
