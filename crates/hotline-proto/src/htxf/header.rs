//! [`HtxfHeader`]: the 16-byte header sent first on the transfer connection.

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::error::{ProtoError, Result};
use crate::htxf::MAGIC;
use crate::wire::{PacketReader, PacketWriter};

/// Transfer type carried in [`HtxfHeader::transfer_type`]. Folder
/// transfers use another wire value not implemented here.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum HtxfType {
    /// A single file.
    File = 0,

    /// The server banner image (`myTran_DownloadBanner`, 212).
    Banner = 2,

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
    /// Transfer type, e.g. [`HtxfType::File`].
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn htxf_header_round_trips() {
        let header = HtxfHeader {
            reference: 0xdead_beef,
            len: 1234,
            transfer_type: HtxfType::File,
        };
        let bytes = header.encode();
        assert_eq!(HtxfHeader::decode(&bytes).unwrap(), header);
    }

    #[test]
    fn htxf_header_rejects_bad_magic() {
        let mut bytes = HtxfHeader {
            reference: 1,
            len: 0,
            transfer_type: HtxfType::File,
        }
        .encode();
        bytes[0] = b'X';
        assert!(matches!(
            HtxfHeader::decode(&bytes),
            Err(ProtoError::BadMagic { .. })
        ));
    }
}
