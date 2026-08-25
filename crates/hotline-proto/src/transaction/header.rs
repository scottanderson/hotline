//! [`TransactionHeader`]: a transaction's raw 20-byte wire prefix,
//! decoded before its fields are readable. See the
//! [module docs](crate::transaction) for why this is a separate type
//! from [`Transaction`](crate::transaction::Transaction).

use crate::error::Result;
use crate::field::FieldList;
use crate::opcode::TransactionType;
use crate::transaction::Transaction;
use crate::wire::PacketReader;

/// A transaction's raw 20-byte wire prefix, decoded before its fields are
/// readable. See the [module docs](crate::transaction) for why this is a
/// separate type from [`Transaction`].
#[derive(Debug)]
pub struct TransactionHeader {
    pub(super) r#type: TransactionType,
    pub(super) trans: u32,
    pub(super) flag: u32,
    len2: u32,
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
        let fields = FieldList::decode(body)?;
        Ok(Transaction {
            r#type: self.r#type,
            trans: self.trans,
            flag: self.flag,
            fields,
        })
    }
}
