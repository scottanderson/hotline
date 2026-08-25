//! [`UserNameWithInfo`]: the packed `USER_NAME_WITH_INFO` (300) record.

use crate::error::{ProtoError, Result};
use crate::text_encoding::{self, Encoding};
use crate::wire::{PacketReader, VecWriter};

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
    pub fn encode(&self, encoding: &'static Encoding) -> Vec<u8> {
        let name_bytes = text_encoding::encode(encoding, &self.name);
        VecWriter::with_capacity(Self::LEN + name_bytes.len())
            .u16(self.user_id)
            .u16(self.icon)
            .u16(self.flags)
            .u16(name_bytes.len() as u16)
            .bytes(&name_bytes)
            .finish()
    }

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
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
        let name = text_encoding::decode(encoding, &buf[Self::LEN..end]);
        Ok(Self {
            user_id,
            icon,
            flags,
            name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UserFlags;

    #[test]
    fn user_name_with_info_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let user = UserNameWithInfo {
            user_id: 7,
            icon: 128,
            flags: UserFlags::AWAY.bits(),
            name: "Virtual1".to_string(),
        };
        let bytes = user.encode(encoding);
        assert_eq!(UserNameWithInfo::decode(&bytes, encoding).unwrap(), user);
    }

    #[test]
    fn user_name_with_info_round_trips_mac_roman_accented_name() {
        let encoding = text_encoding::DEFAULT;
        // e-acute (0x8E) exists in Mac OS Roman, so it round-trips exactly.
        let user = UserNameWithInfo {
            user_id: 1,
            icon: 0,
            flags: 0,
            name: "Ren\u{e9}".to_string(),
        };
        let bytes = user.encode(encoding);
        assert_eq!(
            UserNameWithInfo::decode(&bytes, encoding).unwrap().name,
            "Ren\u{e9}"
        );
    }

    #[test]
    fn user_name_with_info_falls_back_for_text_outside_mac_roman() {
        let encoding = text_encoding::DEFAULT;
        // CJK text doesn't exist in Mac OS Roman, so each character is lossy.
        let user = UserNameWithInfo {
            user_id: 1,
            icon: 0,
            flags: 0,
            name: "日本語".to_string(),
        };
        let bytes = user.encode(encoding);
        assert_eq!(
            UserNameWithInfo::decode(&bytes, encoding).unwrap().name,
            "???"
        );
    }
}
