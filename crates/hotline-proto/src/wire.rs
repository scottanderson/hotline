//! [`PacketBuilder`]: one generic, consuming-builder-style writer for
//! wire buffers, over a pluggable [`Storage`] backing: [`Fixed`] (a
//! stack-allocated `[u8; N]`, no heap allocation, panics on overflow)
//! when the total size is known at compile time, [`Growable`] (a
//! `Vec<u8>`) when it isn't (e.g. a variable number of strings).
//! [`PacketWriter`]/[`VecWriter`] are the two instantiations most call
//! sites actually name.
//!
//! A second, typestate type parameter tracks whether a text encoding has
//! been attached: [`PacketBuilder::pstring`] only exists once
//! [`PacketBuilder::with_encoding`] has run, so a chain that writes a
//! `pstring` without ever choosing an encoding is a compile error rather
//! than a silent default.
//!
//! [`PacketReader`] is the read-side counterpart, always fixed-size,
//! since you always know how many bytes you're about to read.

use crate::text_encoding;

/// Where a [`PacketBuilder`] writes its bytes.
pub trait Storage: Default {
    /// Append `bytes` to the end of what's been written so far.
    ///
    /// # Panics
    /// Implementations that can't grow (e.g. [`Fixed`]) panic if this
    /// would write past their capacity.
    fn append(&mut self, bytes: &[u8]);
}

/// Fixed-size backing: a stack-allocated `[u8; N]`, no heap allocation.
#[derive(Debug)]
pub struct Fixed<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> Default for Fixed<N> {
    fn default() -> Self {
        Self {
            buf: [0; N],
            pos: 0,
        }
    }
}

impl<const N: usize> Storage for Fixed<N> {
    fn append(&mut self, bytes: &[u8]) {
        let len = bytes.len();
        self.buf[self.pos..self.pos + len].copy_from_slice(bytes);
        self.pos += len;
    }
}

/// Growable backing: a `Vec<u8>`, for wire structures whose total size
/// isn't known until a variable number of pieces have been written.
#[derive(Debug, Default)]
pub struct Growable(Vec<u8>);

impl Storage for Growable {
    fn append(&mut self, bytes: &[u8]) {
        self.0.extend_from_slice(bytes);
    }
}

/// [`PacketBuilder`]'s `E` state before [`PacketBuilder::with_encoding`]
/// has run: `pstring` isn't available yet.
#[derive(Debug, Default)]
pub struct NoEncoding;

/// [`PacketBuilder`]'s `E` state once [`PacketBuilder::with_encoding`] has
/// run: unlocks `pstring`.
#[derive(Debug, Clone, Copy)]
pub struct WithEncoding(&'static text_encoding::Encoding);

/// A sequential, consuming-builder-style writer: `w.u32(x).u16(y).finish()`
/// rather than `&mut self` methods, so a chain reads top-to-bottom as the
/// wire layout itself. Generic over [`Storage`], see the [module
/// docs](self) for [`Fixed`] vs. [`Growable`]. The `E` parameter is a
/// typestate for whether [`PacketBuilder::with_encoding`] has run yet, see
/// the [module docs](self).
#[derive(Debug)]
pub struct PacketBuilder<S, E = NoEncoding> {
    storage: S,
    encoding: E,
}

impl<S: Storage, E> PacketBuilder<S, E> {
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

impl<S: Storage> PacketBuilder<S, NoEncoding> {
    /// Attach the text encoding `pstring` writes with for the rest of
    /// this builder's chain.
    pub fn with_encoding(
        self,
        encoding: &'static text_encoding::Encoding,
    ) -> PacketBuilder<S, WithEncoding> {
        PacketBuilder {
            storage: self.storage,
            encoding: WithEncoding(encoding),
        }
    }
}

impl<S: Storage> PacketBuilder<S, WithEncoding> {
    /// Write a Pascal-style string: a 1-byte length prefix followed by
    /// `s` encoded as the encoding [`PacketBuilder::with_encoding`]
    /// attached. Fails if the encoded bytes are longer than 255 bytes.
    pub fn pstring(self, s: &str) -> Result<Self, PStringTooLong> {
        let encoding = self.encoding.0;
        let bytes = text_encoding::encode(encoding, s);
        let len = bytes.len();
        let len = u8::try_from(len).map_err(|_| PStringTooLong { len })?;
        Ok(self.u8(len).bytes(&bytes))
    }
}

/// A sequential, builder-style writer that produces a fixed `[u8; N]`,
/// no heap allocation, panics if a write would run past the end.
pub type PacketWriter<const N: usize> = PacketBuilder<Fixed<N>>;

/// A sequential, builder-style writer that produces a `Vec<u8>`, for
/// wire structures whose total size isn't known until you've written a
/// variable number of pieces into it.
pub type VecWriter<E = NoEncoding> = PacketBuilder<Growable, E>;

impl<const N: usize> PacketWriter<N> {
    /// Start a new writer over an all-zero `N`-byte buffer.
    pub fn new() -> Self {
        Self {
            storage: Fixed::default(),
            encoding: NoEncoding,
        }
    }
}

impl<const N: usize, E> PacketBuilder<Fixed<N>, E> {
    /// Finish and return the completed buffer.
    ///
    /// # Panics
    /// In debug builds, panics if fewer than `N` bytes were written -
    /// catching a forgotten field at the call site instead of silently
    /// shipping a zero-padded buffer.
    pub fn finish(self) -> [u8; N] {
        debug_assert!(
            self.storage.pos == N,
            "PacketWriter finished after writing fewer than N bytes"
        );
        self.storage.buf
    }
}

impl<const N: usize> Default for PacketWriter<N> {
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

impl<E> PacketBuilder<Growable, E> {
    /// Finish and return the completed buffer.
    pub fn finish(self) -> Vec<u8> {
        self.storage.0
    }
}

/// A sequential, builder-style reader over an `N`-byte buffer you already
/// have (e.g. bytes just read off a socket). Always fixed-size: unlike
/// writing, reading a wire structure always starts from a byte count you
/// already know.
pub struct PacketReader<const N: usize> {
    buf: [u8; N],
    pos: usize,
}

impl<const N: usize> PacketReader<N> {
    /// Start reading from the front of `buf`.
    pub const fn new(buf: [u8; N]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Read a big-endian `u8`.
    pub const fn u8(&mut self) -> u8 {
        u8::from_be_bytes(self.take::<1>())
    }

    /// Read a big-endian `u16`.
    pub const fn u16(&mut self) -> u16 {
        u16::from_be_bytes(self.take::<2>())
    }

    /// Read a big-endian `u32`.
    pub const fn u32(&mut self) -> u32 {
        u32::from_be_bytes(self.take::<4>())
    }

    /// Read a big-endian `u64`.
    pub const fn u64(&mut self) -> u64 {
        u64::from_be_bytes(self.take::<8>())
    }

    /// Read exactly `L` raw bytes.
    ///
    /// # Panics
    /// Panics if fewer than `L` bytes remain in the buffer.
    pub const fn take<const L: usize>(&mut self) -> [u8; L] {
        let mut out = [0u8; L];
        let mut i = 0;
        while i < L {
            out[i] = self.buf[self.pos + i];
            i += 1;
        }
        self.pos += L;
        out
    }

    /// Bytes consumed so far. Mainly useful for `debug_assert`s mirroring
    /// [`PacketWriter::finish`]'s check on the write side.
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Finish reading.
    ///
    /// # Panics
    /// In debug builds, panics if fewer than `N` bytes were read -
    /// catching a forgotten field at the call site, mirroring
    /// [`PacketWriter::finish`]'s check on the write side.
    pub const fn finish(self) {
        debug_assert!(
            self.pos == N,
            "PacketReader finished after reading fewer than N bytes"
        );
    }
}

/// A string was too long to fit a 1-byte Pascal-string length prefix
/// (max 255 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("string is {len} bytes, too long for a 1-byte length prefix (max 255)")]
pub struct PStringTooLong {
    /// The string's actual length in bytes.
    pub len: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_fields_in_order() {
        let bytes = PacketWriter::<8>::new()
            .u16(0x1234)
            .u16(0x5678)
            .u32(0x9abc_def0)
            .finish();
        assert_eq!(bytes, [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0]);
    }

    #[test]
    #[should_panic]
    fn overrunning_the_buffer_panics_instead_of_corrupting_it() {
        let _ = PacketWriter::<2>::new().u32(1); // 4 bytes into a 2-byte buffer
    }

    #[test]
    #[should_panic]
    fn finishing_early_panics_in_debug_builds() {
        let _ = PacketWriter::<4>::new().u16(1).finish(); // only 2 of 4 bytes written
    }

    #[test]
    fn reads_back_what_was_written() {
        let bytes = PacketWriter::<8>::new()
            .u16(0x1234)
            .u16(0x5678)
            .u32(0x9abc_def0)
            .finish();

        let mut r = PacketReader::<8>::new(bytes);
        let a = r.u16();
        let b = r.u16();
        let c = r.u32();
        assert_eq!((a, b, c), (0x1234, 0x5678, 0x9abc_def0));
        assert_eq!(r.position(), 8);
        r.finish();
    }

    #[test]
    #[should_panic]
    fn finishing_early_panics_in_debug_builds_on_read_too() {
        let r = PacketReader::<4>::new([0, 0, 0, 0]);
        r.finish();
    }

    #[test]
    #[should_panic]
    fn reading_past_the_end_panics() {
        let mut r = PacketReader::<2>::new([0, 0]);
        r.u16();
        r.u16(); // nothing left
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

    #[test]
    fn fixed_and_growable_share_the_same_builder_methods() {
        let fixed = PacketWriter::<4>::new().u16(1).u16(2).finish().to_vec();
        let grown = VecWriter::new().u16(1).u16(2).finish();
        assert_eq!(fixed, grown);
    }
}
