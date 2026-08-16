//! `Field`: the `(id, size, data)` records that make up a transaction's
//! parameter list.
//!
//! Field tags live in [`crate::field_id`] as [`crate::field_id::FieldId`];
//! `Field` itself stores a raw `u16` id, since direction is a property of
//! the transaction, not the field.
//!
//! The payload is [`FieldValue`], not a bare `Vec<u8>`, so `Debug` shows
//! the actual typed value. A field just decoded off the wire is
//! [`FieldValue::Bytes`] until something interprets it (`as_string()`,
//! `as_u16()`, ...); there's no schema table to guess a raw id's type.

use std::borrow::Cow;

use crate::error::{ProtoError, Result};
use crate::numbers::{decode as decode_number, encode};
use crate::text_encoding::{self, Encoding};
use crate::wire::{PacketReader, VecWriter};

/// A field's payload. Locally-built fields carry their real type end to
/// end; decoded fields start as [`FieldValue::Bytes`] until a typed
/// accessor interprets them (see the [module docs](self)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Text, held as UTF-8, written on the wire as whatever encoding the
    /// caller passes to [`Field::encode`] (see [`Field::as_string`]).
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
    /// Anything else: a packed record, an image, or, for a field just
    /// decoded off the wire, uninterpreted bytes.
    Bytes(Vec<u8>),
}

impl FieldValue {
    /// Encode to wire bytes; text fields are encoded as `encoding`.
    fn encode(&self, encoding: &'static Encoding) -> Vec<u8> {
        match self {
            Self::Text(s) => text_encoding::encode(encoding, s),
            Self::ObfuscatedText(s) => xor_ff(&text_encoding::encode(encoding, s)),
            Self::U16(v) => v.to_be_bytes().to_vec(),
            Self::U32(v) => encode(*v).to_vec(),
            Self::U64(v) => v.to_be_bytes().to_vec(),
            Self::Bytes(b) => b.clone(),
        }
    }
}

/// A single transaction parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    /// Field tag. Pass a [`crate::field_id::FieldId`] constant to any
    /// constructor here (it converts via `Into<u16>`); stored as a plain
    /// `u16` since that's what's actually on the wire and what decoding
    /// produces before direction is known.
    pub id: u16,
    /// The typed payload. See [`FieldValue`].
    pub value: FieldValue,
}

impl Field {
    /// Build a field from an already-typed value.
    fn new(id: impl Into<u16>, value: FieldValue) -> Self {
        Self {
            id: id.into(),
            value,
        }
    }

    /// Build a field from raw bytes, e.g. a packed record
    /// (`FILE_NAME_WITH_INFO`, `USER_NAME_WITH_INFO`) that has its own
    /// internal encoding.
    pub fn bytes(id: impl Into<u16>, data: impl Into<Vec<u8>>) -> Self {
        Self::new(id, FieldValue::Bytes(data.into()))
    }

    /// Build a 16-bit integer field (`UID`, `ICON`, `CHAT_SUBJECT` length
    /// prefixes, etc). Used where the exact wire width is confirmed fixed.
    pub fn u16(id: impl Into<u16>, value: u16) -> Self {
        Self::new(id, FieldValue::U16(value))
    }

    /// Build the 64-bit `USER_ACCESS` bitmask field. Not a "number" in
    /// the [`Field::u32`] sense (it's a bitmap, not a magnitude), so it
    /// doesn't go through [`crate::numbers`].
    pub fn u64(id: impl Into<u16>, value: u64) -> Self {
        Self::new(id, FieldValue::U64(value))
    }

    /// Build a generic unsigned integer field, letting
    /// [`crate::numbers::encode`] pick the smaller of a 2-byte or
    /// 4-byte representation. Use for anything wider than 16 bits (a
    /// chat/thread ID, a file size, ...); use [`Field::u16`] instead for
    /// a field confirmed fixed at exactly 2 bytes.
    pub fn u32(id: impl Into<u16>, value: u32) -> Self {
        Self::new(id, FieldValue::U32(value))
    }

    /// Build a string field. See [`Field::as_string`] for the encoding
    /// used to write it to the wire.
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
    /// was actually constructed or decoded.
    pub fn as_deobfuscated_string(&self, encoding: &'static Encoding) -> String {
        match &self.value {
            FieldValue::ObfuscatedText(s) => s.clone(),
            FieldValue::Bytes(b) => text_encoding::decode(encoding, &xor_ff(b)),
            other => text_encoding::decode(encoding, &xor_ff(&other.encode(encoding))),
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

    /// Interpret this field's data as `u32`. Accepts a 2 or 4 byte
    /// payload, since a value that happens to be small is free to
    /// arrive encoded in the shorter width.
    pub fn as_u32(&self) -> Option<u32> {
        match &self.value {
            FieldValue::U32(v) => Some(*v),
            FieldValue::U16(v) => Some(*v as u32),
            FieldValue::Bytes(b) => decode_number(b),
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

    /// Interpret this field's data as text, decoding raw bytes as
    /// `encoding`. The wire has no mandated text encoding; a legacy
    /// client picks its own, communicated out of band from the field
    /// itself (see `hotline_codec::TransactionCodec`).
    pub fn as_string(&self, encoding: &'static Encoding) -> String {
        match &self.value {
            FieldValue::Text(s) | FieldValue::ObfuscatedText(s) => s.clone(),
            FieldValue::Bytes(b) => text_encoding::decode(encoding, b),
            other => text_encoding::decode(encoding, &other.encode(encoding)),
        }
    }

    /// [`Field::as_string`] plus the byte length of its `encoding`
    /// representation on the wire, from the same bytes rather than two
    /// separate passes over them.
    pub fn as_string_len(&self, encoding: &'static Encoding) -> (String, usize) {
        match &self.value {
            FieldValue::Text(s) | FieldValue::ObfuscatedText(s) => {
                (s.clone(), text_encoding::encode(encoding, s).len())
            }
            FieldValue::Bytes(b) => (text_encoding::decode(encoding, b), b.len()),
            other => {
                let bytes = other.encode(encoding);
                (text_encoding::decode(encoding, &bytes), bytes.len())
            }
        }
    }

    /// Raw bytes, borrowed when already `FieldValue::Bytes`, encoded fresh otherwise.
    pub fn as_bytes(&self, encoding: &'static Encoding) -> Cow<'_, [u8]> {
        match &self.value {
            FieldValue::Bytes(b) => Cow::Borrowed(b),
            other => Cow::Owned(other.encode(encoding)),
        }
    }

    /// `field_id (2) | field_size (2) | data`.
    ///
    /// Fails if the encoded value is longer than 65535 bytes (the length
    /// prefix is a `u16`), reachable with attacker-controlled input.
    pub fn encode(&self, encoding: &'static Encoding) -> crate::error::Result<Vec<u8>> {
        let data = self.value.encode(encoding);
        let len = u16::try_from(data.len()).map_err(|_| ProtoError::TooLong {
            what: "field data",
            len: data.len(),
        })?;
        Ok(VecWriter::with_capacity(4 + data.len())
            .u16(self.id)
            .u16(len)
            .bytes(&data)
            .finish())
    }

    /// Byte length of the fixed prefix before a field's variable-length
    /// data: `field_id(2) | field_size(2)`.
    const PREFIX_LEN: usize = 4;

    /// Decode one field from the front of `buf`, returning the field and
    /// the number of bytes consumed. Always produces [`FieldValue::Bytes`].
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        let prefix: [u8; Self::PREFIX_LEN] = buf
            .get(..Self::PREFIX_LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: Self::PREFIX_LEN,
                available: buf.len(),
            })?;
        let mut r = PacketReader::new(prefix);
        let id = r.u16();
        let size = r.u16() as usize;
        r.finish();

        let total = Self::PREFIX_LEN + size;
        if buf.len() < total {
            return Err(ProtoError::UnexpectedEof {
                needed: total,
                available: buf.len(),
            });
        }
        Ok((
            Field {
                id,
                value: FieldValue::Bytes(buf[Self::PREFIX_LEN..total].to_vec()),
            },
            total,
        ))
    }
}

/// A list of fields: a transaction's body, or the shape of one nested
/// account record inside `HTLC_HDR_GET_USER_LIST` (348) /
/// `HTLC_HDR_SET_USER_LIST` (349) (see
/// [`crate::transaction::Transaction::encode_field_list`]). `Deref`s to
/// `[Field]` for anything not covered by its own lookup methods below.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldList(Vec<Field>);

impl FieldList {
    pub(crate) const fn new(fields: Vec<Field>) -> Self {
        Self(fields)
    }

    /// Find the first field with the given ID, if present.
    pub fn field(&self, id: impl Into<u16>) -> Option<&Field> {
        let id = id.into();
        self.0.iter().find(|f| f.id == id)
    }

    /// Find all fields with the given ID.
    pub fn fields(&self, id: impl Into<u16>) -> impl Iterator<Item = &Field> {
        let id = id.into();
        self.0.iter().filter(move |f| f.id == id)
    }

    /// `id`'s field as text, if present.
    pub fn string(&self, id: impl Into<u16>, encoding: &'static Encoding) -> Option<String> {
        self.field(id).map(|f| f.as_string(encoding))
    }

    /// `id`'s field as text plus its `encoding` byte length, if present.
    /// See [`Field::as_string_len`].
    pub fn string_len(
        &self,
        id: impl Into<u16>,
        encoding: &'static Encoding,
    ) -> Option<(String, usize)> {
        self.field(id).map(|f| f.as_string_len(encoding))
    }
}

impl std::ops::Deref for FieldList {
    type Target = [Field];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// XOR every byte with `0xFF`. Symmetric, so the same function encodes
/// and decodes.
fn xor_ff(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|&b| !b).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field_id::FieldId;

    #[test]
    fn field_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::string(FieldId::Name, "RustHL");
        let encoded = f.encode(encoding).unwrap();
        let (decoded, consumed) = Field::decode(&encoded).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.as_string(encoding), "RustHL");
    }

    #[test]
    fn mac_roman_mappable_text_round_trips() {
        let encoding = text_encoding::DEFAULT;
        // e-acute exists in Mac OS Roman, so it round-trips exactly.
        let f = Field::string(FieldId::Text, "h\u{e9}llo");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap()).unwrap();
        assert_eq!(decoded.as_string(encoding), "h\u{e9}llo");
    }

    #[test]
    fn text_outside_mac_roman_falls_back_to_question_mark() {
        let encoding = text_encoding::DEFAULT;
        // An emoji has no Mac OS Roman encoding, so it's lossy.
        let f = Field::string(FieldId::Text, "hello \u{1f389}");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap()).unwrap();
        assert_eq!(decoded.as_string(encoding), "hello ?");
    }

    #[test]
    fn encode_rejects_data_over_65535_bytes_instead_of_truncating() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::bytes(FieldId::Text, vec![0u8; 65536]);
        assert!(matches!(
            f.encode(encoding),
            Err(ProtoError::TooLong {
                what: "field data",
                len: 65536
            })
        ));
    }

    #[test]
    fn u16_field_round_trips() {
        let f = Field::u16(FieldId::UserId, 42);
        assert_eq!(f.value, FieldValue::U16(42));
        assert_eq!(f.as_u16(), Some(42));
    }

    #[test]
    fn u64_field_round_trips() {
        let f = Field::u64(FieldId::Access, u64::MAX);
        assert_eq!(f.as_u64(), Some(u64::MAX));
    }

    #[test]
    fn u32_field_picks_smallest_unsigned_width() {
        let encoding = text_encoding::DEFAULT;
        let small = Field::u32(FieldId::ChatId, 7);
        assert_eq!(small.encode(encoding).unwrap().len(), 4 + 2);
        assert_eq!(small.as_u32(), Some(7));

        let large = Field::u32(FieldId::FileSize, u32::MAX);
        assert_eq!(large.encode(encoding).unwrap().len(), 4 + 4);
        assert_eq!(large.as_u32(), Some(u32::MAX));
    }

    #[test]
    fn as_u32_accepts_either_a_2_or_4_byte_payload() {
        let short = Field::u32(FieldId::ChatId, 7);
        assert_eq!(short.as_u32(), Some(7));

        // A field just decoded off the wire is raw bytes regardless of
        // which width the sender picked.
        let long = Field::bytes(FieldId::ChatId, vec![0, 0, 0, 7]);
        assert_eq!(long.as_u32(), Some(7));
    }

    #[test]
    fn obfuscated_string_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::obfuscated_string(FieldId::Login, "guest");
        // On the wire it must NOT be the plaintext bytes.
        assert_ne!(f.encode(encoding).unwrap()[4..], *b"guest");
        assert_eq!(f.as_deobfuscated_string(encoding), "guest");
    }

    #[test]
    fn decoded_field_debug_shows_raw_bytes_not_a_guess() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::string(FieldId::Name, "x");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap()).unwrap();
        let debug = format!("{decoded:?}");
        // Decode can't know this was text - it's honestly Bytes until
        // something calls as_string() on it.
        assert!(debug.contains("Bytes"));
    }

    #[test]
    fn locally_built_field_debug_shows_its_real_type() {
        let f = Field::u16(FieldId::UserId, 42);
        let debug = format!("{f:?}");
        assert!(debug.contains("U16(42)"));
    }

    #[test]
    fn field_id_name_lookup() {
        assert_eq!(FieldId::Name.name(), "myField_UserName");
    }
}
