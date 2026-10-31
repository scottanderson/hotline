//! [`PacketReader`]: the read-side counterpart of [`crate::wire::PacketBuilder`].

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
        u8::from_be_bytes(self.take())
    }

    /// Read a big-endian `u16`.
    pub const fn u16(&mut self) -> u16 {
        u16::from_be_bytes(self.take())
    }

    /// Read a big-endian `u32`.
    pub const fn u32(&mut self) -> u32 {
        u32::from_be_bytes(self.take())
    }

    /// Read a big-endian `u64`.
    pub const fn u64(&mut self) -> u64 {
        u64::from_be_bytes(self.take())
    }

    /// Read exactly `L` raw bytes.
    ///
    /// # Panics
    /// Panics if fewer than `L` bytes remain in the buffer.
    #[expect(
        clippy::indexing_slicing,
        reason = "a wire struct reads its own fixed size"
    )]
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
    /// [`crate::wire::PacketWriter::finish`]'s check on the write side.
    pub const fn position(&self) -> usize {
        self.pos
    }

    /// Finish reading.
    ///
    /// # Panics
    /// In debug builds, panics if fewer than `N` bytes were read -
    /// catching a forgotten field at the call site, mirroring
    /// [`crate::wire::PacketWriter::finish`]'s check on the write side.
    pub const fn finish(self) {
        debug_assert!(
            self.pos == N,
            "PacketReader finished after reading fewer than N bytes"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{PacketWriter, VecWriter};

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
    fn fixed_and_growable_share_the_same_builder_methods() {
        let fixed = PacketWriter::<4>::new().u16(1).u16(2).finish().to_vec();
        let grown = VecWriter::new().u16(1).u16(2).finish();
        assert_eq!(fixed, grown);
    }
}
