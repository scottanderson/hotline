//! [`Writer`]: the writer of wire buffers. See the [module
//! docs](crate::wire).

use crate::text_encoding;
use crate::wire::{Fixed, Growable, NoEncoding, PStringTooLong, Storage, WithEncoding};

/// A sequential, consuming-builder-style writer: `w.u32(x).u16(y).finish()`
/// rather than `&mut self` methods, so a chain reads top-to-bottom as the
/// wire layout itself. Generic over [`Storage`], see the [module
/// docs](crate::wire) for [`Fixed`] vs. [`Growable`]. The `E` parameter is a
/// typestate for whether [`Writer::with_encoding`] has run yet, see
/// the [module docs](crate::wire).
#[derive(Debug)]
pub struct Writer<S, E = NoEncoding> {
    storage: S,
    encoding: E,
}

impl<S: Storage, E> Writer<S, E> {
    /// Write a big-endian `u8`.
    pub fn u8(mut self, value: u8) -> Self {
        self.storage.append(&value.to_be_bytes());
        self
    }

    /// Write a big-endian `u16`.
    pub fn u16(mut self, value: u16) -> Self {
        self.storage.append(&value.to_be_bytes());
        self
    }

    /// Write a big-endian `u32`.
    pub fn u32(mut self, value: u32) -> Self {
        self.storage.append(&value.to_be_bytes());
        self
    }

    /// Write a big-endian `u64`.
    pub fn u64(mut self, value: u64) -> Self {
        self.storage.append(&value.to_be_bytes());
        self
    }

    /// Write raw bytes (e.g. a magic number, or another type's own
    /// fixed-size `encode()` output).
    pub fn bytes(mut self, bytes: &[u8]) -> Self {
        self.storage.append(bytes);
        self
    }
}

impl<S: Storage> Writer<S, NoEncoding> {
    /// Attach the text encoding `pstring` writes with for the rest of
    /// this builder's chain.
    pub fn with_encoding(
        self,
        encoding: &'static text_encoding::Encoding,
    ) -> Writer<S, WithEncoding> {
        Writer {
            storage: self.storage,
            encoding: WithEncoding(encoding),
        }
    }
}

impl<S: Storage> Writer<S, WithEncoding> {
    /// Write a Pascal-style string: a 1-byte length prefix followed by
    /// `s` encoded as the encoding [`Writer::with_encoding`]
    /// attached. Fails if the encoded bytes are longer than 255 bytes.
    pub fn pstring(self, s: &str) -> Result<Self, PStringTooLong> {
        let encoding = self.encoding.0;
        let bytes = text_encoding::encode(encoding, s);
        let len = bytes.len();
        let len = u8::try_from(len).map_err(|_| PStringTooLong { len })?;
        Ok(self.u8(len).bytes(&bytes))
    }
}

/// A [`Writer`] that makes a `[u8; N]`. A write past the end panics.
pub type ArrayWriter<const N: usize> = Writer<Fixed<N>>;

/// A [`Writer`] that makes a `Vec<u8>`, for a buffer of variable size.
pub type VecWriter<E = NoEncoding> = Writer<Growable, E>;

impl<const N: usize> ArrayWriter<N> {
    /// Start a new writer over an all-zero `N`-byte buffer.
    pub fn new() -> Self {
        Self {
            storage: Fixed::default(),
            encoding: NoEncoding,
        }
    }
}

impl<const N: usize, E> Writer<Fixed<N>, E> {
    /// Finish and return the completed buffer.
    ///
    /// # Panics
    /// In debug builds, panics if fewer than `N` bytes were written -
    /// catching a forgotten field at the call site instead of silently
    /// shipping a zero-padded buffer.
    pub fn finish(self) -> [u8; N] {
        debug_assert!(
            self.storage.pos == N,
            "ArrayWriter finished after writing fewer than N bytes"
        );
        self.storage.buf
    }
}

impl<const N: usize> Default for ArrayWriter<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl VecWriter {
    /// Start a new, empty writer.
    pub fn new() -> Self {
        Self {
            storage: Growable::default(),
            encoding: NoEncoding,
        }
    }

    /// Start a new writer with room for at least `capacity` bytes
    /// without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            storage: Growable(Vec::with_capacity(capacity)),
            encoding: NoEncoding,
        }
    }
}

impl<E> Writer<Growable, E> {
    /// Finish and return the completed buffer.
    pub fn finish(self) -> Vec<u8> {
        self.storage.0
    }
}

#[cfg(test)]
mod tests {
    use crate::text_encoding;
    use crate::wire::{ArrayWriter, PStringTooLong, VecWriter};

    #[test]
    fn writes_fields_in_order() {
        let bytes = ArrayWriter::<8>::new()
            .u16(0x1234)
            .u16(0x5678)
            .u32(0x9abc_def0)
            .finish();
        assert_eq!(bytes, [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
    }

    #[test]
    #[should_panic]
    fn overrunning_the_buffer_panics_instead_of_corrupting_it() {
        let _ = ArrayWriter::<2>::new().u32(1); // 4 bytes into a 2-byte buffer
    }

    #[test]
    #[should_panic]
    fn finishing_early_panics_in_debug_builds() {
        let _ = ArrayWriter::<4>::new().u16(1).finish(); // only 2 of 4 bytes written
    }

    #[test]
    fn vec_writer_builds_variable_length_buffers() {
        let encoding = text_encoding::DEFAULT;
        let buf = VecWriter::new()
            .bytes(&[127, 0, 0, 1])
            .u16(5500)
            .u16(3)
            .u16(0)
            .with_encoding(encoding)
            .pstring("Test Server")
            .unwrap()
            .pstring("A description")
            .unwrap()
            .finish();

        assert_eq!(&buf[0..4], &[127, 0, 0, 1]);
        assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 5500);
        assert_eq!(buf[10], 11); // "Test Server".len()
        assert_eq!(&buf[11..22], b"Test Server");
    }

    #[test]
    fn pstring_rejects_a_string_over_255_bytes() {
        let encoding = text_encoding::DEFAULT;
        let too_long = "x".repeat(256);
        assert_eq!(
            VecWriter::new()
                .with_encoding(encoding)
                .pstring(&too_long)
                .unwrap_err(),
            PStringTooLong { len: 256 }
        );
    }

    #[test]
    fn pstring_accepts_exactly_255_bytes() {
        let encoding = text_encoding::DEFAULT;
        let exactly_max = "x".repeat(255);
        assert!(VecWriter::new()
            .with_encoding(encoding)
            .pstring(&exactly_max)
            .is_ok());
    }
}
