//! Signed big-endian integer helpers for field payloads.
//!
//! [`encode`] picks whichever of `i16` (2 bytes) or `i32` (4 bytes) is
//! smaller. `i32` is the ceiling: the one genuinely 64-bit field
//! (`ACCESS`, a bitmask rather than a magnitude) is handled separately
//! via [`crate::field::Field::u64`] / [`crate::field::Field::as_u64`].
//!
//! [`decode`] stays tolerant of either width regardless of the value's
//! magnitude, since a sender is free to use the wider encoding even for
//! a small value.

use std::ops::Deref;

/// A 2-or-4-byte big-endian encoded integer, held inline in a fixed
/// `[u8; 4]` buffer rather than a `Vec<u8>` - avoids a heap allocation
/// on the hot path of encoding a numeric field. Derefs to `&[u8]` for
/// `Vec<u8>`-like ergonomics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncodedInt {
    bytes: [u8; 4],
    len: u8,
}

impl Deref for EncodedInt {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        &self.bytes[..self.len as usize]
    }
}

impl AsRef<[u8]> for EncodedInt {
    fn as_ref(&self) -> &[u8] {
        self
    }
}

/// Encode `value` as the smaller of `i16` (2 bytes) or `i32` (4 bytes),
/// with no heap allocation; see [`EncodedInt`].
pub const fn encode(value: i32) -> EncodedInt {
    // `i16::try_from` isn't const-callable on stable Rust yet, so the
    // "does this fit in 16 bits" check is spelled out as a plain range
    // comparison instead.
    if value >= i16::MIN as i32 && value <= i16::MAX as i32 {
        let [a, b] = (value as i16).to_be_bytes();
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

/// Decode a 2-byte or 4-byte integer field body. `None` for any other
/// length rather than guessing.
pub const fn decode(bytes: &[u8]) -> Option<i32> {
    match bytes.len() {
        2 => Some(i16::from_be_bytes([bytes[0], bytes[1]]) as i32),
        4 => Some(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_smaller_width() {
        assert_eq!(encode(0).len(), 2);
        assert_eq!(encode(i16::MAX as i32).len(), 2);
        assert_eq!(encode(i16::MAX as i32 + 1).len(), 4);
        assert_eq!(encode(i16::MIN as i32).len(), 2);
        assert_eq!(encode(i16::MIN as i32 - 1).len(), 4);
    }

    #[test]
    fn round_trips_both_widths() {
        for v in [
            0i32,
            1,
            -1,
            i16::MAX as i32,
            i16::MIN as i32,
            i32::MAX,
            i32::MIN,
        ] {
            assert_eq!(decode(&encode(v)), Some(v));
        }
    }

    #[test]
    fn rejects_odd_widths() {
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
