//! [`SliceReader`]: the read side of [`crate::wire::VecWriter`].

use crate::error::{ProtoError, Result};
use crate::text_encoding;
use crate::wire::{NoEncoding, WithEncoding};

/// A cursor over a borrowed buffer of unknown length. Each read checks
/// that its bytes are there and fails with [`ProtoError::UnexpectedEof`]
/// if not. [`SliceReader::pstring`] exists only after
/// [`SliceReader::with_encoding`].
#[derive(Debug)]
pub struct SliceReader<'a, E = NoEncoding> {
    buf: &'a [u8],
    pos: usize,
    encoding: E,
}

impl<'a> SliceReader<'a> {
    /// Start reading from the front of `buf`.
    pub const fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            encoding: NoEncoding,
        }
    }

    /// Attach the text encoding that [`SliceReader::pstring`] decodes with.
    pub const fn with_encoding(
        self,
        encoding: &'static text_encoding::Encoding,
    ) -> SliceReader<'a, WithEncoding> {
        SliceReader {
            buf: self.buf,
            pos: self.pos,
            encoding: WithEncoding(encoding),
        }
    }
}

impl<'a, E> SliceReader<'a, E> {
    /// Read the next `len` bytes.
    pub fn bytes(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.saturating_add(len);
        let bytes = self
            .buf
            .get(self.pos..end)
            .ok_or(ProtoError::UnexpectedEof {
                needed: end,
                available: self.buf.len(),
            })?;
        self.pos = end;
        Ok(bytes)
    }

    /// Read exactly `N` bytes.
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        Ok(self.bytes(N)?.try_into().expect("bytes returns N bytes"))
    }

    /// Read one byte.
    pub fn u8(&mut self) -> Result<u8> {
        self.array().map(u8::from_be_bytes)
    }

    /// Read a big-endian `u16`.
    pub fn u16(&mut self) -> Result<u16> {
        self.array().map(u16::from_be_bytes)
    }

    /// Read a big-endian `u32`.
    pub fn u32(&mut self) -> Result<u32> {
        self.array().map(u32::from_be_bytes)
    }

    /// The bytes not read yet.
    pub fn rest(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

impl SliceReader<'_, WithEncoding> {
    /// Read a Pascal-style string: a 1-byte length, then that many bytes in
    /// the attached encoding.
    pub fn pstring(&mut self) -> Result<String> {
        let len = self.u8()?;
        let bytes = self.bytes(usize::from(len))?;
        Ok(text_encoding::decode(self.encoding.0, bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::VecWriter;

    #[test]
    fn reads_back_what_a_vec_writer_wrote() {
        let encoding = text_encoding::DEFAULT;
        let buf = VecWriter::new()
            .u8(1)
            .u16(0x1234)
            .u32(0x9abc_def0)
            .with_encoding(encoding)
            .pstring("caf\u{e9}")
            .unwrap()
            .bytes(b"xy")
            .finish();

        let mut r = SliceReader::new(&buf).with_encoding(encoding);
        assert_eq!(r.u8().unwrap(), 1);
        assert_eq!(r.u16().unwrap(), 0x1234);
        assert_eq!(r.u32().unwrap(), 0x9abc_def0);
        assert_eq!(r.pstring().unwrap(), "caf\u{e9}");
        assert_eq!(r.array::<2>().unwrap(), *b"xy");
        assert!(r.rest().is_empty());
    }

    #[test]
    fn a_read_past_the_end_is_an_error() {
        let mut r = SliceReader::new(&[0, 1, 2]);
        assert!(matches!(
            r.u32(),
            Err(ProtoError::UnexpectedEof {
                needed: 4,
                available: 3
            })
        ));
    }

    #[test]
    fn a_read_of_usize_max_bytes_is_an_error() {
        let mut r = SliceReader::new(&[0, 1, 2]);
        r.u8().unwrap();
        assert!(matches!(
            r.bytes(usize::MAX),
            Err(ProtoError::UnexpectedEof { .. })
        ));
    }

    #[test]
    fn a_pstring_longer_than_the_buffer_is_an_error() {
        let mut r = SliceReader::new(&[5, b'a']).with_encoding(text_encoding::DEFAULT);
        assert!(r.pstring().is_err());
    }
}
