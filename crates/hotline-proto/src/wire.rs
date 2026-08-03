//! [`PacketBuilder`]: one generic, consuming-builder-style writer for
//! wire buffers, over a pluggable [`Storage`] backing: [`Fixed`] (a
//! stack-allocated `[u8; N]`, no heap allocation, panics on overflow)
//! when the total size is known at compile time, [`Growable`] (a
//! `Vec<u8>`) when it isn't (e.g. a variable number of strings).
//! [`PacketWriter`]/[`VecWriter`] are the two instantiations most call
//! sites actually name.
//!
//! [`PacketReader`] is the read-side counterpart, always fixed-size,
//! since you always know how many bytes you're about to read.

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
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();
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

/// A sequential, consuming-builder-style writer: `w.u32(x).u16(y).finish()`
/// rather than `&mut self` methods, so a chain reads top-to-bottom as the
/// wire layout itself. Generic over [`Storage`], see the [module
/// docs](self) for [`Fixed`] vs. [`Growable`].
#[derive(Debug)]
pub struct PacketBuilder<S> {
    storage: S,
}

impl<S: Storage> PacketBuilder<S> {
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

    /// Write a Pascal-style string: a 1-byte length prefix followed by
    /// `s`'s UTF-8 bytes. Fails if `s` is longer than 255 bytes.
    pub fn pstring(mut self, s: &str) -> Result<Self, PStringTooLong> {
        let len = u8::try_from(s.len()).map_err(|_| PStringTooLong { len: s.len() })?;
        self.storage.append(&[len]);
        self.storage.append(s.as_bytes());
        Ok(self)
    }
}

/// A sequential, builder-style writer that produces a fixed `[u8; N]`,
/// no heap allocation, panics if a write would run past the end.
pub type PacketWriter<const N: usize> = PacketBuilder<Fixed<N>>;

/// A sequential, builder-style writer that produces a `Vec<u8>`, for
/// wire structures whose total size isn't known until you've written a
/// variable number of pieces into it.
pub type VecWriter = PacketBuilder<Growable>;

impl<const N: usize> PacketWriter<N> {
    /// Start a new writer over an all-zero `N`-byte buffer.
    pub fn new() -> Self {
        Self {
            storage: Fixed::default(),
        }
    }

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
        }
    }

    /// Start a new writer with room for at least `capacity` bytes
    /// without reallocating.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            storage: Growable(Vec::with_capacity(capacity)),
        }
    }

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
    pub const fn u8(mut self) -> (Self, u8) {
        let (next, bytes) = self.take::<1>();
        self = next;
        (self, u8::from_be_bytes(bytes))
    }

    /// Read a big-endian `u16`.
    pub const fn u16(mut self) -> (Self, u16) {
        let (next, bytes) = self.take::<2>();
        self = next;
        (self, u16::from_be_bytes(bytes))
    }

    /// Read a big-endian `u32`.
    pub const fn u32(mut self) -> (Self, u32) {
        let (next, bytes) = self.take::<4>();
        self = next;
        (self, u32::from_be_bytes(bytes))
    }

    /// Read a big-endian `u64`.
    pub const fn u64(mut self) -> (Self, u64) {
        let (next, bytes) = self.take::<8>();
        self = next;
        (self, u64::from_be_bytes(bytes))
    }

    /// Read exactly `L` raw bytes.
    ///
    /// # Panics
    /// Panics if fewer than `L` bytes remain in the buffer.
    pub const fn take<const L: usize>(mut self) -> (Self, [u8; L]) {
        let mut out = [0u8; L];
        let mut i = 0;
        while i < L {
            out[i] = self.buf[self.pos + i];
            i += 1;
        }
        self.pos += L;
        (self, out)
    }

    /// Bytes consumed so far. Mainly useful for `debug_assert`s mirroring
    /// [`PacketWriter::finish`]'s check on the write side.
    pub const fn position(&self) -> usize {
        self.pos
    }
}

/// A type that always encodes to the same fixed number of wire bytes.
/// `N` is a type parameter, not an associated const, since using one as
/// an array length in a trait method's return type needs the unstable
/// `generic_const_exprs` feature.
pub trait FixedEncode<const N: usize> {
    /// Encode to exactly `N` bytes.
    fn encode(&self) -> [u8; N];
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

        let r = PacketReader::<8>::new(bytes);
        let (r, a) = r.u16();
        let (r, b) = r.u16();
        let (r, c) = r.u32();
        assert_eq!((a, b, c), (0x1234, 0x5678, 0x9abc_def0));
        assert_eq!(r.position(), 8);
    }

    #[test]
    #[should_panic]
    fn reading_past_the_end_panics() {
        let (r, _) = PacketReader::<2>::new([0, 0]).u16();
        let _ = r.u16(); // nothing left
    }

    #[test]
    fn vec_writer_builds_variable_length_buffers() {
        let buf = VecWriter::new()
            .bytes(&[127, 0, 0, 1])
            .u16(5500)
            .u16(3)
            .u16(0)
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
        let too_long = "x".repeat(256);
        assert_eq!(
            VecWriter::new().pstring(&too_long).unwrap_err(),
            PStringTooLong { len: 256 }
        );
    }

    #[test]
    fn pstring_accepts_exactly_255_bytes() {
        let exactly_max = "x".repeat(255);
        assert!(VecWriter::new().pstring(&exactly_max).is_ok());
    }

    #[test]
    fn fixed_and_growable_share_the_same_builder_methods() {
        let fixed = PacketWriter::<4>::new().u16(1).u16(2).finish().to_vec();
        let grown = VecWriter::new().u16(1).u16(2).finish();
        assert_eq!(fixed, grown);
    }
}
