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
//!  16    4    len2     hc below plus every field's bytes, this frame only
//!  20    2    hc       number of field chunks that follow (== fields.len())
//!  22    …    fields   `hc` back-to-back `Field` chunks: tag(2) len(2) data(len)
//! ```
//!
//! `Transaction` holds `opcode`, `trans`, `flag`, and `fields`; `len`/`len2`
//! aren't kept since [`Transaction::encode`] recomputes them from `fields`
//! every time. `hc` isn't stored either; it's always `fields.len()`.
//!
//! Streaming decode needs `len2` *before* the fields are readable, so
//! [`TransactionHeader`] holds just the raw prefix. Reading proceeds
//! [`TransactionHeader::decode_from_client`]/[`decode_from_server`], then
//! [`TransactionHeader::finish_decode`] once `remaining_len()` more bytes
//! are in hand, producing the real `Transaction`.
//!
//! The wire's `type` value is resolved into a [`TransactionType`] once,
//! at decode time, and stored under `r#type` (the reference's own field
//! name, as a raw identifier since `type` is a Rust keyword) rather than
//! kept as a raw `u32`.

use std::{debug_assert_matches, fmt};

use crate::error::{ProtoError, Result};
use crate::field::Field;
use crate::field_id::{ClientFieldId, ServerFieldId};
use crate::opcode::{ClientOpcode, ServerOpcode, TransactionType};
use crate::types::{FileNameWithInfo, HlPath, UserNameWithInfo};
use crate::wire::{PacketReader, VecWriter};
use crate::AccessPrivileges;

/// A complete transaction: fixed fields plus its field list. `fields.len()`
/// *is* the wire's `hc`; there is no separate count to keep in sync.
/// Assumes a single-part transaction (`len == len2` on the wire); see the
/// [module docs](self) for `len`/`len2` and [`TransactionHeader`].
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
    pub fields: Vec<Field>,
}

/// A transaction's raw 20-byte wire prefix, decoded before its fields are
/// readable. See the [module docs](self) for why this is a separate type
/// from [`Transaction`].
#[derive(Debug)]
pub struct TransactionHeader {
    r#type: TransactionType,
    trans: u32,
    flag: u32,
    len2: u32,
}

impl Transaction {
    /// Build a request transaction from a set of fields.
    pub const fn request(opcode: ClientOpcode, trans: u32, fields: Vec<Field>) -> Self {
        Self::assemble(TransactionType::Request(opcode), trans, 0, fields)
    }

    /// Build a reply to this request transaction, carrying `fields`.
    pub fn reply(&self, fields: Vec<Field>) -> Self {
        debug_assert_matches!(self.r#type, TransactionType::Request(_));
        Self::assemble(TransactionType::Reply, self.trans, 0, fields)
    }

    /// Build a server-initiated transaction.
    pub const fn push(opcode: ServerOpcode, fields: Vec<Field>) -> Self {
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
    /// (`HTLS_DATA_TASKERROR`) is server-only, only a server ever sends
    /// one of these.
    pub fn reply_error(&self, message: impl Into<String>) -> Self {
        Self::assemble(
            TransactionType::Reply,
            self.trans,
            1,
            vec![Field::string(ServerFieldId::ErrorText, message)],
        )
    }

    /// Shared constructor: every wire representation (request/reply/push)
    /// differs only in `r#type`, `trans`, and `flag`.
    const fn assemble(r#type: TransactionType, trans: u32, flag: u32, fields: Vec<Field>) -> Self {
        Self {
            r#type,
            trans,
            flag,
            fields,
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
    pub fn field(&self, id: impl Into<u16>) -> Option<&Field> {
        let id = id.into();
        self.fields.iter().find(|f| f.id == id)
    }

    /// Find all fields with the given ID.
    pub fn fields(&self, id: impl Into<u16>) -> impl Iterator<Item = &Field> {
        let id = id.into();
        self.fields.iter().filter(move |f| f.id == id)
    }

    /// The first `USER_LIST` (300) field, decoded (`None` if absent, `Some(Err(_))` if malformed).
    pub fn user_list_entry(&self) -> Option<Result<UserNameWithInfo>> {
        self.field(ServerFieldId::UserList)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes()))
    }

    /// Every `USER_LIST` (300) field, decoded; fails on the first that doesn't decode.
    pub fn user_list_entries(&self) -> Result<Vec<UserNameWithInfo>> {
        self.fields(ServerFieldId::UserList)
            .map(|f| UserNameWithInfo::decode(&f.as_bytes()))
            .collect()
    }

    /// Every `FILE_LIST` (200) field, decoded; fails on the first that doesn't decode.
    pub fn file_list_entries(&self) -> Result<Vec<FileNameWithInfo>> {
        self.fields(ServerFieldId::FileList)
            .map(|f| FileNameWithInfo::decode(&f.as_bytes()))
            .collect()
    }

    /// A `FILE_PATH`-shaped field (`FilePath`/`FileNewPath`), decoded.
    pub fn file_path(&self, id: ClientFieldId) -> Option<Result<HlPath>> {
        self.field(id).map(|f| HlPath::decode(&f.as_bytes()))
    }

    /// The `LOGIN` field, deobfuscated, if present.
    pub fn login(&self) -> Option<String> {
        self.field(ClientFieldId::Login)
            .map(Field::as_deobfuscated_string)
    }

    /// The `PASSWORD` field, deobfuscated, if present.
    pub fn password(&self) -> Option<String> {
        self.field(ClientFieldId::Password)
            .map(Field::as_deobfuscated_string)
    }

    /// The `NAME` field, if present.
    pub fn name(&self) -> Option<String> {
        self.field(ClientFieldId::Name).map(Field::as_string)
    }

    /// The `ICON` field, if present.
    pub fn icon(&self) -> Option<u16> {
        self.field(ClientFieldId::Icon).and_then(Field::as_u16)
    }

    /// The `HTLC_DATA_ACCESS` field, if present.
    pub fn access(&self) -> Option<AccessPrivileges> {
        self.field(ClientFieldId::Access)
            .and_then(Field::as_u64)
            .map(AccessPrivileges::from_bits_truncate)
    }

    /// The `HTLC_DATA_CLIENTVERSION` (160) field, if present.
    pub fn client_version(&self) -> Option<u16> {
        self.field(ClientFieldId::Version).and_then(Field::as_u16)
    }

    /// Encode the full transaction: 20-byte fixed prefix, field count,
    /// then the field chunks themselves.
    ///
    /// Fails if `fields.len()` or any field's data exceeds a `u16`
    /// (65535), reachable with attacker-controlled input.
    pub fn encode(&self) -> crate::error::Result<Vec<u8>> {
        let hc = self.fields.len();
        let hc = u16::try_from(hc).map_err(|_| ProtoError::TooLong {
            what: "field count",
            len: hc,
        })?;

        let fields: Vec<u8> = self
            .fields
            .iter()
            .map(Field::encode)
            .collect::<Result<Vec<_>>>()?
            .into_iter()
            .flatten()
            .collect();

        let len = fields.len() + 2;
        let capacity = TransactionHeader::LEN + len;
        let len = u32::try_from(len).map_err(|_| ProtoError::TooLong {
            what: "fields len",
            len,
        })?;

        Ok(VecWriter::with_capacity(capacity)
            .u32(self.r#type.as_u32())
            .u32(self.trans)
            .u32(self.flag)
            .u32(len)
            .u32(len)
            .u16(hc)
            .bytes(&fields)
            .finish())
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
            .and_then(|s| s.try_into().ok())
            .ok_or(ProtoError::UnexpectedEof {
                needed: TransactionHeader::LEN,
                available: buf.len(),
            })?;
        let header = decode_prefix(prefix);
        let body_end = TransactionHeader::LEN + header.remaining_len() as usize;
        let body = buf
            .get(TransactionHeader::LEN..body_end)
            .ok_or(ProtoError::UnexpectedEof {
                needed: body_end,
                available: buf.len(),
            })?;
        header.finish_decode(body)
    }
}

impl TransactionHeader {
    /// Wire length in bytes: `type(4) | trans(4) | flag(4) | len(4) |
    /// len2(4)`.
    pub const LEN: usize = 20;

    /// Decode the fixed-size prefix only (through `len2`), assuming these
    /// bytes were sent **by a client**. [`TransactionHeader::remaining_len`]
    /// says how many more bytes to read, then
    /// [`TransactionHeader::finish_decode`] parses them.
    pub fn decode_from_client(buf: &[u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(*buf);
        let wire_type = r.u32();
        let trans = r.u32();
        let flag = r.u32();
        let _len = r.u32();
        let len2 = r.u32();
        r.finish();
        Self {
            r#type: TransactionType::from_client_wire(wire_type),
            trans,
            flag,
            len2,
        }
    }

    /// Like [`TransactionHeader::decode_from_client`], but for bytes known
    /// to have been sent **by a server** (a client reading its own
    /// incoming traffic).
    pub fn decode_from_server(buf: &[u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(*buf);
        let wire_type = r.u32();
        let trans = r.u32();
        let flag = r.u32();
        let _len = r.u32();
        let len2 = r.u32();
        r.finish();
        Self {
            r#type: TransactionType::from_server_wire(wire_type),
            trans,
            flag,
            len2,
        }
    }

    /// Bytes to read after [`TransactionHeader::decode_from_client`]/
    /// [`TransactionHeader::decode_from_server`], before
    /// [`TransactionHeader::finish_decode`] can run: `len2`, i.e. the
    /// 2-byte field count plus every field's bytes.
    pub const fn remaining_len(&self) -> u32 {
        self.len2
    }

    /// Finish decoding into a [`Transaction`]: `body` is exactly
    /// [`TransactionHeader::remaining_len`] bytes, the field count
    /// followed by the fields.
    pub fn finish_decode(self, body: &[u8]) -> Result<Transaction> {
        if body.len() < 2 {
            return Err(ProtoError::UnexpectedEof {
                needed: 2,
                available: body.len(),
            });
        }
        let hc = u16::from_be_bytes([body[0], body[1]]);

        let mut offset = 2;
        let mut fields = Vec::with_capacity(hc as usize);
        for _ in 0..hc {
            let (field, consumed) = Field::decode(&body[offset..])?;
            offset += consumed;
            fields.push(field);
        }

        Ok(Transaction {
            r#type: self.r#type,
            trans: self.trans,
            flag: self.flag,
            fields,
        })
    }
}

impl fmt::Debug for Transaction {
    /// Looks each field's name up in the correct one of
    /// [`crate::field_id::ClientFieldId`]/[`crate::field_id::ServerFieldId`],
    /// per [`Transaction::r#type`]'s direction.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let r#type = self.r#type();
        let mut out = f.debug_struct("Transaction");
        out.field("type", &r#type);
        if self.trans != 0 {
            out.field("trans", &self.trans);
        }
        if self.flag != 0 {
            out.field("flag", &self.flag);
        }
        match r#type {
            TransactionType::Request(_) => out.field(
                "fields",
                &self
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.id,
                            ClientFieldId::name_for_raw(field.id),
                            &field.value,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            TransactionType::Reply | TransactionType::Push(_) => out.field(
                "fields",
                &self
                    .fields
                    .iter()
                    .map(|field| {
                        (
                            field.id,
                            ServerFieldId::name_for_raw(field.id),
                            &field.value,
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
        }
        .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field_id::{ClientFieldId, ServerFieldId};
    use crate::opcode::ClientOpcode;

    #[test]
    fn prefix_is_20_bytes_full_frame_is_more() {
        let tx = Transaction::request(ClientOpcode::Ping, 1, vec![]);
        assert_eq!(TransactionHeader::LEN, 20);
        // Full wire output is prefix(20) + hc(2) + fields(0 here).
        assert_eq!(tx.encode().unwrap().len(), TransactionHeader::LEN + 2);
    }

    #[test]
    fn field_count_is_never_stored_only_derived() {
        for n in [0, 1, 5] {
            let fields = (0..n)
                .map(|i| Field::string(ClientFieldId::Text, format!("msg{i}")))
                .collect::<Vec<_>>();
            let tx = Transaction::request(ClientOpcode::Chat, 1, fields);
            let decoded = Transaction::decode_from_client(&tx.encode().unwrap()).unwrap();
            assert_eq!(decoded.fields.len(), n);
        }
    }

    #[test]
    fn transaction_round_trips() {
        let tx = Transaction::request(
            ClientOpcode::Login,
            1,
            vec![
                Field::obfuscated_string(ClientFieldId::Login, "guest"),
                Field::obfuscated_string(ClientFieldId::Password, ""),
                Field::u16(ClientFieldId::Version, 151),
            ],
        );
        let bytes = tx.encode().unwrap();
        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        assert_eq!(
            bytes.len(),
            TransactionHeader::LEN + header.remaining_len() as usize,
            "remaining_len() (== len2) must equal hc(2) + all field bytes"
        );

        let decoded = Transaction::decode_from_client(&bytes).unwrap();
        // decode produces FieldValue::Bytes, so compare interpreted values
        assert_eq!(decoded.trans, tx.trans);
        assert_eq!(decoded.fields.len(), tx.fields.len());
        assert_eq!(
            decoded
                .field(ClientFieldId::Login)
                .map(Field::as_deobfuscated_string)
                .as_deref(),
            Some("guest")
        );
    }

    #[test]
    fn streaming_decode_matches_one_shot_decode() {
        let tx = Transaction::request(
            ClientOpcode::Chat,
            7,
            vec![Field::string(ClientFieldId::Text, "hello")],
        );
        let bytes = tx.encode().unwrap();

        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        let body = &bytes
            [TransactionHeader::LEN..TransactionHeader::LEN + header.remaining_len() as usize];
        let streamed = header.finish_decode(body).unwrap();

        assert_eq!(streamed, Transaction::decode_from_client(&bytes).unwrap());
    }

    #[test]
    fn reply_carries_task_reply_sentinel_not_zero() {
        let request = Transaction::request(ClientOpcode::Ping, 42, vec![]);
        let tx = request.reply(vec![]);
        assert_eq!(tx.r#type(), TransactionType::Reply);
        assert!(tx.is_reply());
    }

    #[test]
    fn error_reply_shape() {
        let request = Transaction::request(ClientOpcode::Ping, 42, vec![]);
        let tx = request.reply_error("nope");
        assert_eq!(tx.trans, 42);
        assert!(tx.is_error());
        assert!(tx.is_reply());
        assert_eq!(
            tx.field(ServerFieldId::ErrorText).unwrap().as_string(),
            "nope"
        );
    }

    /// Two fields individually within the u16 limit, combined into one
    /// relayed field that isn't; `encode()` must fail, not truncate.
    #[test]
    fn oversized_relay_field_fails_to_encode_instead_of_corrupting_the_wire() {
        let long_nick = "n".repeat(60_000);
        let long_message = "m".repeat(60_000);
        let relay = format!("{long_nick}: {long_message}");
        assert!(relay.len() > u16::MAX as usize);

        let tx = Transaction::push(
            ServerOpcode::Chat,
            vec![Field::string(ServerFieldId::Text, relay)],
        );
        assert!(matches!(
            tx.encode(),
            Err(ProtoError::TooLong {
                what: "field data",
                ..
            })
        ));
    }

    #[test]
    fn len2_includes_the_hc_field_itself() {
        let tx = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );
        let bytes = tx.encode().unwrap();
        let prefix: [u8; TransactionHeader::LEN] =
            bytes[..TransactionHeader::LEN].try_into().unwrap();
        let header = TransactionHeader::decode_from_client(&prefix);
        let field_bytes = 4 + 2; // one field header + "hi"
        assert_eq!(header.remaining_len(), field_bytes + 2);
    }

    #[test]
    fn transaction_debug_uses_the_correct_direction_table() {
        let request = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hi")],
        );
        let request_debug = format!("{request:?}");
        assert!(request_debug.contains("HTLC_DATA_TEXT"));
        assert!(!request_debug.contains("HTLS_DATA"));

        let reply = request.reply(vec![Field::string(ServerFieldId::Text, "hi")]);
        let reply_debug = format!("{reply:?}");
        assert!(reply_debug.contains("HTLS_DATA"));
        assert!(!reply_debug.contains("HTLC_DATA"));
    }
}
