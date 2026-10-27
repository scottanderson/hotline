//! [`FileNameWithInfo`]: the packed `myField_FileNameWithInfo` (200) record.

use crate::error::{ProtoError, Result};
use crate::text_encoding::{self, Encoding};
use crate::wire::{PacketReader, SliceReader, VecWriter};

/// The type code of a file whose upload has not finished.
pub const PARTIAL_TYPE: [u8; 4] = *b"HTft";

/// The creator code of a file whose upload has not finished.
pub const PARTIAL_CREATOR: [u8; 4] = *b"HTLC";

/// The packed [`myField_FileNameWithInfo`] (200) record from
/// [`myTran_GetFileNameList`] (200) replies.
///
/// [`myField_FileNameWithInfo`]: crate::FieldId::FileNameWithInfo
/// [`myTran_GetFileNameList`]: crate::Opcode::GetFileNameList
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNameWithInfo {
    /// 4-character Mac type code, or `"fldr"` for a folder.
    pub file_type: [u8; 4],
    /// 4-character Mac creator code.
    pub creator: [u8; 4],
    /// A file's size in bytes, or the number of entries in a folder.
    pub size: u32,
    /// File/folder name.
    pub name: Box<str>,
}

impl FileNameWithInfo {
    /// Byte length of the fixed part, before the variable-length name:
    /// `file_type(4) | creator(4) | size(4) | reserved(4) |
    /// name_script(2) | name_len(2)`.
    const LEN: usize = 20;

    /// Encode to the packed wire representation.
    pub fn encode(&self, encoding: &'static Encoding) -> Vec<u8> {
        let name_bytes = text_encoding::encode(encoding, &self.name);
        VecWriter::with_capacity(Self::LEN + name_bytes.len())
            .bytes(&self.file_type)
            .bytes(&self.creator)
            .u32(self.size)
            .u32(0) // reserved
            .u16(0) // name script
            .u16(name_bytes.len() as u16)
            .bytes(&name_bytes)
            .finish()
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let fixed: [u8; Self::LEN] = SliceReader::new(buf).array()?;
        let mut r = PacketReader::new(fixed);
        let file_type = r.take();
        let creator = r.take();
        let size = r.u32();
        let _reserved = r.u32();
        let _name_script = r.u16();
        let name_len = r.u16();
        r.finish();
        let name_len = name_len as usize;
        let name_end = Self::LEN + name_len;
        if buf.len() < name_end {
            return Err(ProtoError::UnexpectedEof {
                needed: name_end,
                available: buf.len(),
            });
        }
        let name = text_encoding::decode(encoding, &buf[Self::LEN..name_end]);
        Ok(Self {
            file_type,
            creator,
            size,
            name: name.into(),
        })
    }

    /// `true` if the type and creator mark an upload that has not finished.
    pub fn is_partial_upload(&self) -> bool {
        self.file_type == PARTIAL_TYPE && self.creator == PARTIAL_CREATOR
    }

    /// `true` if `file_type == "fldr"`.
    pub fn is_folder(&self) -> bool {
        &self.file_type == b"fldr"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_partial_upload_needs_both_codes() {
        let mut file = FileNameWithInfo {
            file_type: PARTIAL_TYPE,
            creator: PARTIAL_CREATOR,
            size: 0,
            name: "a".into(),
        };
        assert!(file.is_partial_upload());
        file.creator = *b"ttxt";
        assert!(!file.is_partial_upload());
    }

    #[test]
    fn file_name_with_info_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let file = FileNameWithInfo {
            file_type: *b"fldr",
            creator: *b"\0\0\0\0",
            size: 0,
            name: "Uploads".into(),
        };
        let bytes = file.encode(encoding);
        let decoded = FileNameWithInfo::decode(&bytes, encoding).unwrap();
        assert_eq!(decoded, file);
        assert!(decoded.is_folder());
    }
}
