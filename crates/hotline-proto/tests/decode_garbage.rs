//! No decoder panics on any input.

#![allow(clippy::indexing_slicing)]

use hotline_proto::htxf::{decode_flat_file_prefix, FolderItemHeader};
use hotline_proto::text_encoding::DEFAULT;
use hotline_proto::types::{
    FileNameWithInfo, HlPath, NewsArticleListData, NewsCategoryListEntry, ResumeData,
    UserNameWithInfo,
};
use hotline_proto::{
    ClientHandshake, Field, FieldList, ForkHeader, HtxfHeader, ServerHandshake, Transaction,
};

/// A fixed xorshift generator, so a failure repeats.
struct Rng(u64);

impl Rng {
    const fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Random bytes, often after a magic that a decoder checks first.
    fn buffer(&mut self) -> Vec<u8> {
        const MAGICS: [&[u8]; 6] = [b"", b"RFLT\0\x01", b"FILP\0\x01", b"HTXF", b"TRTP", b"HTRK"];
        let magic = MAGICS[(self.next() % MAGICS.len() as u64) as usize];
        let len = (self.next() % 96) as usize;
        let mut buf = magic.to_vec();
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
        let _ = HlPath::decode(&buf, DEFAULT);
        let _ = FileNameWithInfo::decode(&buf, DEFAULT);
        let _ = UserNameWithInfo::decode(&buf, DEFAULT);
        let _ = NewsArticleListData::decode(&buf, DEFAULT);
        let _ = NewsCategoryListEntry::decode(&buf, DEFAULT);
        let _ = NewsCategoryListEntry::decode_old(&buf, DEFAULT);
        let _ = ResumeData::decode(&buf);
        let _ = decode_flat_file_prefix(&buf);
        let _ = FolderItemHeader::decode_body(&buf, DEFAULT);
        let _ = ForkHeader::decode(&buf);
        let _ = HtxfHeader::decode(&buf);
        let _ = ClientHandshake::decode(&buf);
        let _ = ServerHandshake::decode(&buf);
        let _ = Field::decode(&buf, DEFAULT);
        let _ = FieldList::decode(&buf, DEFAULT);
        let _ = Transaction::decode_from_client(&buf, DEFAULT);
        let _ = Transaction::decode_from_server(&buf, DEFAULT);
    }
}
