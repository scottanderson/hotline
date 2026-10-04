mod decode_state;
mod error;
mod partial;

use std::collections::HashMap;

use bytes::{Buf, BytesMut};
use hotline_proto::text_encoding::{Encoding, EncodingHandle};
use hotline_proto::{Received, Transaction, TransactionHeader};
use tokio_util::codec::{Decoder, Encoder};

use decode_state::DecodeState;
pub use error::CodecError;
use partial::Partial;

/// Cap on a transaction's declared total size (`len`) and on each part's
/// declared size (`len2`), on a server's connection. Matches openline's
/// own server override of `_gTransactMaxReceiveSize` (`HotlineServ.cp`).
const SERVER_MAX_TRANSACTION_LEN: u32 = 512 * 1024;

/// The same cap on a client's connection. Matches openline's
/// transport default `_gTransactMaxReceiveSize` (`UTransact.cp`).
const CLIENT_MAX_TRANSACTION_LEN: u32 = 2 * 1024 * 1024;

/// Cap on the bytes one connection may hold in reassembly. A decoder-side
/// safety bound; openline has none.
const MAX_PENDING_BYTES: usize = 64 * 1024 * 1024;

/// Partial ids that fit in [`MAX_PENDING_BYTES`]. Each holds under twice the size cap.
const fn max_pending_partials(max_transaction_len: u32) -> usize {
    MAX_PENDING_BYTES / (2 * max_transaction_len as usize)
}

/// A [`tokio_util::codec::Decoder`]/[`tokio_util::codec::Encoder`] for [`Transaction`].
/// `decode_prefix` is set once at construction; see
/// [`TransactionCodec::for_client`]/[`for_server`](TransactionCodec::for_server).
#[derive(Debug)]
pub struct TransactionCodec {
    state: DecodeState,
    decode_prefix: fn(&[u8; TransactionHeader::LEN]) -> TransactionHeader,
    /// Next `trans` to stamp on an outgoing push; `None` on a client codec.
    next_push_trans: Option<u32>,
    /// This connection's active text encoding, owned by whoever sets up the connection.
    encoding: EncodingHandle,
    /// Split transactions awaiting their remaining parts, keyed by
    /// `(is_reply, trans)`.
    partials: HashMap<(bool, u32), Partial>,
    /// Cap on an incoming `len`/`len2`; the two ends use different values.
    max_transaction_len: u32,
    /// Cap on `partials`' size, derived from `max_transaction_len`.
    max_pending_partials: usize,
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
            partials: HashMap::new(),
            max_transaction_len: CLIENT_MAX_TRANSACTION_LEN,
            max_pending_partials: max_pending_partials(CLIENT_MAX_TRANSACTION_LEN),
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
            partials: HashMap::new(),
            max_transaction_len: SERVER_MAX_TRANSACTION_LEN,
            max_pending_partials: max_pending_partials(SERVER_MAX_TRANSACTION_LEN),
        }
    }

    /// This codec's active text encoding.
    fn encoding(&self) -> &'static Encoding {
        self.encoding.get()
    }
}

impl Decoder for TransactionCodec {
    type Item = Transaction<Received>;
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
                    let header = (self.decode_prefix)(&prefix);
                    let sizes = [header.total_len(), header.data_len()];
                    if sizes.contains(&0) {
                        return Err(CodecError::ZeroLength);
                    }
                    let max = self.max_transaction_len;
                    if let Some(len) = sizes.into_iter().find(|&l| l > max) {
                        return Err(CodecError::TooLarge { len, max });
                    }
                    self.state = DecodeState::Body(header);
                    // loop: the body may already be in this segment too
                }
                DecodeState::Body(header) => {
                    let need = header.data_len() as usize;
                    if src.len() < need {
                        return Ok(None);
                    }
                    let part_body = src.split_to(need);
                    let DecodeState::Body(header) = std::mem::take(&mut self.state) else {
                        unreachable!("just matched Body above")
                    };

                    let key = (header.is_reply(), header.trans());

                    // Common case: whole in one part, with no part pending under this id.
                    if header.total_len() == header.data_len() && !self.partials.contains_key(&key)
                    {
                        return Ok(Some(header.finish_decode(&part_body)?));
                    }

                    // Split transaction: accumulate the parts under one id.
                    let total_len = header.total_len() as usize;

                    if let Some(existing) = self.partials.get_mut(&key) {
                        existing.header = header;
                        existing.body.extend_from_slice(&part_body);
                    } else {
                        let max = self.max_pending_partials;
                        if self.partials.len() >= max {
                            return Err(CodecError::TooManyPendingPartials { max });
                        }
                        self.partials.insert(
                            key,
                            Partial {
                                header,
                                body: part_body.to_vec(),
                            },
                        );
                    }

                    let done = self
                        .partials
                        .get(&key)
                        .expect("just inserted or updated above")
                        .body
                        .len()
                        >= total_len;
                    if !done {
                        continue;
                    }
                    let Partial { header, body } =
                        self.partials.remove(&key).expect("present, just checked");
                    return Ok(Some(header.finish_decode(&body)?));
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
    use hotline_proto::opcode::{Opcode, TransactionType};
    use hotline_proto::Field;
    use hotline_proto::FieldId;

    #[test]
    fn server_codec_stamps_each_push_with_an_increasing_trans() {
        let push = Transaction::push(Opcode::ChatMsg, vec![]);

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
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, konnichiwa)],
        );
        server.encode(tx, &mut buf).unwrap();
        let decoded = server.decode(&mut buf).unwrap().unwrap();
        assert_ne!(
            decoded.field(FieldId::Data).unwrap().as_string(encoding),
            konnichiwa
        );

        // Affects both directions on this codec from the next call on.
        handle.set(sjis);
        let mut buf = BytesMut::new();
        let tx = Transaction::request(
            Opcode::ChatSend,
            2,
            vec![Field::string(FieldId::Data, konnichiwa)],
        );
        server.encode(tx, &mut buf).unwrap();
        let decoded = server.decode(&mut buf).unwrap().unwrap();
        assert_eq!(
            decoded.field(FieldId::Data).unwrap().as_string(sjis),
            konnichiwa
        );
    }

    #[test]
    fn round_trips_a_whole_transaction_in_one_go() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::ChatSend,
            1,
            vec![Field::string(FieldId::Data, "hello")],
        );

        let mut buf = BytesMut::new();
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        codec.encode(tx.clone(), &mut buf).unwrap();

        let decoded = codec.decode(&mut buf).unwrap().unwrap();
        // decode produces FieldValue::Bytes, so compare interpreted values
        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded.field(FieldId::Data).unwrap().as_string(encoding),
            "hello"
        );
        assert!(buf.is_empty(), "decode should consume exactly one frame");
    }

    /// Feeds bytes one at a time to verify `decode` correctly reports
    /// "not enough yet" until the full frame is buffered.
    fn feed_byte_by_byte(tx: &Transaction) -> Transaction<Received> {
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
            Opcode::Login,
            7,
            vec![
                Field::string(FieldId::UserName, "RustHL"),
                Field::string(
                    FieldId::Data,
                    "a slightly longer field to cross a few byte boundaries",
                ),
            ],
        );
        let decoded = feed_byte_by_byte(&tx);
        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded
                .field(FieldId::UserName)
                .unwrap()
                .as_string(encoding),
            "RustHL"
        );
        assert_eq!(
            decoded.field(FieldId::Data).unwrap().as_string(encoding),
            "a slightly longer field to cross a few byte boundaries"
        );
    }

    #[test]
    fn decodes_two_back_to_back_transactions_from_one_buffer() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let first = Transaction::request(Opcode::KeepConnectionAlive, 1, vec![]);
        let second = Transaction::request(
            Opcode::ChatSend,
            2,
            vec![Field::string(FieldId::Data, "second frame")],
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
                .field(FieldId::Data)
                .unwrap()
                .as_string(encoding),
            "second frame"
        );
        assert!(buf.is_empty());
    }

    /// Encodes one raw part of `tx`: a 20-byte prefix with `len` set to
    /// `total_len` and `len2` set to `part_body`'s size, then those bytes.
    fn encode_one_part(tx: &Transaction, total_len: u32, part_body: &[u8]) -> BytesMut {
        let mut buf = BytesMut::new();
        buf.extend_from_slice(&tx.r#type().as_u32().to_be_bytes());
        buf.extend_from_slice(&tx.trans().to_be_bytes());
        buf.extend_from_slice(&u32::from(tx.is_error()).to_be_bytes());
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.extend_from_slice(&(part_body.len() as u32).to_be_bytes());
        buf.extend_from_slice(part_body);
        buf
    }

    /// Encodes `tx` as raw wire bytes split into several parts (each with
    /// its own 20-byte prefix), `len` set to the whole body's size and
    /// `len2` set to that part's own chunk size, the way openline's
    /// transport splits a transaction across `SetSessionSendSize`.
    fn encode_as_parts(tx: &Transaction, chunk_sizes: &[usize]) -> BytesMut {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let whole = tx.encode(encoding).unwrap();
        let body = &whole[TransactionHeader::LEN..];
        assert_eq!(
            body.len(),
            chunk_sizes.iter().sum::<usize>(),
            "chunk_sizes must cover the whole body"
        );
        let total_len = body.len() as u32;

        let mut buf = BytesMut::new();
        let mut offset = 0;
        for &size in chunk_sizes {
            let part = encode_one_part(tx, total_len, &body[offset..offset + size]);
            buf.extend_from_slice(&part);
            offset += size;
        }
        buf
    }

    /// A bodyless 20-byte prefix with `len` and `len2` set literally, for
    /// the decoder's size guard.
    fn encode_prefix_with_sizes(len: u32, len2: u32) -> BytesMut {
        let mut buf = BytesMut::new();
        let type_u32 = TransactionType::Request(Opcode::Login).as_u32();
        buf.extend_from_slice(&type_u32.to_be_bytes());
        buf.extend_from_slice(&1u32.to_be_bytes()); // trans
        buf.extend_from_slice(&0u32.to_be_bytes()); // flag
        buf.extend_from_slice(&len.to_be_bytes());
        buf.extend_from_slice(&len2.to_be_bytes());
        buf
    }

    #[test]
    fn reassembles_a_transaction_split_across_two_parts() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::Login,
            7,
            vec![
                Field::string(FieldId::UserName, "RustHL"),
                Field::string(FieldId::Data, "a body long enough to split in two"),
            ],
        );
        let whole_body_len = tx.encode(encoding).unwrap().len() - TransactionHeader::LEN;
        let split_at = whole_body_len / 2;
        let both_parts = encode_as_parts(&tx, &[split_at, whole_body_len - split_at]);
        let first_part_len = TransactionHeader::LEN + split_at;

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let mut buf = BytesMut::from(&both_parts[..first_part_len]);
        assert!(
            codec.decode(&mut buf).unwrap().is_none(),
            "must not decode after only the first part"
        );
        buf.extend_from_slice(&both_parts[first_part_len..]);
        let decoded = codec
            .decode(&mut buf)
            .unwrap()
            .expect("second part completes the transaction");

        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded
                .field(FieldId::UserName)
                .unwrap()
                .as_string(encoding),
            "RustHL"
        );
        assert_eq!(
            decoded.field(FieldId::Data).unwrap().as_string(encoding),
            "a body long enough to split in two"
        );
        assert!(buf.is_empty());
    }

    #[test]
    fn reassembled_transaction_decodes_identically_to_the_same_transaction_sent_whole() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::Login,
            9,
            vec![Field::string(FieldId::Data, "split vs whole")],
        );
        let whole_body_len = tx.encode(encoding).unwrap().len() - TransactionHeader::LEN;

        let mut whole_buf = BytesMut::new();
        TransactionCodec::for_server(EncodingHandle::default())
            .encode(tx.clone(), &mut whole_buf)
            .unwrap();
        let from_whole = TransactionCodec::for_server(EncodingHandle::default())
            .decode(&mut whole_buf)
            .unwrap()
            .unwrap();

        let both_parts = encode_as_parts(&tx, &[1, whole_body_len - 1]);
        let first_part_len = TransactionHeader::LEN + 1;
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let mut split_buf = BytesMut::from(&both_parts[..first_part_len]);
        assert!(codec.decode(&mut split_buf).unwrap().is_none());
        split_buf.extend_from_slice(&both_parts[first_part_len..]);
        let from_split = codec.decode(&mut split_buf).unwrap().unwrap();

        // `Transaction` has no `Debug` impl, so compare field by field.
        assert_eq!(from_whole.trans(), from_split.trans());
        assert_eq!(from_whole.is_error(), from_split.is_error());
        assert_eq!(
            from_whole.field(FieldId::Data).unwrap().as_string(encoding),
            from_split.field(FieldId::Data).unwrap().as_string(encoding)
        );
    }

    #[test]
    fn rejects_a_declared_len_above_the_cap() {
        let oversized = SERVER_MAX_TRANSACTION_LEN + 1;
        let mut buf = encode_prefix_with_sizes(oversized, oversized);

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge { len, max }
            if len == oversized && max == SERVER_MAX_TRANSACTION_LEN));
    }

    #[test]
    fn reports_the_part_size_that_broke_the_cap() {
        let oversized = SERVER_MAX_TRANSACTION_LEN + 1;
        let mut buf = encode_prefix_with_sizes(16, oversized);

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge { len, .. } if len == oversized));
    }

    /// A client's cap is the transport default, not the server's own
    /// tighter override, so a reply over 512 KB must still decode.
    #[test]
    fn a_client_accepts_a_len_above_the_server_cap() {
        let big = SERVER_MAX_TRANSACTION_LEN + 1;
        let mut buf = encode_prefix_with_sizes(big, big);

        let mut codec = TransactionCodec::for_client(EncodingHandle::default());
        assert!(
            codec.decode(&mut buf).unwrap().is_none(),
            "body still pending"
        );

        let over = CLIENT_MAX_TRANSACTION_LEN + 1;
        let mut buf = encode_prefix_with_sizes(over, over);
        let mut codec = TransactionCodec::for_client(EncodingHandle::default());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::TooLarge { max, .. }
            if max == CLIENT_MAX_TRANSACTION_LEN));
    }

    #[test]
    fn rejects_a_zero_len_with_a_non_zero_len2() {
        let mut buf = encode_prefix_with_sizes(0, 16);

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::ZeroLength));
    }

    #[test]
    fn rejects_a_zero_len2() {
        let mut buf = encode_prefix_with_sizes(16, 0);

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let err = codec.decode(&mut buf).unwrap_err();
        assert!(matches!(err, CodecError::ZeroLength));
    }

    #[test]
    fn a_whole_transaction_reusing_a_pending_trans_appends_to_that_partial() {
        let encoding = hotline_proto::text_encoding::DEFAULT;
        let tx = Transaction::request(
            Opcode::Login,
            7,
            vec![Field::string(FieldId::Data, "a body long enough to split")],
        );
        let whole = tx.encode(encoding).unwrap();
        let body = &whole[TransactionHeader::LEN..];

        let mut codec = TransactionCodec::for_server(EncodingHandle::default());
        let mut buf = encode_one_part(&tx, body.len() as u32, &body[..1]);
        assert!(
            codec.decode(&mut buf).unwrap().is_none(),
            "first part alone must not complete the transaction"
        );

        // The rest of the body, sent as a whole transaction reusing that trans.
        let tail = &body[1..];
        let mut buf = encode_one_part(&tx, tail.len() as u32, tail);
        let decoded = codec
            .decode(&mut buf)
            .unwrap()
            .expect("appends to the pending partial and completes it");

        assert_eq!(decoded.trans(), tx.trans());
        assert_eq!(
            decoded.field(FieldId::Data).unwrap().as_string(encoding),
            "a body long enough to split"
        );
        assert!(codec.partials.is_empty(), "no partial may be stranded");
        assert!(buf.is_empty());
    }

    #[test]
    fn rejects_too_many_concurrently_pending_partial_transactions() {
        let mut codec = TransactionCodec::for_server(EncodingHandle::default());

        let max = max_pending_partials(SERVER_MAX_TRANSACTION_LEN);
        for trans in 0..max as u32 {
            let tx = Transaction::request(Opcode::Login, trans, vec![]);
            let body_len = tx
                .encode(hotline_proto::text_encoding::DEFAULT)
                .unwrap()
                .len()
                - TransactionHeader::LEN;
            // A single-byte first part, leaving the rest pending forever.
            let mut buf = encode_as_parts(&tx, &[1, body_len - 1]);
            let mut part1 = buf.split_to(TransactionHeader::LEN + 1);
            assert!(
                codec.decode(&mut part1).unwrap().is_none(),
                "first part alone must not complete the transaction"
            );
        }

        // One more distinct id, past the cap, must be rejected outright.
        let tx = Transaction::request(Opcode::Login, max as u32, vec![]);
        let body_len = tx
            .encode(hotline_proto::text_encoding::DEFAULT)
            .unwrap()
            .len()
            - TransactionHeader::LEN;
        let mut buf = encode_as_parts(&tx, &[1, body_len - 1]);
        let mut part1 = buf.split_to(TransactionHeader::LEN + 1);

        let err = codec.decode(&mut part1).unwrap_err();
        assert!(matches!(err, CodecError::TooManyPendingPartials { max: m } if m == max));
    }
}
