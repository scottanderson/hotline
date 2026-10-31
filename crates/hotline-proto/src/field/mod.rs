//! `Field`: the `(id, size, data)` records that make up a transaction's
//! parameter list.
//!
//! Field tags live as [`FieldId`], which `Field` stores directly;
//! direction is a property of the transaction, not the field.
//!
//! A field is in one of two states. A [`Built`] field holds a typed
//! [`FieldValue`], so `Debug` shows the real value, and its text stays text
//! until [`Field::encode`]. A [`Received`] field holds the bytes read off the
//! wire and the connection's encoding, until a reader such as
//! [`Field::as_string`] interprets them.

use crate::error::{ProtoError, Result};
use crate::numbers::decode as decode_number;
use crate::text_encoding::{self, Encoding};
use crate::wire::{PacketReader, VecWriter};

mod field_list;
mod id;
mod value;
pub use field_list::FieldList;
pub use id::FieldId;
pub use value::FieldValue;

/// The state of a field built to send: its typed value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built(FieldValue);

/// The state of a field read off the wire: its bytes, and the encoding
/// of the connection that read them.
#[derive(Clone, PartialEq, Eq)]
pub struct Received {
    bytes: Vec<u8>,
    encoding: &'static Encoding,
}

impl std::fmt::Debug for Received {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Received").field(&self.bytes).finish()
    }
}

/// A single transaction parameter, [`Built`] or [`Received`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field<S = Built> {
    /// Field tag.
    pub id: FieldId,
    /// The payload in this state.
    state: S,
}

impl Field {
    /// Build a field from an already-typed value.
    const fn new(id: FieldId, value: FieldValue) -> Self {
        Self {
            id,
            state: Built(value),
        }
    }

    /// The typed value.
    pub const fn value(&self) -> &FieldValue {
        &self.state.0
    }

    /// Build a field from raw bytes, e.g. a packed record
    /// ([`myField_FileNameWithInfo`] (200), [`myField_UserNameWithInfo`] (300))
    /// that has its own
    /// internal encoding.
    ///
    /// [`myField_FileNameWithInfo`]: crate::FieldId::FileNameWithInfo
    /// [`myField_UserNameWithInfo`]: crate::FieldId::UserNameWithInfo
    pub fn bytes(id: FieldId, data: impl Into<Vec<u8>>) -> Self {
        Self::new(id, FieldValue::Bytes(data.into()))
    }

    /// Build a 16-bit integer field ([`myField_UserID`] (103),
    /// [`myField_UserIconID`] (104), [`myField_ChatSubject`] (115) length
    /// prefixes, etc). Used where the exact wire width is confirmed fixed.
    ///
    /// [`myField_UserID`]: crate::FieldId::UserID
    /// [`myField_UserIconID`]: crate::FieldId::UserIconID
    /// [`myField_ChatSubject`]: crate::FieldId::ChatSubject
    pub const fn u16(id: FieldId, value: u16) -> Self {
        Self::new(id, FieldValue::U16(value))
    }

    /// Build the 64-bit [`myField_UserAccess`] (110) bitmask field. Not a "number" in
    /// the [`Field::u32`] sense (it's a bitmap, not a magnitude), so it
    /// doesn't go through [`crate::numbers`].
    ///
    /// [`myField_UserAccess`]: crate::FieldId::UserAccess
    pub const fn u64(id: FieldId, value: u64) -> Self {
        Self::new(id, FieldValue::U64(value))
    }

    /// Build a generic unsigned integer field, letting
    /// [`crate::numbers::encode`] pick the smaller of a 2-byte or
    /// 4-byte representation. Use for anything wider than 16 bits (a
    /// chat/thread ID, a file size, ...); use [`Field::u16`] instead for
    /// a field confirmed fixed at exactly 2 bytes.
    pub const fn u32(id: FieldId, value: u32) -> Self {
        Self::new(id, FieldValue::U32(value))
    }

    /// Build a string field, written to the wire in the encoding that
    /// [`Field::encode`] gets.
    pub fn string(id: FieldId, value: impl Into<String>) -> Self {
        Self::new(id, FieldValue::Text(value.into()))
    }

    /// Build a `LOGIN`/`PASSWORD`-style field: text obfuscated with
    /// Hotline's symmetric XOR-0xFF transform (`hl_code` in the
    /// reference C implementation).
    pub fn obfuscated_string(id: FieldId, value: impl Into<String>) -> Self {
        Self::new(id, FieldValue::ObfuscatedText(value.into()))
    }

    /// `field_id (2) | field_size (2) | data`.
    ///
    /// Fails if the encoded value is longer than 65535 bytes (the length
    /// prefix is a `u16`), reachable with attacker-controlled input.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let data = self.state.0.encode(encoding);
        let len = u16::try_from(data.len()).map_err(|_| ProtoError::TooLong {
            what: "field data",
            len: data.len(),
        })?;
        Ok(VecWriter::with_capacity(4 + data.len())
            .u16(self.id.into())
            .u16(len)
            .bytes(&data)
            .finish())
    }

    /// This field as a peer reads it after [`Field::encode`] with `encoding`.
    pub fn received(&self, encoding: &'static Encoding) -> Field<Received> {
        Field {
            id: self.id,
            state: Received {
                bytes: self.state.0.encode(encoding),
                encoding,
            },
        }
    }
}

impl Field<Received> {
    /// The encoding of the connection that read this field.
    pub const fn encoding(&self) -> &'static Encoding {
        self.state.encoding
    }

    /// Interpret this field as deobfuscated text.
    pub fn as_deobfuscated_string(&self) -> String {
        text_encoding::decode(self.encoding(), &xor_ff(&self.state.bytes))
    }

    /// Interpret this field's data as `u16`. Accepts a 1, 2, or 4 byte
    /// payload whose value fits in 16 bits.
    pub fn as_u16(&self) -> Option<u16> {
        self.as_u32()?.try_into().ok()
    }

    /// Interpret this field's data as `u32`. Accepts a 1, 2, or 4 byte
    /// payload.
    pub fn as_u32(&self) -> Option<u32> {
        decode_number(&self.state.bytes)
    }

    /// Interpret this field's data as the 64-bit [`myField_UserAccess`] (110)
    /// bitmask.
    /// `None` unless the payload is exactly 8 bytes.
    ///
    /// [`myField_UserAccess`]: crate::FieldId::UserAccess
    pub fn as_u64(&self) -> Option<u64> {
        let bytes: [u8; 8] = self.state.bytes.as_slice().try_into().ok()?;
        Some(u64::from_be_bytes(bytes))
    }

    /// Interpret this field's data as text in [`Field::encoding`]. The wire
    /// has no mandated text encoding; a legacy client picks its own,
    /// communicated out of band from the field itself (see
    /// `hotline_codec::TransactionCodec`).
    pub fn as_string(&self) -> String {
        text_encoding::decode(self.encoding(), &self.state.bytes)
    }

    /// [`Field::as_string`] plus the byte length of the text on the wire.
    pub fn as_string_len(&self) -> (String, usize) {
        (self.as_string(), self.state.bytes.len())
    }

    /// The raw bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.state.bytes
    }

    /// Interpret this field's data as a nested [`FieldList`].
    pub fn as_field_list(&self) -> Result<FieldList<Received>> {
        FieldList::decode(self.as_bytes(), self.encoding())
    }

    /// Byte length of the fixed prefix before a field's variable-length
    /// data: `field_id(2) | field_size(2)`.
    const PREFIX_LEN: usize = 4;

    /// Decode one field from the front of `buf`, returning the field and
    /// the number of bytes consumed.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<(Self, usize)> {
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
        let bytes = buf
            .get(Self::PREFIX_LEN..total)
            .ok_or(ProtoError::UnexpectedEof {
                needed: total,
                available: buf.len(),
            })?;
        Ok((
            Self {
                id: FieldId::from(id),
                state: Received {
                    bytes: bytes.to_vec(),
                    encoding,
                },
            },
            total,
        ))
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
    use crate::field::id::FieldId;

    #[test]
    fn field_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::string(FieldId::UserName, "RustHL");
        let encoded = f.encode(encoding).unwrap();
        let (decoded, consumed) = Field::decode(&encoded, encoding).unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.as_string(), "RustHL");
    }

    #[test]
    fn mac_roman_mappable_text_round_trips() {
        let encoding = text_encoding::DEFAULT;
        // e-acute exists in Mac OS Roman, so it round-trips exactly.
        let f = Field::string(FieldId::Data, "h\u{e9}llo");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap(), encoding).unwrap();
        assert_eq!(decoded.as_string(), "h\u{e9}llo");
    }

    #[test]
    fn text_outside_mac_roman_falls_back_to_question_mark() {
        let encoding = text_encoding::DEFAULT;
        // An emoji has no Mac OS Roman encoding, so it's lossy.
        let f = Field::string(FieldId::Data, "hello \u{1f389}");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap(), encoding).unwrap();
        assert_eq!(decoded.as_string(), "hello ?");
    }

    #[test]
    fn encode_rejects_data_over_65535_bytes_instead_of_truncating() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::bytes(FieldId::Data, vec![0u8; 65536]);
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
        let f = Field::u16(FieldId::UserID, 42);
        assert_eq!(*f.value(), FieldValue::U16(42));
        assert_eq!(f.received(text_encoding::DEFAULT).as_u16(), Some(42));
    }

    #[test]
    fn as_u16_reads_one_and_four_byte_payloads() {
        let encoding = text_encoding::DEFAULT;
        let one = Field::bytes(FieldId::UserID, vec![7]);
        assert_eq!(one.received(encoding).as_u16(), Some(7));
        let four = Field::bytes(FieldId::UserID, vec![0, 0, 0, 42]);
        assert_eq!(four.received(encoding).as_u16(), Some(42));
        let wide = Field::bytes(FieldId::UserID, vec![0, 1, 0, 0]);
        assert_eq!(wide.received(encoding).as_u16(), None);
    }

    #[test]
    fn u64_field_round_trips() {
        let f = Field::u64(FieldId::UserAccess, u64::MAX);
        assert_eq!(f.received(text_encoding::DEFAULT).as_u64(), Some(u64::MAX));
    }

    #[test]
    fn u32_field_picks_smallest_unsigned_width() {
        let encoding = text_encoding::DEFAULT;
        let small = Field::u32(FieldId::ChatID, 7);
        assert_eq!(small.encode(encoding).unwrap().len(), 4 + 2);
        assert_eq!(small.received(encoding).as_u32(), Some(7));

        let large = Field::u32(FieldId::FileSize, u32::MAX);
        assert_eq!(large.encode(encoding).unwrap().len(), 4 + 4);
        assert_eq!(large.received(encoding).as_u32(), Some(u32::MAX));
    }

    #[test]
    fn as_u32_accepts_either_a_2_or_4_byte_payload() {
        let encoding = text_encoding::DEFAULT;
        let short = Field::bytes(FieldId::ChatID, vec![0, 7]);
        assert_eq!(short.received(encoding).as_u32(), Some(7));

        let long = Field::bytes(FieldId::ChatID, vec![0, 0, 0, 7]);
        assert_eq!(long.received(encoding).as_u32(), Some(7));
    }

    #[test]
    fn obfuscated_string_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::obfuscated_string(FieldId::UserLogin, "guest");
        // On the wire it must NOT be the plaintext bytes.
        assert_ne!(f.encode(encoding).unwrap()[4..], *b"guest");
        assert_eq!(f.received(encoding).as_deobfuscated_string(), "guest");
    }

    #[test]
    fn decoded_field_debug_shows_raw_bytes_not_a_guess() {
        let encoding = text_encoding::DEFAULT;
        let f = Field::string(FieldId::UserName, "x");
        let (decoded, _) = Field::decode(&f.encode(encoding).unwrap(), encoding).unwrap();
        let debug = format!("{decoded:?}");
        // Decode can't know this was text: it stays bytes until
        // something calls as_string() on it.
        assert!(debug.contains("Received([120])"));
    }

    #[test]
    fn locally_built_field_debug_shows_its_real_type() {
        let f = Field::u16(FieldId::UserID, 42);
        let debug = format!("{f:?}");
        assert!(debug.contains("U16(42)"));
    }

    #[test]
    fn field_id_name_lookup() {
        assert_eq!(FieldId::UserName.original_name(), "myField_UserName");
    }

    #[test]
    fn decoding_an_unrecognized_field_id_preserves_its_wire_value() {
        let f = Field::bytes(FieldId::Unknown(9999), vec![1, 2, 3]);
        let (decoded, _) = Field::decode(
            &f.encode(text_encoding::DEFAULT).unwrap(),
            text_encoding::DEFAULT,
        )
        .unwrap();
        assert_eq!(decoded.id, FieldId::Unknown(9999));
    }
}
