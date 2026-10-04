//! [`FieldList`]: the parsed body of a transaction, or a nested account
//! record inside [`myTran_GetUserList`] (348) / [`myTran_SetUserList`] (349).
//!
//! [`myTran_GetUserList`]: crate::Opcode::GetUserList
//! [`myTran_SetUserList`]: crate::Opcode::SetUserList

use crate::error::{ProtoError, Result};
use crate::field::id::FieldId;
use crate::field::{Built, Field, Received};
use crate::text_encoding::Encoding;
use crate::wire::{PacketReader, VecWriter};

/// A list of fields: a transaction's body, or the shape of one nested
/// account record inside [`myTran_GetUserList`] (348) /
/// [`myTran_SetUserList`] (349). `Deref`s to `[Field]` for anything not
/// covered by its own lookup methods below.
///
/// [`myTran_GetUserList`]: crate::Opcode::GetUserList
/// [`myTran_SetUserList`]: crate::Opcode::SetUserList
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldList<S = Built>(Vec<Field<S>>);

impl<S> Default for FieldList<S> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<S> FieldList<S> {
    pub(crate) const fn new(fields: Vec<Field<S>>) -> Self {
        Self(fields)
    }

    /// Byte length of the `count` prefix before a field list's own fields.
    const COUNT_LEN: usize = 2;

    /// Find the first field with the given ID, if present.
    pub fn field(&self, id: FieldId) -> Option<&Field<S>> {
        self.0.iter().find(|f| f.id == id)
    }

    /// Find all fields with the given ID.
    pub fn fields(&self, id: FieldId) -> impl Iterator<Item = &Field<S>> {
        self.0.iter().filter(move |f| f.id == id)
    }
}

impl FieldList {
    /// Encode as a `count`-prefixed field list: a transaction's own body
    /// shape, and also the shape of one nested account record inside
    /// [`myTran_GetUserList`] (348) / [`myTran_SetUserList`] (349).
    ///
    /// Fails if `fields.len()` or any field's data exceeds a `u16`
    /// (65535), reachable with attacker-controlled input.
    ///
    /// [`myTran_GetUserList`]: crate::Opcode::GetUserList
    /// [`myTran_SetUserList`]: crate::Opcode::SetUserList
    pub fn encode(fields: &[Field], encoding: &'static Encoding) -> Result<Vec<u8>> {
        let count = fields.len();
        let count = u16::try_from(count).map_err(|_| ProtoError::TooLong {
            what: "field count",
            len: count,
        })?;

        let encoded: Vec<u8> = fields
            .iter()
            .map(|f| f.encode(encoding))
            .collect::<Result<Vec<_>>>()?
            .concat();

        Ok(VecWriter::with_capacity(2 + encoded.len())
            .u16(count)
            .bytes(&encoded)
            .finish())
    }

    /// This list as a peer reads it after [`FieldList::encode`] with `encoding`.
    pub fn received(&self, encoding: &'static Encoding) -> FieldList<Received> {
        FieldList(self.0.iter().map(|f| f.received(encoding)).collect())
    }
}

impl FieldList<Received> {
    /// Decode the `count`-prefixed field list [`FieldList::encode`] produces.
    pub fn decode(body: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let prefix: [u8; Self::COUNT_LEN] = body
            .get(..Self::COUNT_LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: Self::COUNT_LEN,
                available: body.len(),
            })?;
        let mut r = PacketReader::new(prefix);
        let count = r.u16();
        r.finish();

        let mut offset = Self::COUNT_LEN;
        let mut fields = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let (field, consumed) = Field::decode(&body[offset..], encoding)?;
            offset += consumed;
            fields.push(field);
        }
        Ok(Self::new(fields))
    }

    /// `id`'s field as text, if present.
    pub fn string(&self, id: FieldId) -> Option<String> {
        self.field(id).map(Field::as_string)
    }

    /// `id`'s field as text plus its byte length, if present.
    /// See [`Field::as_string_len`].
    pub fn string_len(&self, id: FieldId) -> Option<(String, usize)> {
        self.field(id).map(Field::as_string_len)
    }
}

impl<S> std::ops::Deref for FieldList<S> {
    type Target = [Field<S>];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
