//! [`TransactionCodec`]: a [`tokio_util::codec::Decoder`]/[`Encoder`] for
//! [`Transaction`], used via `tokio_util::codec::Framed` instead of a
//! hand-rolled read-then-parse loop over a raw socket.
//!
//! `Decoder::decode` is called with whatever bytes have arrived so far
//! and returns `Ok(None)` to ask for more, so `Framed` handles TCP
//! fragmentation without relying on a single read returning a complete
//! frame. Callers get a `Stream<Item = Result<Transaction, _>>` +
//! `Sink<Transaction>` - `.next().await` / `.send(tx).await`.

use bytes::{Buf, BytesMut};
use hotline_proto::{ProtoError, Transaction, PREFIX_LEN};
use tokio_util::codec::{Decoder, Encoder};

/// Framing errors: either the transport failed (`io::Error`, required by
/// `Decoder`/`Encoder`'s `Error: From<io::Error>` bound) or the bytes
/// that arrived weren't a valid `Transaction`.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The underlying transport failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes read were not a valid transaction.
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),
}

/// What [`TransactionCodec`] is waiting for next.
#[derive(Debug, Default)]
enum DecodeState {
    /// Waiting for the 20-byte fixed prefix.
    #[default]
    Prefix,
    /// Prefix decoded (fields still empty); waiting for
    /// `partial.remaining_len()` more bytes to finish it.
    Body(Transaction),
}

/// A [`tokio_util::codec::Decoder`]/[`Encoder`] for [`Transaction`].
/// Stateful across `decode` calls (see [`DecodeState`]) since a
/// transaction's total size isn't known until its prefix has been read,
/// and `Framed` may call `decode` again before a full transaction has
/// arrived.
#[derive(Debug, Default)]
pub struct TransactionCodec {
    state: DecodeState,
}

impl Decoder for TransactionCodec {
    type Item = Transaction;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Transaction>, CodecError> {
        loop {
            match &self.state {
                DecodeState::Prefix => {
                    if src.len() < PREFIX_LEN {
                        return Ok(None);
                    }
                    let prefix: [u8; PREFIX_LEN] = src[..PREFIX_LEN]
                        .try_into()
                        .expect("slice length checked above");
                    src.advance(PREFIX_LEN);
                    self.state = DecodeState::Body(Transaction::decode_prefix(&prefix));
                    // A small transaction can arrive in the same segment
                    // as its prefix, so loop instead of waiting for
                    // another decode() call.
                }
                DecodeState::Body(partial) => {
                    let need = partial.remaining_len() as usize;
                    if src.len() < need {
                        return Ok(None);
                    }
                    let body = src.split_to(need);
                    let DecodeState::Body(partial) = std::mem::take(&mut self.state) else {
                        unreachable!("just matched Body above")
                    };
                    let tx = partial.finish_decode(&body)?;
                    return Ok(Some(tx));
                }
            }
        }
    }
}

impl Encoder<Transaction> for TransactionCodec {
    type Error = CodecError;

    fn encode(&mut self, item: Transaction, dst: &mut BytesMut) -> Result<(), CodecError> {
        let bytes = item.encode()?;
        dst.extend_from_slice(&bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::field_id::ClientFieldId;
    use hotline_proto::opcode::ClientOpcode;
    use hotline_proto::Field;

    #[test]
    fn round_trips_a_whole_transaction_in_one_go() {
        let tx = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hello")],
        );

        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::default();
        codec.encode(tx.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        // Decode always produces `FieldValue::Bytes` (no schema to
        // interpret against), so compare interpreted values rather than
        // raw struct equality against the locally-built, typed original.
        assert_eq!(decoded.trans, tx.trans);
        assert_eq!(
            decoded.field(ClientFieldId::Text).unwrap().as_string(),
            "hello"
        );
        assert!(buf.is_empty(), "decode should consume exactly one frame");
    }

    /// Feeds bytes one at a time to verify `decode` correctly reports
    /// "not enough yet" until the full frame is buffered.
    fn feed_byte_by_byte(tx: &Transaction) -> Transaction {
        let encoded = tx.encode().unwrap();
        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::default();

        for &byte in &encoded[..encoded.len() - 1] {
            buf.extend_from_slice(&[byte]);
            assert!(
                codec.decode(&mut buf).unwrap().is_none(),
                "must not decode a partial frame"
            );
        }
        buf.extend_from_slice(&[*encoded.last().unwrap()]);
        codec
            .decode(&mut buf)
            .unwrap()
            .expect("full frame now buffered")
    }

    #[test]
    fn decodes_correctly_when_bytes_arrive_one_at_a_time() {
        let tx = Transaction::request(
            ClientOpcode::Login,
            7,
            vec![
                Field::string(ClientFieldId::Name, "RustHL"),
                Field::string(
                    ClientFieldId::Text,
                    "a slightly longer field to cross a few byte boundaries",
                ),
            ],
        );
        let decoded = feed_byte_by_byte(&tx);
        assert_eq!(decoded.trans, tx.trans);
        assert_eq!(
            decoded.field(ClientFieldId::Name).unwrap().as_string(),
            "RustHL"
        );
        assert_eq!(
            decoded.field(ClientFieldId::Text).unwrap().as_string(),
            "a slightly longer field to cross a few byte boundaries"
        );
    }

    #[test]
    fn decodes_two_back_to_back_transactions_from_one_buffer() {
        let first = Transaction::request(ClientOpcode::Ping, 1, vec![]);
        let second = Transaction::request(
            ClientOpcode::Chat,
            2,
            vec![Field::string(ClientFieldId::Text, "second frame")],
        );

        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::default();
        codec.encode(first.clone(), &mut buf).unwrap();
        codec.encode(second.clone(), &mut buf).unwrap();

        let decoded_first = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_first.trans, first.trans);

        let decoded_second = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_second.trans, second.trans);
        assert_eq!(
            decoded_second
                .field(ClientFieldId::Text)
                .unwrap()
                .as_string(),
            "second frame"
        );
        assert!(buf.is_empty());
    }
}
