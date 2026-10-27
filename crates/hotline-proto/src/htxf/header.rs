//! [`HtxfHeader`]: the 16-byte header sent first on the transfer connection.

use num_enum::{FromPrimitive, IntoPrimitive};

use crate::error::{ProtoError, Result};
use crate::htxf::MAGIC;
use crate::wire::{PacketReader, PacketWriter, SliceReader};

/// Transfer type carried in [`HtxfHeader::transfer_type`].
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum HtxfType {
    /// A single file.
    File = 0,

    /// A folder, [`myTran_DownloadFldr`] (210) or [`myTran_UploadFldr`] (213).
    ///
    /// [`myTran_DownloadFldr`]: crate::Opcode::DownloadFldr
    /// [`myTran_UploadFldr`]: crate::Opcode::UploadFldr
    Folder = 1,

    /// The server banner image, [`myTran_DownloadBanner`] (212).
    ///
    /// [`myTran_DownloadBanner`]: crate::Opcode::DownloadBanner
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
    /// Reference number from the [`myTran_DownloadFile`] (202) or
    /// [`myTran_UploadFile`] (203) reply this
    /// connection is completing.
    ///
    /// [`myTran_DownloadFile`]: crate::Opcode::DownloadFile
    /// [`myTran_UploadFile`]: crate::Opcode::UploadFile
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
        let buf: [u8; Self::LEN] = SliceReader::new(buf).array()?;
        let mut r = PacketReader::new(buf);
        let magic = r.take();
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
