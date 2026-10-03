//! The codec returns an error, and does not panic, on any byte stream.

use bytes::BytesMut;
use hotline_codec::TransactionCodec;
use hotline_proto::text_encoding::EncodingHandle;
use tokio_util::codec::Decoder;

/// A fixed xorshift generator, so a failure repeats.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    const fn byte(&mut self) -> u8 {
        match self.next() % 4 {
            0 => 0,
            1 => 0xFF,
            _ => self.next() as u8,
        }
    }

    /// A transaction header, often with small lengths, then a random body.
    fn chunk(&mut self) -> Vec<u8> {
        let mut buf: Vec<u8> = (0..12).map(|_| self.byte()).collect();
        if self.next().is_multiple_of(4) {
            buf.extend((0..8).map(|_| self.byte()));
        } else {
            let total = (self.next() % 64) as u32;
            let part = if self.next().is_multiple_of(2) {
                total
            } else {
                (self.next() % 64) as u32
            };
            buf.extend(total.to_be_bytes());
            buf.extend(part.to_be_bytes());
        }
        let len = (self.next() % 64) as usize;
        buf.extend((0..len).map(|_| self.byte()));
        buf
    }
}

fn feed(new_codec: fn(EncodingHandle) -> TransactionCodec) {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut codec = new_codec(EncodingHandle::default());
    let mut src = BytesMut::new();
    for _ in 0..100_000 {
        src.extend_from_slice(&rng.chunk());
        loop {
            match codec.decode(&mut src) {
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => {
                    codec = new_codec(EncodingHandle::default());
                    src.clear();
                    break;
                }
            }
        }
    }
}

#[test]
fn server_codec_rejects_garbage_without_panicking() {
    feed(TransactionCodec::for_server);
}

#[test]
fn client_codec_rejects_garbage_without_panicking() {
    feed(TransactionCodec::for_client);
}
