//! [`HlDate`]: a Hotline date field.

use crate::error::{ProtoError, Result};
use crate::wire::{PacketReader, PacketWriter};

/// A Hotline date: `year(2) | milliseconds(2) | seconds-since-midnight(4)`.
/// Used by [`myField_FileCreateDate`] (208), [`myField_FileModifyDate`] (209),
/// [`myField_NewsArtDate`] (330), and the
/// old-guide `datetime` object (base year 1904, Mac HFS style).
///
/// [`myField_FileCreateDate`]: crate::FieldId::FileCreateDate
/// [`myField_FileModifyDate`]: crate::FieldId::FileModifyDate
/// [`myField_NewsArtDate`]: crate::FieldId::NewsArtDate
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
