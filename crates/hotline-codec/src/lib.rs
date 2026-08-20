//! [`TransactionCodec`]: a [`tokio_util::codec::Decoder`]/[`Encoder`] for
//! [`Transaction`], used via `tokio_util::codec::Framed` instead of a
//! hand-rolled read-then-parse loop over a raw socket.
//!
//! `Decoder::decode` is called with whatever bytes have arrived so far
//! and returns `Ok(None)` to ask for more, so `Framed` handles TCP
//! fragmentation without relying on a single read returning a complete
//! frame. Callers get a `Stream<Item = Result<Transaction, _>>` +
//! `Sink<Transaction>`, used as `.next().await` / `.send(tx).await`.

use bytes::{Buf, BytesMut};
use hotline_proto::text_encoding::{Encoding, EncodingHandle};
use hotline_proto::{ProtoError, Transaction, TransactionHeader};
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
    /// Prefix decoded; waiting for `header.remaining_len()` more bytes to
    /// finish it.
    Body(TransactionHeader),
}

/// A [`tokio_util::codec::Decoder`]/[`Encoder`] for [`Transaction`].
/// `decode_prefix` is set once at construction; see
/// [`TransactionCodec::for_client`]/[`for_server`].
#[derive(Debug)]
pub struct TransactionCodec {
    state: DecodeState,
    decode_prefix: fn(&[u8; TransactionHeader::LEN]) -> TransactionHeader,
    /// Next `trans` to stamp on an outgoing push; `None` on a client codec.
    next_push_trans: Option<u32>,
    /// This connection's active text encoding, owned by whoever sets up the connection.
    encoding: EncodingHandle,
}

impl TransactionCodec {
    /// A codec for a client's connection: incoming bytes were sent by
    /// the server it's connected to (replies to this client's own
    /// requests, or unsolicited pushes).
    pub fn for_client(encoding: EncodingHandle) -> Self {
        Self {
            state: DecodeState::default(),
            decode_prefix: TransactionHeader::decode_from_server,
            next_push_trans: None,
            encoding,
        }
    }

    /// A codec for a server's connection: incoming bytes were sent by
    /// the client on the other end (requests).
    pub fn for_server(encoding: EncodingHandle) -> Self {
        Self {
            state: DecodeState::default(),
            decode_prefix: TransactionHeader::decode_from_client,
            next_push_trans: Some(1),
            encoding,
        }
    }

    /// This codec's active text encoding.
    fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }
}

impl Decoder for TransactionCodec {
    type Item = Transaction;
    type Error = CodecError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        loop {
            match &self.state {
                DecodeState::Prefix => {
                    if src.len() < TransactionHeader::LEN {
                        return Ok(None);
                    }
                    let prefix: [u8; TransactionHeader::LEN] = src[..TransactionHeader::LEN]
                        .try_into()
                        .expect("slice length checked above");
                    src.advance(TransactionHeader::LEN);
                    self.state = DecodeState::Body((self.decode_prefix)(&prefix));
                    // loop: the body may already be in this segment too
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

    fn encode(&mut self, item: Transaction, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let next = self.next_push_trans.as_mut();
        // No-op for a request or reply; stamps a push's trans.
        let item = item.with_push_trans(next);
        let bytes = item.encode(self.encoding())?;
        dst.extend_from_slice(&bytes);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::field_id::ClientFieldId;
    use hotline_proto::opcode::{ClientOpcode, ServerOpcode};
    use hotline_proto::Field;

    #[test]
    fn server_codec_stamps_each_push_with_an_increasing_trans() {
        let push = Transaction::push(ServerOpcode::Chat, vec![]);

        let mut buf = BytesMut::new();
        let mut server = TransactionCodec::for_server(EncodingHandle::default());
        server.encode(push.clone(), &mut buf).unwrap();
        server.encode(push, &mut buf).unwrap();

        let mut client = TransactionCodec::for_client(EncodingHandle::default());
        let first = client.decode(&mut buf).unwrap().unwrap();
        let second = client.decode(&mut buf).unwrap().unwrap();
        assert_eq!(first.trans(), 1);
        assert_eq!(second.trans(), 2);
    }

    #[test]
    fn changing_the_encoding_handle_affects_the_very_next_decode_and_encode() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let sjis = hotline_proto::Encoding::for_label(b"shift-jis").unwrap();
        let konnichiwa = "\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"; // "こんにちは"

        let handle = EncodingHandle::default();
        let mut server = TransactionCodec::for_server(handle.clone());

        // Encoded and decoded as Mac OS Roman by default: lossy.
        let mut buf = BytesMut::new();
        let tx = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, konnichiwa)],
        );
        server.encode(tx, &mut buf).unwrap();
        let decoded = server.decode(&mut buf).unwrap().unwrap();
        assert_ne!(
            decoded
                .field(ClientFieldId::Text)
                .unwrap()
                .as_string(encoding),
            konnichiwa
        );

        // Affects both directions on this codec from the next call on.
        handle.set(sjis);
        let mut buf = BytesMut::new();
        let tx = Transaction::request(
            ClientOpcode::Chat,
            2,
            vec![Field::string(ClientFieldId::Text, konnichiwa)],
        );
        server.encode(tx, &mut buf).unwrap();
        let decoded = server.decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            decoded.field(ClientFieldId::Text).unwrap().as_string(sjis),
            konnichiwa
        );
    }

    #[test]
    fn round_trips_a_whole_transaction_in_one_go() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let tx = Transaction::request(
            ClientOpcode::Chat,
            1,
            vec![Field::string(ClientFieldId::Text, "hello")],
        );

        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        codec.encode(tx.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        // decode produces FieldValue::Bytes, so compare interpreted values
        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded
                .field(ClientFieldId::Text)
                .unwrap()
                .as_string(encoding),
            "hello"
        );
        assert!(buf.is_empty(), "decode should consume exactly one frame");
    }

    /// Feeds bytes one at a time to verify `decode` correctly reports
    /// "not enough yet" until the full frame is buffered.
    fn feed_byte_by_byte(tx: &Transaction) -> Transaction {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let encoded = tx.encode(encoding).unwrap();
        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());

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
        let encoding = hotline_proto::text_encoding::DEFAULT;
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
        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded
                .field(ClientFieldId::Name)
                .unwrap()
                .as_string(encoding),
            "RustHL"
        );
        assert_eq!(
            decoded
                .field(ClientFieldId::Text)
                .unwrap()
                .as_string(encoding),
            "a slightly longer field to cross a few byte boundaries"
        );
    }

    #[test]
    fn decodes_two_back_to_back_transactions_from_one_buffer() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let first = Transaction::request(ClientOpcode::Ping, 1, vec![]);
        let second = Transaction::request(
            ClientOpcode::Chat,
            2,
            vec![Field::string(ClientFieldId::Text, "second frame")],
        );

        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        codec.encode(first.clone(), &mut buf).unwrap();
        codec.encode(second.clone(), &mut buf).unwrap();

        let decoded_first = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_first.trans(), first.trans());

        let decoded_second = codec.decode(&mut buf).unwrap().unwrap();
        assert_eq!(decoded_second.trans(), second.trans());
        assert_eq!(
            decoded_second
                .field(ClientFieldId::Text)
                .unwrap()
                .as_string(encoding),
            "second frame"
        );
        assert!(buf.is_empty());
    }
}
