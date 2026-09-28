//! Composite structures carried inside field payloads: dates, paths, and
//! the packed file/user list-entry records.

mod date;
mod file_name;
mod news_article_list_data;
mod news_article_summary;
mod news_category_list_entry;
mod path;
mod user_flags;
mod user_name;

pub use date::HlDate;
pub use file_name::FileNameWithInfo;
pub use news_article_list_data::NewsArticleListData;
pub use news_article_summary::NewsArticleSummary;
pub use news_category_list_entry::NewsCategoryListEntry;
pub use path::HlPath;
pub use user_flags::UserFlags;
pub use user_name::UserNameWithInfo;

use crate::error::{ProtoError, Result};
use crate::wire::{PacketReader, PacketWriter};

/// Byte length of [`decode_resume_data`]'s fixed prefix, before its
/// per-fork entries: `format(4) | version(2) | reserved(34) |
/// fork_count(2)`.
const RESUME_DATA_PREFIX_LEN: usize = 42;

/// Byte length of one fork entry in [`decode_resume_data`]'s fork list:
/// `fork_type(4) | data_size(4) | reserved(4) | reserved(4)`.
const RESUME_DATA_ENTRY_LEN: usize = 16;

/// Byte length of [`encode_resume_data`]'s output: the fixed prefix plus
/// its two fork entries (`DATA`, `MACR`).
const RESUME_DATA_LEN: usize = RESUME_DATA_PREFIX_LEN + 2 * RESUME_DATA_ENTRY_LEN;

/// Decode a `FileResumeData` blob, [`myField_FileResumeData`] (203): how many
/// bytes of the `DATA` fork the sender already has, so a resumed transfer only
/// sends the remainder. Ignores any other fork entry, e.g. a resource fork
/// position this server doesn't track.
///
/// [`myField_FileResumeData`]: crate::FieldId::FileResumeData
pub fn decode_resume_data(buf: &[u8]) -> Result<u32> {
    if buf.len() < RESUME_DATA_PREFIX_LEN {
        return Err(ProtoError::UnexpectedEof {
            needed: RESUME_DATA_PREFIX_LEN,
            available: buf.len(),
        });
    }
    let prefix: [u8; RESUME_DATA_PREFIX_LEN] = buf[..RESUME_DATA_PREFIX_LEN].try_into().unwrap();
    let mut r = PacketReader::new(prefix);
    let magic = r.take::<4>();
    let version = r.u16();
    let _reserved = r.take::<34>();
    let fork_count = r.u16();
    r.finish();
    if magic != *b"RFLT" {
        return Err(ProtoError::BadMagic {
            expected: *b"RFLT",
            got: magic,
        });
    }
    if version != 1 {
        return Err(ProtoError::Malformed(format!(
            "unsupported RFLT version: {version}"
        )));
    }

    let mut offset = RESUME_DATA_PREFIX_LEN;
    for _ in 0..fork_count {
        if buf.len() < offset + RESUME_DATA_ENTRY_LEN {
            return Err(ProtoError::UnexpectedEof {
                needed: offset + RESUME_DATA_ENTRY_LEN,
                available: buf.len(),
            });
        }
        let entry: [u8; RESUME_DATA_ENTRY_LEN] = buf[offset..offset + RESUME_DATA_ENTRY_LEN]
            .try_into()
            .unwrap();
        let mut er = PacketReader::new(entry);
        let fork_type = er.take::<4>();
        let data_size = er.u32();
        let _reserved = er.take::<8>();
        er.finish();
        if &fork_type == b"DATA" {
            return Ok(data_size);
        }
        offset += RESUME_DATA_ENTRY_LEN;
    }
    Err(ProtoError::Malformed(
        "RFLT resume data has no DATA fork entry".to_string(),
    ))
}

/// Build a `FileResumeData` blob, [`myField_FileResumeData`] (203), claiming
/// `already_have` bytes of the `DATA` fork, plus an always-empty `MACR`
/// fork, the shape [`decode_resume_data`] parses.
///
/// [`myField_FileResumeData`]: crate::FieldId::FileResumeData
pub fn encode_resume_data(already_have: u32) -> [u8; RESUME_DATA_LEN] {
    PacketWriter::new()
        .bytes(b"RFLT")
        .u16(1) // version
        .bytes(&[0; 34]) // reserved
        .u16(2) // fork count
        .bytes(b"DATA")
        .u32(already_have)
        .u32(0) // reserved
        .u32(0) // reserved
        .bytes(b"MACR")
        .u32(0) // resource fork size, always empty
        .u32(0) // reserved
        .u32(0) // reserved
        .finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::VecWriter;

    fn build_resume_data(entries: &[([u8; 4], u32)]) -> Vec<u8> {
        let mut w = VecWriter::with_capacity(RESUME_DATA_PREFIX_LEN + entries.len() * 16);
        w = w
            .bytes(b"RFLT")
            .u16(1)
            .bytes(&[0; 34])
            .u16(entries.len() as u16);
        for (fork_type, data_size) in entries {
            w = w.bytes(fork_type).u32(*data_size).u32(0).u32(0);
        }
        w.finish()
    }

    #[test]
    fn decode_resume_data_finds_the_data_fork_entry() {
        let buf = build_resume_data(&[(*b"DATA", 1234)]);
        assert_eq!(decode_resume_data(&buf).unwrap(), 1234);
    }

    #[test]
    fn decode_resume_data_skips_a_leading_non_data_entry() {
        let buf = build_resume_data(&[(*b"MACR", 99), (*b"DATA", 555)]);
        assert_eq!(decode_resume_data(&buf).unwrap(), 555);
    }

    #[test]
    fn decode_resume_data_rejects_bad_magic() {
        let mut buf = build_resume_data(&[(*b"DATA", 1)]);
        buf[0] = b'X';
        assert!(matches!(
            decode_resume_data(&buf),
            Err(ProtoError::BadMagic { .. })
        ));
    }

    #[test]
    fn decode_resume_data_rejects_missing_data_fork_entry() {
        let buf = build_resume_data(&[(*b"MACR", 99)]);
        assert!(matches!(
            decode_resume_data(&buf),
            Err(ProtoError::Malformed(_))
        ));
    }

    #[test]
    fn encode_resume_data_round_trips_through_decode() {
        let buf = encode_resume_data(4321);
        assert_eq!(decode_resume_data(&buf).unwrap(), 4321);
    }
}
