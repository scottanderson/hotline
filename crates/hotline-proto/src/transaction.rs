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
//! `Transaction` holds every field above directly (`trans`, `flag`,
//! `len`, `len2`, `fields`) rather than wrapping a separate header type.
//! `hc` isn't stored; it's always `fields.len()`. Streaming decode
//! ([`Transaction::decode_prefix_from_client`]/
//! [`Transaction::decode_prefix_from_server`] + [`Transaction::finish_decode`])
//! splits prefix from body since `len2` must be known before reading more.
//!
//! There's no raw `type_: u32` field: the wire's `type` value is
//! resolved into an [`Opcode`] once and stored as that.

use std::fmt;

use crate::error::{ProtoError, Result};
use crate::field::Field;
use crate::field_id::{ClientFieldId, ServerFieldId};
use crate::opcode::{ClientOpcode, Opcode, ServerOpcode};
use crate::wire::{PacketReader, PacketWriter};

/// A complete transaction: fixed fields plus its field list. `fields.len()`
/// *is* the wire's `hc`; there is no separate count to keep in sync.
///
/// Assumes a single-part transaction (`len == len2`). Reassembling
/// multi-part (split) transactions is the transport layer's job, since it
/// requires buffering across reads.
#[derive(Clone, PartialEq, Eq)]
pub struct Transaction {
    /// The wire `type` value, already resolved to which direction sent
    /// it. See the [module docs](self) for why this replaces a raw
    /// `type_: u32`.
    opcode: Opcode,
    /// Transaction ID. Client-assigned and nonzero on a request; echoed
    /// back on the matching reply. Always `0` on a server-initiated push.
    /// Private so a reply can only be built through [`Transaction::reply`]/
    /// [`Transaction::reply_error`], which copy it from the request they're
    /// answering rather than letting a caller pass the wrong id by hand.
    trans: u32,
    /// Bit 0 is the error flag; the rest is reserved on the wire.
    pub flag: u32,
    /// Total size of the data section across every part of a (possibly
    /// split) transaction.
    pub len: u32,
    /// Byte count of everything after `len2` itself: the 2-byte field
    /// count plus every field's bytes, this frame only.
    pub len2: u32,
    /// The fields themselves.
    pub fields: Vec<Field>,
}

/// Bytes needed before `len2` (and therefore the total frame size) is
/// known. Streaming transports read this many bytes first, then
/// [`Transaction::remaining_len`] more.
pub const PREFIX_LEN: usize = 20;

impl Transaction {
    /// Build a request transaction from a set of fields.
    pub fn request(opcode: ClientOpcode, trans: u32, fields: Vec<Field>) -> Self {
        Self::assemble(Opcode::Request(opcode), trans, 0, fields)
    }

    /// Build a reply to this (request) transaction, carrying `fields`.
    pub fn reply(&self, fields: Vec<Field>) -> Self {
        Self::assemble(Opcode::Reply, self.trans, 0, fields)
    }

    /// Build a server-initiated push transaction (chat relay, user list
    /// change, news post, ...). It's not a reply to anything, so it carries
    /// `trans = 0` and the push's own opcode rather than the
    /// [`crate::opcode::TASK_REPLY`] sentinel.
    pub fn push(opcode: ServerOpcode, fields: Vec<Field>) -> Self {
        Self::assemble(Opcode::Push(opcode), 0, 0, fields)
    }

    /// Build an error reply to this (request) transaction: the error flag
    /// set and an `ERROR_TEXT` field carrying `message`. `ERROR_TEXT`
    /// (`HTLS_DATA_TASKERROR`) is server-only, only a server ever sends
    /// one of these.
    pub fn reply_error(&self, message: impl Into<String>) -> Self {
        Self::assemble(
            Opcode::Reply,
            self.trans,
            1,
            vec![Field::string(ServerFieldId::ErrorText, message)],
        )
    }

    /// Shared constructor: every wire representation (request/reply/push)
    /// differs only in `opcode`, `trans`, and `flag`.
    ///
    /// `len2` is a saturating-arithmetic estimate; it can't panic or wrap,
    /// but the real "does this fit on the wire" check happens once, in
    /// [`Transaction::encode`].
    fn assemble(opcode: Opcode, trans: u32, flag: u32, fields: Vec<Field>) -> Self {
        let len2 = fields
            .iter()
            .map(|f| 4u32.saturating_add(u32::try_from(f.as_bytes().len()).unwrap_or(u32::MAX)))
            .fold(2u32, u32::saturating_add);
        Self {
            opcode,
            trans,
            flag,
            len: len2,
            len2,
            fields,
        }
    }

    /// This transaction's opcode, already resolved to the correct
    /// direction (see the [module docs](self)) - a plain field read, not
    /// a guess from a raw number.
    pub const fn opcode(&self) -> Opcode {
        self.opcode
    }

    /// `true` if this transaction is a reply.
    pub const fn is_reply(&self) -> bool {
        matches!(self.opcode, Opcode::Reply)
    }

    /// The raw wire `type` value, mainly for logging. Prefer
    /// [`Transaction::opcode`] for anything that branches on it.
    pub const fn type_(&self) -> u32 {
        self.opcode.as_u32()
    }

    /// `true` if bit 0 of `flag` is set.
    pub const fn is_error(&self) -> bool {
        self.flag & 1 != 0
    }

    /// The transaction ID: client-assigned and nonzero on a request,
    /// echoed back on the matching reply, always `0` on a push. Used to
    /// match a reply to the request that's still awaiting it.
    pub const fn trans(&self) -> u32 {
        self.trans
    }

    /// Find the first field with the given ID, if present.
    pub fn field(&self, id: impl Into<u16>) -> Option<&Field> {
        let id = id.into();
        self.fields.iter().find(|f| f.id == id)
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
        let hc = u16::try_from(self.fields.len()).map_err(|_| ProtoError::TooLong {
            what: "field count",
            len: self.fields.len(),
        })?;

        let mut field_bytes = Vec::new();
        for field in &self.fields {
            let encoded = field.encode()?;
            field_bytes.extend(encoded);
        }

        Ok(PacketWriter::<PREFIX_LEN>::new()
            .u32(self.opcode.as_u32())
            .u32(self.trans)
            .u32(self.flag)
            .u32(self.len)
            .u32(self.len2)
            .finish()
            .into_iter()
            .chain(hc.to_be_bytes())
            .chain(field_bytes)
            .collect())
    }

    /// Decode the fixed-size prefix only (through `len2`), with an empty
    /// field list, assuming these bytes were sent **by a client**.
    /// [`Transaction::remaining_len`] says how many more bytes to read,
    /// then [`Transaction::finish_decode`] parses them.
    pub const fn decode_prefix_from_client(buf: &[u8; PREFIX_LEN]) -> Self {
        let r = PacketReader::new(*buf);
        let (r, type_) = r.u32();
        let (r, trans) = r.u32();
        let (r, flag) = r.u32();
        let (r, len) = r.u32();
        let (_, len2) = r.u32();
        Self {
            opcode: Opcode::from_client_wire(type_),
            trans,
            flag,
            len,
            len2,
            fields: Vec::new(),
        }
    }

    /// Like [`Transaction::decode_prefix_from_client`], but for bytes
    /// known to have been sent **by a server** (a client reading its own
    /// incoming traffic).
    pub const fn decode_prefix_from_server(buf: &[u8; PREFIX_LEN]) -> Self {
        let r = PacketReader::new(*buf);
        let (r, type_) = r.u32();
        let (r, trans) = r.u32();
        let (r, flag) = r.u32();
        let (r, len) = r.u32();
        let (_, len2) = r.u32();
        Self {
            opcode: Opcode::from_server_wire(type_),
            trans,
            flag,
            len,
            len2,
            fields: Vec::new(),
        }
    }

    /// Bytes to read after [`Transaction::decode_prefix_from_client`]/
    /// [`Transaction::decode_prefix_from_server`], before
    /// [`Transaction::finish_decode`] can run: `len2`, i.e. the 2-byte
    /// field count plus every field's bytes.
    pub const fn remaining_len(&self) -> u32 {
        self.len2
    }

    /// Finish decoding a transaction whose prefix was already read via
    /// [`Transaction::decode_prefix_from_client`]/
    /// [`Transaction::decode_prefix_from_server`]: `body` is exactly
    /// [`Transaction::remaining_len`] bytes, the field count followed by
    /// the fields.
    pub fn finish_decode(self, body: &[u8]) -> Result<Self> {
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

        Ok(Self { fields, ..self })
    }

    /// Decode a complete, single-part transaction (prefix + body) from
    /// one contiguous buffer, assuming it was sent **by a client**.
    pub fn decode_from_client(buf: &[u8]) -> Result<Self> {
        Self::decode_with(buf, Self::decode_prefix_from_client)
    }

    /// Like [`Transaction::decode_from_client`], but for bytes known to
    /// have been sent **by a server**.
    pub fn decode_from_server(buf: &[u8]) -> Result<Self> {
        Self::decode_with(buf, Self::decode_prefix_from_server)
    }

    /// Shared one-shot decoder driving a caller-picked prefix decoder
    /// through [`Transaction::finish_decode`].
    fn decode_with(
        buf: &[u8],
        decode_prefix: impl FnOnce(&[u8; PREFIX_LEN]) -> Self,
    ) -> Result<Self> {
        let prefix: &[u8; PREFIX_LEN] = buf
            .get(..PREFIX_LEN)
            .and_then(|s| s.try_into().ok())
            .ok_or(ProtoError::UnexpectedEof {
                needed: PREFIX_LEN,
                available: buf.len(),
            })?;
        let partial = decode_prefix(prefix);
        let body_end = PREFIX_LEN + partial.remaining_len() as usize;
        let body = buf
            .get(PREFIX_LEN..body_end)
            .ok_or(ProtoError::UnexpectedEof {
                needed: body_end,
                available: buf.len(),
            })?;
        partial.finish_decode(body)
    }
}

impl fmt::Debug for Transaction {
    /// Looks each field's name up in the correct one of
    /// [`crate::field_id::ClientFieldId`]/[`crate::field_id::ServerFieldId`],
    /// per [`Transaction::opcode`]'s direction.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let opcode = self.opcode();
        let mut out = f.debug_struct("Transaction");
        out.field("type_", &opcode);
        if self.trans != 0 {
            out.field("trans", &self.trans);
        }
        if self.flag != 0 {
            out.field("flag", &self.flag);
        }
        out.field("len", &self.len);
        if self.len != self.len2 {
            out.field("len2", &self.len2);
        }
        match opcode {
            Opcode::Request(_) => out.field(
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
            Opcode::Reply | Opcode::Push(_) => out.field(
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
            // The wire value isn't a recognized opcode in either table,
            // so there's no schema to name fields against; fall back to
            // the plain, honest (unnamed) Field::Debug.
            Opcode::Unknown(_) => out.field("fields", &self.fields),
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
        assert_eq!(PREFIX_LEN, 20);
        // Full wire output is prefix(20) + hc(2) + fields(0 here).
        assert_eq!(tx.encode().unwrap().len(), PREFIX_LEN + 2);
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
        assert_eq!(
            bytes.len(),
            PREFIX_LEN + tx.remaining_len() as usize,
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

        let prefix: [u8; PREFIX_LEN] = bytes[..PREFIX_LEN].try_into().unwrap();
        let partial = Transaction::decode_prefix_from_client(&prefix);
        let body = &bytes[PREFIX_LEN..PREFIX_LEN + partial.remaining_len() as usize];
        let streamed = partial.finish_decode(body).unwrap();

        assert_eq!(streamed, Transaction::decode_from_client(&bytes).unwrap());
    }

    #[test]
    fn reply_carries_task_reply_sentinel_not_zero() {
        let request = Transaction::request(ClientOpcode::Ping, 42, vec![]);
        let tx = request.reply(vec![]);
        assert_eq!(tx.opcode(), Opcode::Reply);
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
        let field_bytes = 4 + 2; // one field header + "hi"
        assert_eq!(tx.len2, field_bytes + 2);
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
