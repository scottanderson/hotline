use crate::numbers::encode;
use crate::text_encoding::{self, Encoding};

use super::xor_ff;

/// A field's payload. Locally-built fields carry their real type until
/// encoded; decoded fields start as [`FieldValue::Bytes`] until a typed
/// accessor interprets them (see the [module docs](super)).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// Text, held as UTF-8, written on the wire as whatever encoding the
    /// caller passes to [`Field::encode`](super::Field::encode) (see
    /// [`Field::as_string`](super::Field::as_string)).
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
    pub(super) fn encode(&self, encoding: &'static Encoding) -> Vec<u8> {
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
