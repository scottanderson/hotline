//! Composite structures carried inside field payloads: dates, paths, and
//! the packed file/user list-entry records.

use crate::error::{ProtoError, Result};
use crate::text_encoding::{self, Encoding};
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
        let buf: [u8; Self::LEN] = buf
            .get(..Self::LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            })?;
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
    ///
    /// Fails if any component name is longer than 255 bytes, since each
    /// is written with a 1-byte length prefix.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let mut w = VecWriter::new()
            .u16(self.components.len() as u16)
            .with_encoding(encoding);
        for name in &self.components {
            w = w.u16(0).pstring(name)?;
        }
        Ok(w.finish())
    }

    /// Decode a path from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
        const LEVELS_LEN: usize = 2;
        let prefix: [u8; LEVELS_LEN] = buf
            .get(..LEVELS_LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: LEVELS_LEN,
                available: buf.len(),
            })?;
        let mut r = PacketReader::new(prefix);
        let levels = r.u16() as usize;
        r.finish();
        if levels > Self::MAX_COMPONENTS {
            return Err(ProtoError::Malformed(format!(
                "path has {levels} components, more than the max of {}",
                Self::MAX_COMPONENTS
            )));
        }
        let mut offset = LEVELS_LEN;
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
            let name = text_encoding::decode(encoding, &buf[offset..offset + name_len]);
            components.push(name);
            offset += name_len;
        }
        Ok(Self { components })
    }
}

/// The packed `FILE_NAME_WITH_INFO` (200) record from `myTran_GetFileNameList`
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
        let name = text_encoding::decode(encoding, &buf[Self::LEN..name_end]);
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

/// One entry of a `NEWS_CATEGORY_LIST_DATA_15` (323) news bundle/category
/// listing: a plain folder, or a leaf category holding a thread of
/// articles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewsCategoryListEntry {
    /// A folder containing other bundles/categories.
    Bundle {
        /// Number of items directly inside this bundle.
        count: u16,
        /// Bundle name.
        name: String,
    },
    /// A leaf category holding a thread of articles.
    Category {
        /// Number of articles currently in this category's thread.
        count: u16,
        /// The category's persistent identifier.
        guid: [u8; 16],
        /// Incremented each time an article is added to this category.
        add_sn: u32,
        /// This category's deletion serial number.
        del_sn: u32,
        /// Category name.
        name: String,
    },
}

impl NewsCategoryListEntry {
    /// Encode to the packed wire representation.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(match self {
            Self::Bundle { count, name } => VecWriter::new()
                .u16(2)
                .u16(*count)
                .with_encoding(encoding)
                .pstring(name)?
                .finish(),
            Self::Category {
                count,
                guid,
                add_sn,
                del_sn,
                name,
            } => VecWriter::new()
                .u16(3)
                .u16(*count)
                .bytes(guid)
                .u32(*add_sn)
                .u32(*del_sn)
                .with_encoding(encoding)
                .pstring(name)?
                .finish(),
        })
    }
}

/// One article summary inside a `NEWS_ARTICLE_LIST_DATA` (321) field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsArticleSummary {
    /// Article ID.
    pub id: u32,
    /// Post date.
    pub date: HlDate,
    /// Parent article ID in the thread, `0` for a top-level article.
    pub parent_id: u32,
    /// Article flags, as set on post.
    pub flags: u32,
    /// Article title.
    pub title: String,
    /// Article poster name.
    pub poster: String,
    /// This article's stored MIME type, e.g. `"text/plain"`.
    pub flavor: String,
    /// This article's data size in bytes for `flavor`.
    pub flavor_size: u16,
}

impl NewsArticleSummary {
    /// Encode to the packed wire representation; only `title`/`poster`/
    /// `flavor` are text.
    fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(VecWriter::new()
            .u32(self.id)
            .bytes(&self.date.encode())
            .u32(self.parent_id)
            .u32(self.flags)
            .u16(1) // flavor count: this server stores one flavor per article
            .with_encoding(encoding)
            .pstring(&self.title)?
            .pstring(&self.poster)?
            .pstring(&self.flavor)?
            .u16(self.flavor_size)
            .finish())
    }
}

/// The whole `NEWS_ARTICLE_LIST_DATA` (321) field: one category's thread,
/// flattened into listing order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewsArticleListData {
    /// Every article in the category, in listing order.
    pub articles: Vec<NewsArticleSummary>,
}

impl NewsArticleListData {
    /// Encode to the packed wire representation.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let mut w = VecWriter::new()
            .u32(0) // category ID, unused by the client
            .u32(self.articles.len() as u32)
            .with_encoding(encoding)
            .pstring("")? // name, unused by the client
            .pstring("")?; // description, unused by the client
        for article in &self.articles {
            w = w.bytes(&article.encode(encoding)?);
        }
        Ok(w.finish())
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
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::new(["Games", "Emulators"]);
        let bytes = path.encode(encoding).unwrap();
        assert_eq!(HlPath::decode(&bytes, encoding).unwrap(), path);
    }

    #[test]
    fn empty_path_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::default();
        assert_eq!(
            HlPath::decode(&path.encode(encoding).unwrap(), encoding).unwrap(),
            path
        );
    }

    #[test]
    fn path_encode_rejects_an_oversized_component() {
        let encoding = text_encoding::DEFAULT;
        let path = HlPath::new(["x".repeat(256)]);
        assert!(matches!(
            path.encode(encoding),
            Err(ProtoError::StringTooLong(_))
        ));
    }

    #[test]
    fn path_decode_rejects_too_many_components() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = ((HlPath::MAX_COMPONENTS + 1) as u16).to_be_bytes().to_vec();
        buf.extend(std::iter::repeat_n(0u8, HlPath::MAX_COMPONENTS + 1));
        assert!(matches!(
            HlPath::decode(&buf, encoding),
            Err(ProtoError::Malformed(_))
        ));
    }

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

    #[test]
    fn file_name_with_info_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let file = FileNameWithInfo {
            file_type: *b"fldr",
            creator: *b"\0\0\0\0",
            size: 0,
            name: "Uploads".to_string(),
        };
        let bytes = file.encode(encoding);
        let decoded = FileNameWithInfo::decode(&bytes, encoding).unwrap();
        assert_eq!(decoded, file);
        assert!(decoded.is_folder());
    }
}
