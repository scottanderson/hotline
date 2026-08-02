//! `Field`: the `(id, size, data)` records that make up a transaction's
//! parameter list.
//!
//! Field *tags* live in [`crate::field_id`], split into
//! [`crate::field_id::ClientFieldId`]/[`crate::field_id::ServerFieldId`];
//! see that module's docs for why. `Field` itself still stores a raw
//! `u16` id: once bytes come off the wire, direction is a property of
//! the *transaction* they arrived in, not of an individual field, so
//! `Field` can't know which of the two naming tables applies to itself
//! in isolation.
//!
//! The payload is [`FieldValue`], not a bare `Vec<u8>`: every
//! constructor takes (or builds) a specific variant, so there's no way
//! to end up with, say, a 3-byte buffer masquerading as a `U16`, and
//! `Debug` shows the actual typed value instead of guessing from length
//! and ASCII-ness at print time. A field just decoded off the wire is
//! [`FieldValue::Bytes`] until something interprets it (`as_string()`,
//! `as_u16()`, ...). There's no schema table telling decode what a raw
//! id "should" mean, so it stays honestly untyped until a caller who
//! knows the field says otherwise.

use crate::error::{ProtoError, Result};
use crate::numbers::{decode as decode_number, encode};
use crate::wire::VecWriter;

/// A field's payload. Locally-built fields carry their real type end to
/// end; decoded fields start as [`FieldValue::Bytes`] until a typed
/// accessor interprets them (see the [module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// UTF-8 text (see [`Field::as_string`] for the encoding rationale).
    Text(String),
    /// XOR-0xFF obfuscated text, used for `LOGIN`/`PASSWORD` on the wire.
    ObfuscatedText(String),
    /// A 16-bit field (`UID`, `ICON`, length prefixes, ...).
    U16(u16),
    /// A 32-bit field (file/transfer sizes, chat/thread IDs, ...).
    U32(u32),
    /// The 64-bit `USER_ACCESS` bitmask, the only genuinely 64-bit
    /// field on the wire.
    U64(u64),
    /// A generic signed integer, encoded as the smaller of 2 or 4 bytes
    /// (see [`crate::numbers`]).
    Integer(i32),
    /// Anything else: a packed record, an image, or, for a field just
    /// decoded off the wire, uninterpreted bytes.
    Bytes(Vec<u8>),
}

impl FieldValue {
    /// Encode to wire bytes.
    fn encode(&self) -> Vec<u8> {
        match self {
            Self::Text(s) => s.as_bytes().to_vec(),
            Self::ObfuscatedText(s) => xor_ff(s.as_bytes()),
            Self::U16(v) => v.to_be_bytes().to_vec(),
            Self::U32(v) => v.to_be_bytes().to_vec(),
            Self::U64(v) => v.to_be_bytes().to_vec(),
            Self::Integer(v) => encode(*v).to_vec(),
            Self::Bytes(b) => b.clone(),
        }
    }

    /// Byte length this value encodes to, without allocating.
    fn encoded_len(&self) -> usize {
        match self {
            Self::Text(s) => s.len(),
            Self::ObfuscatedText(s) => s.len(),
            Self::U16(_) => 2,
            Self::U32(_) => 4,
            Self::U64(_) => 8,
            Self::Integer(v) => encode(*v).len(),
            Self::Bytes(b) => b.len(),
        }
    }
}

/// A single transaction parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field tag. Pass a [`crate::field_id::ClientFieldId`] or
    /// [`crate::field_id::ServerFieldId`] constant to any constructor
    /// here (both convert via `Into<u16>`); stored as a plain `u16`
    /// since that's what's actually on the wire and what decoding
    /// produces before direction is known.
    pub id: u16,
    /// The typed payload. See [`FieldValue`].
    pub value: FieldValue,
}

impl Field {
    /// Build a field from an already-typed value.
    pub fn new(id: impl Into<u16>, value: FieldValue) -> Self {
        Self {
            id: id.into(),
            value,
        }
    }

    /// Build a field from raw bytes, e.g. a packed record
    /// (`FILE_NAME_WITH_INFO`, `USER_NAME_WITH_INFO`) that has its own
    /// internal encoding.
    pub fn bytes(id: impl Into<u16>, data: Vec<u8>) -> Self {
        Self::new(id, FieldValue::Bytes(data))
    }

    /// Build a 16-bit integer field (`UID`, `ICON`, `CHAT_SUBJECT` length
    /// prefixes, etc). Used where the exact wire width is confirmed fixed.
    pub fn u16(id: impl Into<u16>, value: u16) -> Self {
        Self::new(id, FieldValue::U16(value))
    }

    /// Build a 32-bit integer field (file/transfer sizes, chat/thread
    /// IDs). Used where the exact wire width is confirmed fixed.
    pub fn u32(id: impl Into<u16>, value: u32) -> Self {
        Self::new(id, FieldValue::U32(value))
    }

    /// Build the 64-bit `USER_ACCESS` bitmask field. Not a "number" in
    /// the [`Field::integer`] sense (it's a bitmap, not a magnitude), so
    /// it doesn't go through [`crate::numbers`].
    pub fn u64(id: impl Into<u16>, value: u64) -> Self {
        Self::new(id, FieldValue::U64(value))
    }

    /// Build a generic signed integer field, letting
    /// [`crate::numbers::encode`] pick the smaller of a 2-byte or 4-byte
    /// two's-complement representation. Use this for fields whose exact
    /// wire width isn't independently confirmed; use
    /// [`Field::u16`]/[`Field::u32`] instead when it is.
    pub fn integer(id: impl Into<u16>, value: i32) -> Self {
        Self::new(id, FieldValue::Integer(value))
    }

    /// Build a string field, encoded as UTF-8 (see [`Field::as_string`]
    /// for why that's a reasonable choice here).
    pub fn string(id: impl Into<u16>, value: impl Into<String>) -> Self {
        Self::new(id, FieldValue::Text(value.into()))
    }

    /// Build a `LOGIN`/`PASSWORD`-style field: text obfuscated with
    /// Hotline's symmetric XOR-0xFF transform (`hl_code` in the
    /// reference C implementation).
    pub fn obfuscated_string(id: impl Into<u16>, value: impl Into<String>) -> Self {
        Self::new(id, FieldValue::ObfuscatedText(value.into()))
    }

    /// Interpret this field as deobfuscated text, regardless of how it
    /// was actually constructed/decoded (an obfuscated field decoded off
    /// the wire is [`FieldValue::Bytes`], not
    /// [`FieldValue::ObfuscatedText`], since decode doesn't know a
    /// field's semantics, see the [module docs](self)).
    pub fn as_deobfuscated_string(&self) -> String {
        match &self.value {
            FieldValue::ObfuscatedText(s) => s.clone(),
            FieldValue::Bytes(b) => String::from_utf8_lossy(&xor_ff(b)).into_owned(),
            other => String::from_utf8_lossy(&xor_ff(&other.encode())).into_owned(),
        }
    }

    /// Interpret this field's data as an integer. Accepts a 2 or 4 byte
    /// payload (see [`crate::numbers`] for why encoding picks the
    /// smaller width but decoding stays tolerant of either).
    pub fn as_integer(&self) -> Option<i32> {
        match &self.value {
            FieldValue::Integer(v) => Some(*v),
            FieldValue::U16(v) => Some(*v as i32),
            FieldValue::U32(v) => decode_number(&v.to_be_bytes()).or(Some(*v as i32)),
            FieldValue::Bytes(b) => decode_number(b),
            _ => None,
        }
    }

    /// Interpret this field's data as `u16`. `None` unless the payload
    /// is exactly 2 bytes.
    pub fn as_u16(&self) -> Option<u16> {
        match &self.value {
            FieldValue::U16(v) => Some(*v),
            FieldValue::Bytes(b) => {
                let bytes: [u8; 2] = b.clone().try_into().ok()?;
                Some(u16::from_be_bytes(bytes))
            }
            _ => None,
        }
    }

    /// Interpret this field's data as `u32`. `None` unless the payload
    /// is exactly 4 bytes.
    pub fn as_u32(&self) -> Option<u32> {
        match &self.value {
            FieldValue::U32(v) => Some(*v),
            FieldValue::Bytes(b) => {
                let bytes: [u8; 4] = b.clone().try_into().ok()?;
                Some(u32::from_be_bytes(bytes))
            }
            _ => None,
        }
    }

    /// Interpret this field's data as the 64-bit `USER_ACCESS` bitmask.
    /// `None` unless the payload is exactly 8 bytes.
    pub fn as_u64(&self) -> Option<u64> {
        match &self.value {
            FieldValue::U64(v) => Some(*v),
            FieldValue::Bytes(b) => {
                let bytes: [u8; 8] = b.clone().try_into().ok()?;
                Some(u64::from_be_bytes(bytes))
            }
            _ => None,
        }
    }

    /// Interpret this field's data as text, decoded as UTF-8 (lossily,
    /// substituting U+FFFD for any invalid sequence). The protocol has
    /// no mandated text encoding: field bytes are opaque on the wire,
    /// and servers may convert between encodings. UTF-8 works when both
    /// ends speak it; bytes from a legacy client using another encoding
    /// (historically Mac OS Roman) won't round-trip correctly here.
    pub fn as_string(&self) -> String {
        match &self.value {
            FieldValue::Text(s) | FieldValue::ObfuscatedText(s) => s.clone(),
            FieldValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
            other => String::from_utf8_lossy(&other.encode()).into_owned(),
        }
    }

    /// Raw bytes, however this field is actually represented.
    pub fn as_bytes(&self) -> Vec<u8> {
        match &self.value {
            FieldValue::Bytes(b) => b.clone(),
            other => other.encode(),
        }
    }

    /// `field_id (2) | field_size (2) | data`.
    ///
    /// Fails if the encoded value is longer than 65535 bytes, the
    /// length prefix is a `u16`. This is reachable with
    /// attacker-controlled input (see [`ProtoError::TooLong`]), not just
    /// a theoretical limit: don't silently truncate it.
    pub fn encode(&self) -> crate::error::Result<Vec<u8>> {
        let data_len = self.value.encoded_len();
        let len = u16::try_from(data_len).map_err(|_| ProtoError::TooLong {
            what: "field data",
            len: data_len,
        })?;
        Ok(VecWriter::with_capacity(4 + data_len)
            .u16(self.id)
            .u16(len)
            .bytes(&self.value.encode())
            .finish())
    }

    /// Decode one field from the front of `buf`, returning the field and
    /// the number of bytes consumed. Always produces
    /// [`FieldValue::Bytes`]. See the [module docs](self) for why
    /// decode can't know a field's semantic type.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        if buf.len() < 4 {
            return Err(ProtoError::UnexpectedEof {
                needed: 4,
                available: buf.len(),
            });
        }
        let id = u16::from_be_bytes([buf[0], buf[1]]);
        let size = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        let total = 4 + size;
        if buf.len() < total {
            return Err(ProtoError::UnexpectedEof {
                needed: total,
                available: buf.len(),
            });
        }
        Ok((
            Field {
                id,
                value: FieldValue::Bytes(buf[4..total].to_vec()),
            },
            total,
        ))
    }
}

/// Hotline's `hl_code`: XOR every byte with `0xFF`. Symmetric, so the
/// same function encodes and decodes. A free function rather than a
/// `Field`/`FieldValue` method since it's a pure byte-transform several
/// call sites here happen to need, not something tied to either type.
fn xor_ff(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|&b| !b).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field_id::{ClientFieldId, ServerFieldId};

    #[test]
    fn field_round_trips() {
        let f = Field::string(ClientFieldId::Name, "RustHL");
        let encoded = f.encode().unwrap();
        let (decoded, consumed) = Field::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.as_string(), "RustHL");
    }

    #[test]
    fn multi_byte_utf8_round_trips() {
        let f = Field::string(ClientFieldId::Text, "h\u{e9}llo \u{1f389}");
        let (decoded, _) = Field::decode(&f.encode().unwrap()).unwrap();
        assert_eq!(decoded.as_string(), "h\u{e9}llo \u{1f389}");
    }

    #[test]
    fn encode_rejects_data_over_65535_bytes_instead_of_truncating() {
        let f = Field::bytes(ClientFieldId::Text, vec![0u8; 65536]);
        assert!(matches!(
            f.encode(),
            Err(ProtoError::TooLong {
                what: "field data",
                len: 65536
            })
        ));
    }

    #[test]
    fn u16_field_round_trips() {
        let f = Field::u16(ClientFieldId::Uid, 42);
        assert_eq!(f.value, FieldValue::U16(42));
        assert_eq!(f.as_integer(), Some(42));
    }

    #[test]
    fn u64_field_round_trips() {
        let f = Field::u64(ClientFieldId::Access, u64::MAX);
        assert_eq!(f.as_u64(), Some(u64::MAX));
    }

    #[test]
    fn integer_field_picks_smallest_signed_width() {
        let small = Field::integer(ClientFieldId::Version, 151);
        assert_eq!(small.encode().unwrap().len(), 4 + 2);
        assert_eq!(small.as_integer(), Some(151));

        let large = Field::integer(ServerFieldId::FileSize, i32::MAX);
        assert_eq!(large.encode().unwrap().len(), 4 + 4);
        assert_eq!(large.as_integer(), Some(i32::MAX));
    }

    #[test]
    fn obfuscated_string_round_trips() {
        let f = Field::obfuscated_string(ClientFieldId::Login, "guest");
        // On the wire it must NOT be the plaintext bytes.
        assert_ne!(f.encode().unwrap()[4..], *b"guest");
        assert_eq!(f.as_deobfuscated_string(), "guest");
    }

    #[test]
    fn decoded_field_debug_shows_raw_bytes_not_a_guess() {
        let f = Field::string(ClientFieldId::Name, "x");
        let (decoded, _) = Field::decode(&f.encode().unwrap()).unwrap();
        let debug = format!("{decoded:?}");
        // Decode can't know this was text - it's honestly Bytes until
        // something calls as_string() on it.
        assert!(debug.contains("Bytes"));
    }

    #[test]
    fn locally_built_field_debug_shows_its_real_type() {
        let f = Field::u16(ClientFieldId::Uid, 42);
        let debug = format!("{f:?}");
        assert!(debug.contains("U16(42)"));
    }

    #[test]
    fn client_and_server_field_ids_never_collide_in_name() {
        assert_eq!(ClientFieldId::Name.name(), "HTLC_DATA_NAME");
        assert_eq!(ServerFieldId::Name.name(), "HTLS_DATA_NAME");
    }
}
