//! Composite structures carried inside field payloads: dates, paths, and
//! the packed file/user list-entry records.

use crate::error::{ProtoError, Result};
use crate::wire::{FixedEncode, PacketWriter, VecWriter};

/// A Hotline date: `year(2) | milliseconds(2) | seconds-since-midnight(4)`.
/// Used by `FILE_CREATE_DATE`, `FILE_MODIFY_DATE`, `NEWS_ART_DATE`, and the
/// old-guide `datetime` object (base year 1904, Mac HFS style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlDate {
    /// Calendar year, e.g. `1998`.
    pub year: u16,
    /// Milliseconds component (rarely nonzero in practice).
    pub milliseconds: u16,
    /// Seconds since midnight Jan 1 of `year`.
    pub seconds: u32,
}

impl HlDate {
    /// Encode as the 8-byte wire representation.
    pub fn encode(&self) -> [u8; 8] {
        PacketWriter::<8>::new()
            .u16(self.year)
            .u16(self.milliseconds)
            .u32(self.seconds)
            .finish()
    }

    /// Decode from an 8-byte buffer.
    pub const fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 8 {
            return Err(ProtoError::UnexpectedEof {
                needed: 8,
                available: buf.len(),
            });
        }
        Ok(Self {
            year: u16::from_be_bytes([buf[0], buf[1]]),
            milliseconds: u16::from_be_bytes([buf[2], buf[3]]),
            seconds: u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]),
        })
    }
}

/// A file/news path: a sequence of directory-level names from root to leaf.
/// Wire shape per the older guide: `levels(2)` then, per level,
/// `reserved(2)=0 | name_len(1) | name`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HlPath {
    /// Directory components, root-to-leaf (does not include the file name
    /// itself for file operations, that's a separate `FILE_NAME` field).
    pub components: Vec<String>,
}

impl HlPath {
    /// Build a path from components.
    pub fn new(components: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            components: components.into_iter().map(Into::into).collect(),
        }
    }

    /// Encode to bytes suitable for a `FILE_PATH` / `NEWS_PATH` field.
    /// Fails if any component name is longer than 255 bytes, since each
    /// is written with a 1-byte length prefix.
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = VecWriter::new().u16(self.components.len() as u16);
        for name in &self.components {
            w = w.u16(0).pstring(name)?;
        }
        Ok(w.finish())
    }

    /// Decode a path from a field's raw data.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 2 {
            return Err(ProtoError::UnexpectedEof {
                needed: 2,
                available: buf.len(),
            });
        }
        let levels = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        let mut offset = 2;
        let mut components = Vec::with_capacity(levels);
        for _ in 0..levels {
            if buf.len() < offset + 3 {
                return Err(ProtoError::UnexpectedEof {
                    needed: offset + 3,
                    available: buf.len(),
                });
            }
            // 2 reserved bytes, then a 1-byte length prefix (pstring-style).
            let name_len = buf[offset + 2] as usize;
            offset += 3;
            if buf.len() < offset + name_len {
                return Err(ProtoError::UnexpectedEof {
                    needed: offset + name_len,
                    available: buf.len(),
                });
            }
            let name = String::from_utf8_lossy(&buf[offset..offset + name_len]).into_owned();
            components.push(name);
            offset += name_len;
        }
        Ok(Self { components })
    }
}

/// The packed `FILE_NAME_WITH_INFO` (200) record from `HTLC_HDR_FILE_LIST`
/// replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileNameWithInfo {
    /// 4-character Mac type code, or `"fldr"` for a folder.
    pub file_type: [u8; 4],
    /// 4-character Mac creator code.
    pub creator: [u8; 4],
    /// File size in bytes (`0` for folders).
    pub size: u32,
    /// File/folder name.
    pub name: String,
}

impl FileNameWithInfo {
    /// Encode to the packed wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&self.file_type);
        buf.extend_from_slice(&self.creator);
        buf.extend_from_slice(&self.size.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
        buf.extend_from_slice(&0u16.to_be_bytes()); // name script
        let name_bytes = self.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
        buf.extend_from_slice(name_bytes);
        buf
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        const FIXED_LEN: usize = 4 + 4 + 4 + 4 + 2 + 2;
        if buf.len() < FIXED_LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: FIXED_LEN,
                available: buf.len(),
            });
        }
        let file_type = buf[0..4].try_into().unwrap();
        let creator = buf[4..8].try_into().unwrap();
        let size = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        let name_len = u16::from_be_bytes([buf[18], buf[19]]) as usize;
        let name_end = 20 + name_len;
        if buf.len() < name_end {
            return Err(ProtoError::UnexpectedEof {
                needed: name_end,
                available: buf.len(),
            });
        }
        let name = String::from_utf8_lossy(&buf[20..name_end]).into_owned();
        Ok(Self {
            file_type,
            creator,
            size,
            name,
        })
    }

    /// `true` if `file_type == "fldr"`.
    pub fn is_folder(&self) -> bool {
        &self.file_type == b"fldr"
    }
}

/// The packed `USER_NAME_WITH_INFO` (300) record from userlist replies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserNameWithInfo {
    /// Socket / connection ID for this user.
    pub user_id: u16,
    /// Icon ID.
    pub icon: u16,
    /// See `USER_FLAGS` bit meanings (away, admin, refuses PM/chat).
    pub flags: u16,
    /// Display name.
    pub name: String,
}

impl UserNameWithInfo {
    /// Encode to the packed wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8 + self.name.len());
        buf.extend_from_slice(&self.user_id.to_be_bytes());
        buf.extend_from_slice(&self.icon.to_be_bytes());
        buf.extend_from_slice(&self.flags.to_be_bytes());
        let name_bytes = self.name.as_bytes();
        buf.extend_from_slice(&(name_bytes.len() as u16).to_be_bytes());
        buf.extend_from_slice(name_bytes);
        buf
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 8 {
            return Err(ProtoError::UnexpectedEof {
                needed: 8,
                available: buf.len(),
            });
        }
        let user_id = u16::from_be_bytes([buf[0], buf[1]]);
        let icon = u16::from_be_bytes([buf[2], buf[3]]);
        let flags = u16::from_be_bytes([buf[4], buf[5]]);
        let name_len = u16::from_be_bytes([buf[6], buf[7]]) as usize;
        let end = 8 + name_len;
        if buf.len() < end {
            return Err(ProtoError::UnexpectedEof {
                needed: end,
                available: buf.len(),
            });
        }
        let name = String::from_utf8_lossy(&buf[8..end]).into_owned();
        Ok(Self {
            user_id,
            icon,
            flags,
            name,
        })
    }
}

bitflags::bitflags! {
    /// Bit meanings for [`UserNameWithInfo::flags`] / the `USER_FLAGS` field.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct UserFlags: u16 {
        /// User is away/idle.
        const AWAY               = 1 << 0;
        /// User is an admin (or, per the reference's own uncertainty, disconnected).
        const ADMIN               = 1 << 1;
        /// User refuses private messages.
        const REFUSES_PM          = 1 << 2;
        /// User refuses private chat invites.
        const REFUSES_CHAT        = 1 << 3;
    }
}

impl FixedEncode<8> for HlDate {
    fn encode(&self) -> [u8; 8] {
        HlDate::encode(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_round_trips() {
        let path = HlPath::new(["Games", "Emulators"]);
        let bytes = path.encode().unwrap();
        assert_eq!(HlPath::decode(&bytes).unwrap(), path);
    }

    #[test]
    fn empty_path_round_trips() {
        let path = HlPath::default();
        assert_eq!(HlPath::decode(&path.encode().unwrap()).unwrap(), path);
    }

    #[test]
    fn path_encode_rejects_an_oversized_component() {
        let path = HlPath::new(["x".repeat(256)]);
        assert!(matches!(path.encode(), Err(ProtoError::StringTooLong(_))));
    }

    #[test]
    fn user_name_with_info_round_trips() {
        let user = UserNameWithInfo {
            user_id: 7,
            icon: 128,
            flags: UserFlags::AWAY.bits(),
            name: "Virtual1".to_string(),
        };
        let bytes = user.encode();
        assert_eq!(UserNameWithInfo::decode(&bytes).unwrap(), user);
    }

    #[test]
    fn user_name_with_info_decodes_multi_byte_utf8() {
        let user = UserNameWithInfo {
            user_id: 1,
            icon: 0,
            flags: 0,
            name: "日本語".to_string(),
        };
        let bytes = user.encode();
        assert_eq!(UserNameWithInfo::decode(&bytes).unwrap().name, "日本語");
    }

    #[test]
    fn file_name_with_info_round_trips() {
        let file = FileNameWithInfo {
            file_type: *b"fldr",
            creator: *b"\0\0\0\0",
            size: 0,
            name: "Uploads".to_string(),
        };
        let bytes = file.encode();
        let decoded = FileNameWithInfo::decode(&bytes).unwrap();
        assert_eq!(decoded, file);
        assert!(decoded.is_folder());
    }
}
