//! [`FlatFileInfo`]: the `INFO` fork of a flattened file object.

use crate::text_encoding::{self, Encoding};
use crate::types::HlDate;
use crate::wire::{ArrayReader, VecWriter};

/// The `INFO` fork's data: a file's Finder info, dates, name, and comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatFileInfo {
    /// The sender's operating system, `AMAC` or `MWIN`.
    pub platform: [u8; 4],
    /// The Mac file type.
    pub file_type: [u8; 4],
    /// The Mac creator.
    pub creator: [u8; 4],
    /// The Mac Finder flags (`fdFlags`).
    pub platform_flags: u32,
    /// When the file was made.
    pub created: HlDate,
    /// When the file last changed.
    pub modified: HlDate,
    /// The file's name.
    pub name: String,
    /// The file's comment.
    pub comment: String,
}

impl Default for FlatFileInfo {
    fn default() -> Self {
        Self {
            platform: *b"AMAC",
            file_type: [0; 4],
            creator: [0; 4],
            platform_flags: 0,
            created: HlDate::default(),
            modified: HlDate::default(),
            name: String::new(),
            comment: String::new(),
        }
    }
}

impl FlatFileInfo {
    /// Byte length of the fields before the name: platform through name size.
    pub const FIXED_LEN: usize = 72;

    /// The most bytes [`Self::decode`] reads, the same as openline.
    pub const MAX_LEN: usize = Self::FIXED_LEN + 520;

    /// The longest name [`Self::encode`] writes, in bytes.
    pub const MAX_NAME_LEN: usize = 63;

    /// The longest comment, in bytes.
    pub const MAX_COMMENT_LEN: usize = 255;

    /// Encode as `INFO` fork data.
    pub fn encode(&self, encoding: &'static Encoding) -> Vec<u8> {
        let name = text_encoding::encode_truncated(encoding, &self.name, Self::MAX_NAME_LEN);
        let comment =
            text_encoding::encode_truncated(encoding, &self.comment, Self::MAX_COMMENT_LEN);
        VecWriter::with_capacity(
            name.len()
                .saturating_add(comment.len())
                .saturating_add(Self::FIXED_LEN + 2),
        )
        .bytes(&self.platform)
        .bytes(&self.file_type)
        .bytes(&self.creator)
        .u32(0) // flags
        .u32(self.platform_flags)
        .bytes(&[0; 32]) // reserved
        .bytes(&self.created.encode())
        .bytes(&self.modified.encode())
        .u16(0) // name script
        .u16(name.len() as u16)
        .bytes(&name)
        .u16(comment.len() as u16)
        .bytes(&comment)
        .finish()
    }

    /// Decode `INFO` fork data. Reads at most [`Self::MAX_LEN`] bytes, and
    /// a missing byte reads as zero. The comment needs its size field
    /// before the end of `buf`, and stops at the end of `buf`.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Self {
        let buf = buf.get(..Self::MAX_LEN).unwrap_or(buf);
        let len = buf.len();
        // Two more bytes so a comment size at the last byte reads in full.
        let mut data = [0u8; Self::MAX_LEN + 2];
        data.iter_mut().zip(buf).for_each(|(d, &b)| *d = b);

        let fixed = data.first_chunk().copied().unwrap_or([0; Self::FIXED_LEN]);
        let mut r = ArrayReader::new(fixed);
        let platform = r.take();
        let file_type = r.take();
        let creator = r.take();
        let _flags = r.u32();
        let platform_flags = r.u32();
        let _reserved = r.take::<32>();
        let created = HlDate::decode(r.take());
        let modified = HlDate::decode(r.take());
        let _name_script = r.u16();
        let name_size = r.u16() as usize;
        r.finish();

        let name_end = name_size.saturating_add(Self::FIXED_LEN);
        let name = data
            .get(Self::FIXED_LEN..name_end.min(len))
            .unwrap_or_default();
        let comment: &[u8] = if name_end < len {
            let size = data
                .get(name_end..)
                .and_then(<[u8]>::first_chunk)
                .map_or(0, |&size| usize::from(u16::from_be_bytes(size)));
            let start = name_end.saturating_add(2);
            let end = start
                .saturating_add(size.min(Self::MAX_COMMENT_LEN))
                .min(len);
            data.get(start..end).unwrap_or_default()
        } else {
            &[]
        };
        Self {
            platform,
            file_type,
            creator,
            platform_flags,
            created,
            modified,
            name: text_encoding::decode(encoding, name),
            comment: text_encoding::decode(encoding, comment),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::htxf::empty_info;
    use crate::text_encoding::DEFAULT;

    fn sample() -> FlatFileInfo {
        FlatFileInfo {
            platform: *b"AMAC",
            file_type: *b"TEXT",
            creator: *b"ttxt",
            platform_flags: 0x0100,
            created: HlDate {
                year: 1998,
                milliseconds: 0,
                seconds: 42,
            },
            modified: HlDate {
                year: 1999,
                milliseconds: 5,
                seconds: 7,
            },
            name: "read me".into(),
            comment: "a note".into(),
        }
    }

    #[test]
    fn encode_and_decode_round_trip() {
        let info = sample();
        let encoded = info.encode(DEFAULT);
        assert_eq!(encoded.len(), FlatFileInfo::FIXED_LEN + 7 + 2 + 6);
        assert_eq!(FlatFileInfo::decode(&encoded, DEFAULT), info);
    }

    #[test]
    fn encode_of_the_default_is_the_empty_info() {
        assert_eq!(FlatFileInfo::default().encode(DEFAULT), empty_info());
    }

    #[test]
    fn encode_cuts_the_name_and_the_comment() {
        let info = FlatFileInfo {
            name: "n".repeat(100),
            comment: "c".repeat(300),
            ..FlatFileInfo::default()
        };
        let decoded = FlatFileInfo::decode(&info.encode(DEFAULT), DEFAULT);
        assert_eq!(decoded.name.len(), FlatFileInfo::MAX_NAME_LEN);
        assert_eq!(decoded.comment.len(), FlatFileInfo::MAX_COMMENT_LEN);
    }

    #[test]
    fn decode_of_a_short_buffer_reads_zeros() {
        let decoded = FlatFileInfo::decode(b"AMACTEXT", DEFAULT);
        assert_eq!(&decoded.file_type, b"TEXT");
        assert_eq!(decoded.creator, [0; 4]);
        assert_eq!(decoded.comment, "");
    }

    #[test]
    fn decode_stops_the_comment_at_the_end_of_the_buffer() {
        let mut encoded = sample().encode(DEFAULT);
        encoded.truncate(encoded.len() - 2);
        assert_eq!(FlatFileInfo::decode(&encoded, DEFAULT).comment, "a no");
    }

    #[test]
    fn decode_needs_the_comment_size_before_the_end() {
        let mut encoded = sample().encode(DEFAULT);
        encoded.truncate(FlatFileInfo::FIXED_LEN + 7);
        assert_eq!(FlatFileInfo::decode(&encoded, DEFAULT).comment, "");
    }

    #[test]
    fn decode_reads_at_most_max_len_bytes() {
        let name_len = FlatFileInfo::MAX_LEN - FlatFileInfo::FIXED_LEN;
        let mut buf = sample().encode(DEFAULT);
        buf.truncate(FlatFileInfo::FIXED_LEN - 2);
        buf.extend_from_slice(&(name_len as u16).to_be_bytes());
        buf.extend(std::iter::repeat_n(b'n', name_len));
        buf.extend_from_slice(b"\0\x05hello");
        let decoded = FlatFileInfo::decode(&buf, DEFAULT);
        assert_eq!(decoded.name.len(), name_len);
        assert_eq!(decoded.comment, "");
    }
}
