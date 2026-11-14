//! [`myTran_DownloadFldr`] (210)/[`myTran_UploadFldr`] (213) item stepping: the
//! per-item header sent before each file/folder, and the action a
//! receiver replies with to control what happens next.
//!
//! [`myTran_DownloadFldr`]: crate::Opcode::DownloadFldr
//! [`myTran_UploadFldr`]: crate::Opcode::UploadFldr

use crate::error::Result;
use crate::text_encoding::Encoding;
use crate::types::HlPath;
use crate::wire::{ArrayReader, SliceReader, VecWriter};
use num_enum::{FromPrimitive, IntoPrimitive};

/// Action a folder transfer's receiver sends to control what the sender
/// does with the item it was just told about.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq, FromPrimitive, IntoPrimitive)]
#[repr(u16)]
pub enum FolderAction {
    /// Start sending/receiving this item's data (1).
    /// Original: `dlFldrAction_SendFile`.
    SendFile = 1,
    /// Resume this item's data from `File resume data` (2).
    /// Original: `dlFldrAction_ResumeFile`.
    ResumeFile = 2,
    /// Skip this item; advance to the next one (3).
    /// Original: `dlFldrAction_NextFile`.
    NextFile = 3,
    /// Not recognized by this build.
    #[num_enum(catch_all)]
    Unknown(u16),
}

/// One item's header on a folder transfer connection: a folder flag and
/// the item's path from the root folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderItemHeader {
    /// Whether this item is a folder (no data follows) or a file. Any
    /// odd flag value decodes as a folder.
    pub is_folder: bool,
    /// Path from the transfer's root to this item, root not included.
    pub path: HlPath,
}

impl FolderItemHeader {
    /// `is_folder(2)`, the fixed prefix of the body before `path`.
    const LEN: usize = 2;

    /// Encode the full wire form: `size(2) | is_folder(2) | path`,
    /// `size` covering everything after itself.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let path_bytes = self.path.encode(encoding)?;
        let body_len = path_bytes.len().saturating_add(Self::LEN);
        let w = VecWriter::with_capacity(body_len.saturating_add(2))
            .u16(body_len as u16)
            .u16(u16::from(self.is_folder))
            .bytes(&path_bytes);
        Ok(w.finish())
    }

    /// Decode `is_folder(2) | path` from `body`, the bytes already read
    /// past the wire form's own leading `size` field.
    pub fn decode_body(body: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let mut slice = SliceReader::new(body);
        let mut r = ArrayReader::new(slice.array::<{ Self::LEN }>()?);
        let is_folder = r.u16() & 1 != 0;
        r.finish();
        let path = HlPath::decode(slice.rest(), encoding)?;
        Ok(Self { is_folder, path })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_encoding;

    #[test]
    fn folder_item_header_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let header = FolderItemHeader {
            is_folder: true,
            path: HlPath::new(["Sub", "Deeper"]),
        };
        let bytes = header.encode(encoding).unwrap();
        // `size` covers everything after itself.
        let size = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
        assert_eq!(bytes.len(), 2 + size);
        let decoded = FolderItemHeader::decode_body(&bytes[2..], encoding).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn folder_item_header_round_trips_a_file() {
        let encoding = text_encoding::DEFAULT;
        let header = FolderItemHeader {
            is_folder: false,
            path: HlPath::new(["a.txt"]),
        };
        let bytes = header.encode(encoding).unwrap();
        let decoded = FolderItemHeader::decode_body(&bytes[2..], encoding).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn folder_item_header_treats_an_even_flag_as_a_file() {
        let encoding = text_encoding::DEFAULT;
        let mut body = 2u16.to_be_bytes().to_vec();
        body.extend(HlPath::new(["a.txt"]).encode(encoding).unwrap());
        let decoded = FolderItemHeader::decode_body(&body, encoding).unwrap();
        assert!(!decoded.is_folder);
    }

    #[test]
    fn folder_action_round_trips_known_values() {
        assert_eq!(FolderAction::from(1u16), FolderAction::SendFile);
        assert_eq!(FolderAction::from(2u16), FolderAction::ResumeFile);
        assert_eq!(FolderAction::from(3u16), FolderAction::NextFile);
        assert_eq!(FolderAction::from(99u16), FolderAction::Unknown(99));
    }
}
