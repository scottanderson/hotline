//! Composite structures carried inside field payloads: dates, paths, and
//! the packed file/user list-entry records.

use crate::error::{ProtoError, Result};
use crate::wire::{PacketReader, PacketWriter, VecWriter};

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
    /// Wire length in bytes: `year(2) | milliseconds(2) | seconds(4)`.
    pub const LEN: usize = 8;

    /// Encode as the 8-byte wire representation.
    pub fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .u16(self.year)
            .u16(self.milliseconds)
            .u32(self.seconds)
            .finish()
    }

    /// Decode from an 8-byte buffer.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let buf: [u8; Self::LEN] = buf.get(..Self::LEN).and_then(|b| b.try_into().ok()).ok_or(
            ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            },
        )?;
        let mut r = PacketReader::new(buf);
        let year = r.u16();
        let milliseconds = r.u16();
        let seconds = r.u32();
        r.finish();
        Ok(Self {
            year,
            milliseconds,
            seconds,
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
    /// Decoder-side safety bound on component count, well beyond any real
    /// client's directory depth, to avoid an oversized allocation from a
    /// malformed `levels` count.
    const MAX_COMPONENTS: usize = 256;

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
        if levels > Self::MAX_COMPONENTS {
            return Err(ProtoError::Malformed(format!(
                "path has {levels} components, more than the max of {}",
                Self::MAX_COMPONENTS
            )));
        }
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
    /// Byte length of the fixed part, before the variable-length name:
    /// `file_type(4) | creator(4) | size(4) | reserved(4) |
    /// name_script(2) | name_len(2)`.
    const LEN: usize = 20;

    /// Encode to the packed wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        VecWriter::with_capacity(Self::LEN + name_bytes.len())
            .bytes(&self.file_type)
            .bytes(&self.creator)
            .u32(self.size)
            .u32(0) // reserved
            .u16(0) // name script
            .u16(name_bytes.len() as u16)
            .bytes(name_bytes)
            .finish()
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            });
        }
        let fixed: [u8; Self::LEN] = buf[..Self::LEN].try_into().unwrap();
        let mut r = PacketReader::new(fixed);
        let file_type = r.take::<4>();
        let creator = r.take::<4>();
        let size = r.u32();
        let reserved = r.u32();
        let name_script = r.u16();
        let name_len = r.u16();
        r.finish();
        debug_assert_eq!(reserved, 0);
        debug_assert_eq!(name_script, 0);
        let name_len = name_len as usize;
        let name_end = Self::LEN + name_len;
        if buf.len() < name_end {
            return Err(ProtoError::UnexpectedEof {
                needed: name_end,
                available: buf.len(),
            });
        }
        let name = String::from_utf8_lossy(&buf[Self::LEN..name_end]).into_owned();
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
    /// Byte length of the fixed part, before the variable-length name:
    /// `user_id(2) | icon(2) | flags(2) | name_len(2)`.
    const LEN: usize = 8;

    /// Encode to the packed wire representation.
    pub fn encode(&self) -> Vec<u8> {
        let name_bytes = self.name.as_bytes();
        VecWriter::with_capacity(Self::LEN + name_bytes.len())
            .u16(self.user_id)
            .u16(self.icon)
            .u16(self.flags)
            .u16(name_bytes.len() as u16)
            .bytes(name_bytes)
            .finish()
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            });
        }
        let fixed: [u8; Self::LEN] = buf[..Self::LEN].try_into().unwrap();
        let mut r = PacketReader::new(fixed);
        let user_id = r.u16();
        let icon = r.u16();
        let flags = r.u16();
        let name_len = r.u16();
        r.finish();
        let name_len = name_len as usize;
        let end = Self::LEN + name_len;
        if buf.len() < end {
            return Err(ProtoError::UnexpectedEof {
                needed: end,
                available: buf.len(),
            });
        }
        let name = String::from_utf8_lossy(&buf[Self::LEN..end]).into_owned();
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
        /// User holds every currently-defined `USER_ACCESS` privilege.
        const ALL_PRIVS           = 1 << 4;
        /// User holds no `USER_ACCESS` privileges at all.
        const NO_PRIVS            = 1 << 5;
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
    fn path_decode_rejects_too_many_components() {
        let mut buf = ((HlPath::MAX_COMPONENTS + 1) as u16).to_be_bytes().to_vec();
        buf.extend(std::iter::repeat_n(0u8, HlPath::MAX_COMPONENTS + 1));
        assert!(matches!(
            HlPath::decode(&buf),
            Err(ProtoError::Malformed(_))
        ));
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
