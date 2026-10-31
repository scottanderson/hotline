//! Big-endian unsigned integer helpers for field payloads.
//!
//! Every wire integer field wider than 16 bits is unsigned in mhxd
//! (`u_int32_t`; there's no genuinely signed field anywhere in it), so
//! [`encode`]/[`decode`] are unsigned and are what
//! [`crate::field::Field::u32`] uses.
//!
//! [`encode`] picks whichever of the 2-byte or 4-byte width is
//! smaller. [`decode`] takes a 1, 2, or 4 byte body for any value.
//! `ACCESS` is the one 64-bit field. Read and write it with
//! [`crate::field::Field::as_u64`] and [`crate::field::Field::u64`].

use std::ops::Deref;

/// A 2-or-4-byte big-endian encoded integer, held inline in a fixed
/// `[u8; 4]` buffer rather than a `Vec<u8>`, avoiding a heap allocation
/// on the hot path of encoding a numeric field. Derefs to `&[u8]` for
/// `Vec<u8>`-like ergonomics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedInt {
    bytes: [u8; 4],
    len: u8,
}

impl Deref for EncodedInt {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.bytes
            .get(..usize::from(self.len))
            .unwrap_or(&self.bytes)
    }
}

impl AsRef<[u8]> for EncodedInt {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Encode `value` as the smaller of `u16` (2 bytes) or `u32` (4 bytes),
/// with no heap allocation; see [`EncodedInt`].
pub const fn encode(value: u32) -> EncodedInt {
    if value <= u16::MAX as u32 {
        let [a, b] = (value as u16).to_be_bytes();
        EncodedInt {
            bytes: [a, b, 0, 0],
            len: 2,
        }
    } else {
        EncodedInt {
            bytes: value.to_be_bytes(),
            len: 4,
        }
    }
}

/// Decode a 1, 2, or 4 byte unsigned integer field body. `None` for
/// any other length.
pub const fn decode(bytes: &[u8]) -> Option<u32> {
    match *bytes {
        [a] => Some(a as u32),
        [a, b] => Some(u16::from_be_bytes([a, b]) as u32),
        [a, b, c, d] => Some(u32::from_be_bytes([a, b, c, d])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_smaller_width() {
        assert_eq!(encode(0).len(), 2);
        assert_eq!(encode(u16::MAX as u32).len(), 2);
        assert_eq!(encode(u16::MAX as u32 + 1).len(), 4);
    }

    #[test]
    fn round_trips_both_widths() {
        for v in [0u32, 1, u16::MAX as u32, u16::MAX as u32 + 1, u32::MAX] {
            assert_eq!(decode(&encode(v)), Some(v));
        }
    }

    #[test]
    fn decodes_one_byte() {
        assert_eq!(decode(&[7]), Some(7));
    }

    #[test]
    fn rejects_odd_widths() {
        assert_eq!(decode(&[]), None);
        assert_eq!(decode(&[0, 0, 0]), None);
    }

    #[test]
    fn encoded_int_holds_no_heap_allocation() {
        // Purely a size/type-level check: EncodedInt must be Copy (no Vec,
        // no Box, nothing heap-owning inside it), and small.
        fn assert_copy<T: Copy>() {}
        assert_copy::<EncodedInt>();
        assert!(std::mem::size_of::<EncodedInt>() <= 8);
    }
}
