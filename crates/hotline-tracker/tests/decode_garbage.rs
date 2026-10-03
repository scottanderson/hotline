//! Every decoder returns an error, and does not panic, on any input.

use hotline_proto::text_encoding::DEFAULT;
use hotline_tracker::{decode_reply_header, decode_server_list, TrackedServer};

/// A fixed xorshift generator, so a failure repeats.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Random bytes, often after the `HTRK` magic.
    fn buffer(&mut self) -> Vec<u8> {
        let mut buf = if self.next().is_multiple_of(2) {
            b"HTRK\0\x01".to_vec()
        } else {
            Vec::new()
        };
        let len = (self.next() % 96) as usize;
        buf.extend((0..len).map(|_| match self.next() % 4 {
            0 => 0,
            1 => 0xFF,
            _ => self.next() as u8,
        }));
        buf
    }
}

#[test]
fn decoders_reject_garbage_without_panicking() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..200_000 {
        let buf = rng.buffer();
        let _ = decode_reply_header(&buf);
        let _ = decode_server_list(&buf, DEFAULT);
        let _ = TrackedServer::decode(&buf, DEFAULT);
    }
}
