//! A `Transaction` is one complete message on the wire: a fixed-size
//! prefix (opcode, transaction ID, error flag, two length fields),
//! followed by a field count, followed by the fields themselves.
//!
//! ```text
//!  off  size  field    meaning
//!   0    4    type     opcode (see crate::opcode), a full u32, not u16
//!   4    4    trans    transaction ID
//!   8    4    flag     bit 0 = error; rest reserved/unused on the wire
//!  12    4    len      total size across every part of a split transaction
//!  16    4    len2     count below plus every field's bytes, this frame only
//!  20    2    count    number of field chunks that follow (== fields.len())
//!  22    …    fields   `count` back-to-back `Field` chunks: tag(2) len(2) data(len)
//! ```
//!
//! `Transaction` holds `opcode`, `trans`, `flag`, and `fields`; `len`/`len2`
//! aren't kept since [`Transaction::encode`] recomputes them from `fields`
//! every time. `count` isn't stored either; it's always `fields.len()`.
//!
//! Streaming decode needs `len2` *before* the fields are readable, so
//! [`TransactionHeader`] holds just the raw prefix. Reading proceeds
//! [`TransactionHeader::decode_from_client`]/[`decode_from_server`](TransactionHeader::decode_from_server), then
//! [`TransactionHeader::finish_decode`] once the body bytes are in hand,
//! producing the real `Transaction`.
//!
//! The wire's `type` value is resolved into a [`TransactionType`] once,
//! at decode time, and stored under `r#type` (mhxd's `struct hl_hdr`
//! field name, as a raw identifier since `type` is a Rust keyword) rather than
//! kept as a raw `u32`.

use std::{debug_assert_matches, fmt};

mod header;
pub use header::TransactionHeader;

use crate::error::{ProtoError, Result};
use crate::field::FieldId;
use crate::field::{Field, FieldList};
use crate::opcode::{Opcode, TransactionType};
use crate::text_encoding::Encoding;
use crate::types::{FileNameWithInfo, HlPath, UserNameWithInfo};
use crate::wire::VecWriter;
use crate::Access;

/// A complete transaction: fixed fields plus its field list. `fields.len()`
/// *is* the wire's `count`; there is no separate count to keep in sync.
/// Assumes a single-part transaction (`len == len2` on the wire); see the
/// [module docs](self) for `len`/`len2` and [`TransactionHeader`].
///
/// Carries no text encoding of its own; callers supply one per read/write call.
#[derive(Clone, PartialEq, Eq)]
pub struct Transaction {
    /// The wire `type` value, already resolved to which direction sent
    /// it. See the [module docs](self) for details.
    r#type: TransactionType,
    /// Transaction ID: client-assigned on a request, echoed on the reply,
    /// `0` on a push until [`Transaction::with_push_trans`] assigns it.
    trans: u32,
    /// Bit 0 is the error flag; the rest is reserved on the wire.
    flag: u32,
    /// The fields themselves.
    pub fields: FieldList,
}

impl Transaction {
    /// Build a request transaction from a set of fields.
    pub const fn request(opcode: Opcode, trans: u32, fields: Vec<Field>) -> Self {
        Self::assemble(TransactionType::Request(opcode), trans, 0, fields)
    }

    /// Build a reply to this request transaction, carrying `fields`.
    pub fn reply(&self, fields: Vec<Field>) -> Self {
        debug_assert_matches!(self.r#type, TransactionType::Request(_));
        Self::assemble(TransactionType::Reply, self.trans, 0, fields)
    }

    /// Build a server-initiated transaction.
    pub const fn push(opcode: Opcode, fields: Vec<Field>) -> Self {
        Self::assemble(TransactionType::Push(opcode), 0, 0, fields)
    }

    /// Assign this push's per-connection `trans` sequence number.
    pub const fn with_push_trans(mut self, next: Option<&mut u32>) -> Self {
        if let (TransactionType::Push(_), Some(next)) = (self.r#type, next) {
            self.trans = *next;
            *next += 1;
        }
        self
    }

    /// Build an error reply to this (request) transaction: the error flag
    /// set and an `ERROR_TEXT` field carrying `message`. `ERROR_TEXT`
    /// (`myField_ErrorText`) is server-only, only a server ever sends
    /// one of these.
    pub fn reply_error(&self, message: impl Into<String>) -> Self {
        Self::assemble(
            TransactionType::Reply,
            self.trans,
            1,
            vec![Field::string(FieldId::ErrorText, message)],
        )
    }

    /// Shared constructor: every wire representation (request/reply/push)
    /// differs only in `r#type`, `trans`, and `flag`.
    const fn assemble(r#type: TransactionType, trans: u32, flag: u32, fields: Vec<Field>) -> Self {
        Self {
            r#type,
            trans,
            flag,
            fields: FieldList::new(fields),
        }
    }

    /// This transaction's type, already resolved to the correct
    /// direction (see the [module docs](self)) - a plain field read, not
    /// a guess from a raw number.
    pub const fn r#type(&self) -> TransactionType {
        self.r#type
    }

    /// `true` if this transaction is a reply.
    pub const fn is_reply(&self) -> bool {
        matches!(self.r#type, TransactionType::Reply)
    }

    /// `true` if bit 0 of `flag` is set.
    pub const fn is_error(&self) -> bool {
        self.flag & 1 != 0
    }

    /// The transaction ID: client-assigned and nonzero on a request,
    /// echoed back on the matching reply.
    pub const fn trans(&self) -> u32 {
        self.trans
    }

    /// Find the first field with the given ID, if present.
    pub fn field(&self, id: FieldId) -> Option<&Field> {
        self.fields.field(id)
    }

    /// `id`'s field as text, if present.
    pub fn string(&self, id: FieldId, encoding: &'static Encoding) -> Option<String> {
        self.fields.string(id, encoding)
    }

    /// `id`'s field as text plus its `encoding` byte length, if present.
    /// See [`Field::as_string_len`].
    pub fn string_len(&self, id: FieldId, encoding: &'static Encoding) -> Option<(String, usize)> {
        self.fields.string_len(id, encoding)
    }

    /// Find all fields with the given ID.
    pub fn fields(&self, id: FieldId) -> impl Iterator<Item = &Field> {
        self.fields.fields(id)
    }

    /// The first `USER_LIST` (300) field, decoded (`None` if absent, `Some(Err(_))` if malformed).
    pub fn user_list_entry(&self, encoding: &'static Encoding) -> Option<Result<UserNameWithInfo>> {
        self.field(FieldId::UserList)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes(encoding), encoding))
    }

    /// Every `USER_LIST` (300) field, decoded; fails on the first that doesn't decode.
    pub fn user_list_entries(&self, encoding: &'static Encoding) -> Result<Vec<UserNameWithInfo>> {
        self.fields(FieldId::UserList)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes(encoding), encoding))
            .collect()
    }

    /// Every `FILE_LIST` (200) field, decoded; fails on the first that doesn't decode.
    pub fn file_list_entries(&self, encoding: &'static Encoding) -> Result<Vec<FileNameWithInfo>> {
        self.fields(FieldId::FileList)
            .map(|f| FileNameWithInfo::decode(&f.as_bytes(encoding), encoding))
            .collect()
    }

    /// A `FILE_PATH`-shaped field (`FilePath`/`FileNewPath`), decoded.
    pub fn file_path(&self, id: FieldId, encoding: &'static Encoding) -> Option<Result<HlPath>> {
        self.field(id)
            .map(|f| HlPath::decode(&f.as_bytes(encoding), encoding))
    }

    /// The `LOGIN` field, deobfuscated, if present.
    pub fn login(&self, encoding: &'static Encoding) -> Option<String> {
        self.field(FieldId::Login)
            .map(|f| f.as_deobfuscated_string(encoding))
    }

    /// The `PASSWORD` field, deobfuscated, if present.
    pub fn password(&self, encoding: &'static Encoding) -> Option<String> {
        self.field(FieldId::Password)
            .map(|f| f.as_deobfuscated_string(encoding))
    }

    /// The `NAME` field, if present.
    pub fn name(&self, encoding: &'static Encoding) -> Option<String> {
        self.string(FieldId::Name, encoding)
    }

    /// The `ICON` field, if present.
    pub fn icon(&self) -> Option<u16> {
        self.field(FieldId::Icon).and_then(Field::as_u16)
    }

    /// The `myField_NewsPath` (325) field, decoded.
    pub fn news_path(&self, encoding: &'static Encoding) -> Option<Result<HlPath>> {
        self.file_path(FieldId::NewsPath, encoding)
    }

    /// The `myField_NewsArtID` (326) field, if present.
    pub fn news_article_id(&self) -> Option<u32> {
        self.field(FieldId::NewsArtId).and_then(Field::as_u32)
    }

    /// The `myField_NewsCatName` (322) field, if present.
    pub fn news_category_name(&self, encoding: &'static Encoding) -> Option<String> {
        self.string(FieldId::NewsCatName, encoding)
    }

    /// The `myField_NewsArtTitle` (328) field, if present.
    pub fn news_article_title(&self, encoding: &'static Encoding) -> Option<String> {
        self.string(FieldId::NewsArtTitle, encoding)
    }

    /// The `myField_NewsArtDataFlav` (327) field, if present.
    pub fn news_article_flavor(&self, encoding: &'static Encoding) -> Option<String> {
        self.string(FieldId::NewsArtDataFlavor, encoding)
    }

    /// The `myField_NewsArtFlags` (334) field, if present.
    pub fn news_article_flags(&self) -> Option<u32> {
        self.field(FieldId::NewsArtFlags).and_then(Field::as_u32)
    }

    /// The `myField_NewsArtRecurseDel` (337) field: whether a delete
    /// should recurse into child articles.
    pub fn news_article_recurse_delete(&self) -> bool {
        self.field(FieldId::NewsArtRecurseDel)
            .and_then(Field::as_u32)
            .unwrap_or(0)
            != 0
    }

    /// The `myField_NewsArtData` (333) field's raw bytes, if present.
    pub fn news_article_data(&self, encoding: &'static Encoding) -> Option<Vec<u8>> {
        self.field(FieldId::NewsArtData)
            .map(|f| f.as_bytes(encoding).into_owned())
    }

    /// The `myField_UserAccess` field, if present.
    pub fn access(&self) -> Option<Access> {
        self.field(FieldId::Access)
            .and_then(Field::as_u64)
            .map(Access::from_bits_truncate)
    }

    /// The `myField_Vers` (160) field, if present.
    pub fn client_version(&self) -> Option<u16> {
        self.field(FieldId::Version).and_then(Field::as_u16)
    }

    /// Encode the full transaction: 20-byte fixed prefix, then
    /// [`FieldList::encode`]'s output as the body.
    ///
    /// Fails if `fields.len()` or any field's data exceeds a `u16`
    /// (65535), reachable with attacker-controlled input.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let body = FieldList::encode(&self.fields, encoding)?;
        let len = u32::try_from(body.len()).map_err(|_| ProtoError::TooLong {
            what: "fields len",
            len: body.len(),
        })?;

        Ok(
            VecWriter::with_capacity(TransactionHeader::LEN + body.len())
                .u32(self.r#type.as_u32())
                .u32(self.trans)
                .u32(self.flag)
                .u32(len)
                .u32(len)
                .bytes(&body)
                .finish(),
        )
    }

    /// Decode a complete, single-part transaction (prefix + body) from
    /// one contiguous buffer, assuming it was sent **by a client**.
    pub fn decode_from_client(buf: &[u8]) -> Result<Self> {
        Self::decode_with(buf, TransactionHeader::decode_from_client)
    }

    /// Like [`Transaction::decode_from_client`], but for bytes known to
    /// have been sent **by a server**.
    pub fn decode_from_server(buf: &[u8]) -> Result<Self> {
        Self::decode_with(buf, TransactionHeader::decode_from_server)
    }

    /// Shared one-shot decoder driving a caller-picked prefix decoder
    /// through [`TransactionHeader::finish_decode`].
    fn decode_with(
        buf: &[u8],
        decode_prefix: impl FnOnce(&[u8; TransactionHeader::LEN]) -> TransactionHeader,
    ) -> Result<Self> {
        let prefix: &[u8; TransactionHeader::LEN] = buf
            .get(..TransactionHeader::LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: TransactionHeader::LEN,
                available: buf.len(),
            })?;
        let header = decode_prefix(prefix);
        let body_end = TransactionHeader::LEN + header.data_len() as usize;
        let body = buf
            .get(TransactionHeader::LEN..body_end)
            .ok_or(ProtoError::UnexpectedEof {
                needed: body_end,
                available: buf.len(),
            })?;
        header.finish_decode(body)
    }
}

impl fmt::Debug for Transaction {
    /// Looks each field's name up via [`crate::field::FieldId`].
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("Transaction");
        out.field("type", &self.r#type());
        if self.trans != 0 {
            out.field("trans", &self.trans);
        }
        if self.flag != 0 {
            out.field("flag", &self.flag);
        }
        out.field(
            "fields",
            &self
                .fields
                .iter()
                .map(|field| (field.id, field.id.original_name(), &field.value))
                .collect::<Vec<_>>(),
        )
        .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::FieldId;
    use crate::opcode::Opcode;
    use crate::text_encoding;

    #[test]
    fn prefix_is_20_bytes_full_frame_is_more() {
        let encoding = text_encoding::DEFAULT;
        let tx = Transaction::request(Opcode::Ping, 1, vec![]);
        assert_eq!(TransactionHeader::LEN, 20);
        // Full wire output is prefix(20) + count(2) + fields(0 here).
        assert_eq!(
            tx.encode(encoding).unwrap().len(),
            TransactionHeader::LEN + 2
        );
    }

    #[test]
    fn field_count_is_never_stored_only_derived() {
        let encoding = text_encoding::DEFAULT;
        for n in [0, 1, 5] {
            let fields = (0..n)
                .map(|i| Field::string(FieldId::Text, format!("msg{i}")))
                .collect::<Vec<_>>();
            let tx = Transaction::request(Opcode::ChatSend, 1, fields);
            let decoded = Transaction::decode_from_client(&tx.encode(encoding).unwrap()).unwrap();
            assert_eq!(decoded.fields.len(), n);
        }
    }

    #[test]
    fn transaction_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::Login,
            1,
            vec![
                Field::obfuscated_string(FieldId::Login, "guest"),
                Field::obfuscated_string(FieldId::Password, ""),
                Field::u16(FieldId::Version, 151),
            ],
        );
        let bytes = tx.encode(encoding).unwrap();
        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        assert_eq!(
            bytes.len(),
            TransactionHeader::LEN + header.data_len() as usize,
            "data_len() (== len2) must equal count(2) + all field bytes"
        );

        let decoded = Transaction::decode_from_client(&bytes).unwrap();
        // decode produces FieldValue::Bytes, so compare interpreted values
        assert_eq!(decoded.trans, tx.trans);
        assert_eq!(decoded.fields.len(), tx.fields.len());
        assert_eq!(
            decoded
                .field(FieldId::Login)
                .map(|f| f.as_deobfuscated_string(encoding))
                .as_deref(),
            Some("guest")
        );
    }

    #[test]
    fn streaming_decode_matches_one_shot_decode() {
        let encoding = text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::ChatSend,
            7,
            vec![Field::string(FieldId::Text, "hello")],
        );
        let bytes = tx.encode(encoding).unwrap();

        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        let body =
            &bytes[TransactionHeader::LEN..TransactionHeader::LEN + header.data_len() as usize];
        let streamed = header.finish_decode(body).unwrap();

        assert_eq!(streamed, Transaction::decode_from_client(&bytes).unwrap());
    }

    #[test]
    fn reply_carries_task_reply_sentinel_not_zero() {
        let request = Transaction::request(Opcode::Ping, 42, vec![]);
        let tx = request.reply(vec![]);
        assert_eq!(tx.r#type(), TransactionType::Reply);
        assert!(tx.is_reply());
    }

    #[test]
    fn error_reply_shape() {
        let encoding = text_encoding::DEFAULT;
        let request = Transaction::request(Opcode::Ping, 42, vec![]);
        let tx = request.reply_error("nope");
        assert_eq!(tx.trans, 42);
        assert!(tx.is_error());
        assert!(tx.is_reply());
        assert_eq!(
            tx.field(FieldId::ErrorText).unwrap().as_string(encoding),
            "nope"
        );
    }

    /// Two fields individually within the u16 limit, combined into one
    /// relayed field that isn't; `encode()` must fail, not truncate.
    #[test]
    fn oversized_relay_field_fails_to_encode_instead_of_corrupting_the_wire() {
        let encoding = text_encoding::DEFAULT;
        let long_nick = "n".repeat(60_000);
        let long_message = "m".repeat(60_000);
        let relay = format!("{long_nick}: {long_message}");
        assert!(relay.len() > u16::MAX as usize);

        let tx = Transaction::push(Opcode::ChatMsg, vec![Field::string(FieldId::Text, relay)]);
        assert!(matches!(
            tx.encode(encoding),
            Err(ProtoError::TooLong {
                what: "field data",
                ..
            })
        ));
    }

    #[test]
    fn len2_includes_the_count_field_itself() {
        let encoding = text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Text, "hi")],
        );
        let bytes = tx.encode(encoding).unwrap();
        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        let field_bytes = 4 + 2; // one field header + "hi"
        assert_eq!(header.data_len(), field_bytes + 2);
    }

    #[test]
    fn transaction_debug_shows_known_field_names_regardless_of_direction() {
        let request = Transaction::request(
            Opcode::FileSetinfo,
            1,
            vec![Field::string(FieldId::FileNewName, "new.txt")],
        );
        let request_debug = format!("{request:?}");
        assert!(request_debug.contains("myField_FileNewName"));
        assert!(!request_debug.contains("(unrecognized field id)"));

        let reply = request.reply_error("boom");
        let reply_debug = format!("{reply:?}");
        assert!(reply_debug.contains("myField_ErrorText"));
        assert!(!reply_debug.contains("(unrecognized field id)"));
    }
}
