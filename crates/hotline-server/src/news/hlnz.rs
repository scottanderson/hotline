//! Reads an openline news category file: an `HLNZ` version 2 database.

use std::collections::{BTreeMap, HashMap, HashSet};

use hotline_proto::text_encoding::{self, Encoding};
use hotline_proto::types::HlDate;
use hotline_proto::wire::{PacketReader, SliceReader};
use uuid::Uuid;

use crate::news::category::MAX_REPLY_CHAIN_LEN;
use crate::news::{body_from_wire, NewsArticle, NewsCategory};

/// The file's type code and first four bytes.
pub const HLNZ_MAGIC: &[u8; 4] = b"HLNZ";

const VERSION: u32 = 2;

/// Bytes before block 1.
const HEADER_AREA: u64 = 512;

/// Most flavors read for one article.
const MAX_FLAVORS: usize = 32;

/// The file header fields that a read uses.
struct Header {
    guid: Uuid,
    block_size: u32,
    article_count: u32,
    first_article: u32,
    next_article_id: u32,
}

impl Header {
    pub const LEN: usize = 380;

    fn decode(buf: [u8; Self::LEN]) -> Result<Self, &'static str> {
        let mut r = PacketReader::new(buf);
        if &r.take::<4>() != HLNZ_MAGIC {
            return Err("not an HLNZ news file");
        }
        if r.u32() != VERSION {
            return Err("unsupported HLNZ version");
        }
        let guid = Uuid::from_bytes(r.take());
        let _add_serial = r.u32();
        let _del_serial = r.u32();
        let block_size = r.u32();
        let _list = r.take::<8>();
        let article_count = r.u32();
        let first_article = r.u32();
        let _last_article = r.u32();
        let next_article_id = r.u32();
        let _alloc_table = r.take::<12>();
        let _id_hash = r.take::<8>();
        let _reserved = r.take::<12>();
        let _name = r.take::<32>();
        let _description = r.take::<256>();
        r.finish();
        if block_size == 0 {
            return Err("HLNZ block size is 0");
        }
        Ok(Self {
            guid,
            block_size,
            article_count,
            first_article,
            next_article_id,
        })
    }
}

/// The article block fields that a read uses.
struct ItemBlock {
    first_flavor: u32,
    id: u32,
    next_article: u32,
    first_child: u32,
    date: HlDate,
    flags: u32,
    title: [u8; 64],
    poster: [u8; 32],
}

impl ItemBlock {
    pub const LEN: usize = 228;

    fn decode(buf: [u8; Self::LEN]) -> Self {
        let mut r = PacketReader::new(buf);
        let first_flavor = r.u32();
        let _size = r.u32();
        let _type = r.take::<28>();
        let id = r.u32();
        let _prev_article = r.u32();
        let next_article = r.u32();
        let _parent = r.u32();
        let first_child = r.u32();
        let date = HlDate::decode(&r.take::<{ HlDate::LEN }>()).expect("8 bytes");
        let flags = r.u32();
        let title = r.take();
        let poster = r.take();
        let _extern_id = r.take::<64>();
        r.finish();
        Self {
            first_flavor,
            id,
            next_article,
            first_child,
            date,
            flags,
            title,
            poster,
        }
    }
}

/// A walk over one file's article tree.
struct Reader<'a> {
    file: &'a [u8],
    block_size: u32,
    encoding: &'static Encoding,
    remaining: u32,
    visited_blocks: HashSet<u32>,
    article_ids: HashSet<u32>,
    articles: HashMap<u32, NewsArticle>,
}

impl Reader<'_> {
    /// The bytes from block `offset` to the end of the file.
    fn block(&self, offset: u32) -> Result<&[u8], &'static str> {
        let start = u64::from(offset)
            .checked_sub(1)
            .map(|index| HEADER_AREA + index * u64::from(self.block_size))
            .ok_or("HLNZ block offset is 0")?;
        usize::try_from(start)
            .ok()
            .and_then(|start| self.file.get(start..))
            .ok_or("HLNZ block is past the end of the file")
    }

    /// Text from a fixed-size pstring field.
    fn pstring(&self, field: &[u8]) -> Box<str> {
        let len = usize::from(field[0]).min(field.len() - 1);
        text_encoding::decode(self.encoding, &field[1..=len]).into()
    }

    /// Reads the sibling list at block `offset` and its replies. Returns
    /// the siblings' IDs.
    fn siblings(
        &mut self,
        mut offset: u32,
        parent_id: u32,
        depth: usize,
    ) -> Result<Vec<u32>, &'static str> {
        let mut ids = Vec::new();
        while offset != 0 && self.remaining > 0 {
            if depth >= MAX_REPLY_CHAIN_LEN {
                return Err("HLNZ reply chain is too deep");
            }
            if !self.visited_blocks.insert(offset) {
                return Err("HLNZ article list has a loop");
            }
            self.remaining -= 1;
            let item = ItemBlock::decode(
                self.block(offset)?
                    .first_chunk()
                    .copied()
                    .ok_or("HLNZ article is cut short")?,
            );
            if item.id == 0 {
                return Err("HLNZ article id is 0");
            }
            if !self.article_ids.insert(item.id) {
                return Err("duplicate article id in HLNZ file");
            }
            let data = self.flavors(item.first_flavor);
            let children = self.siblings(item.first_child, item.id, depth + 1)?;
            self.articles.insert(
                item.id,
                NewsArticle {
                    parent_id,
                    flags: item.flags,
                    date: item.date,
                    title: self.pstring(&item.title),
                    poster: self.pstring(&item.poster),
                    data,
                    children,
                },
            );
            ids.push(item.id);
            offset = item.next_article;
        }
        Ok(ids)
    }

    /// The flavor chain at `offset`, keyed by MIME type. The chain ends at
    /// the first flavor that does not read.
    fn flavors(&self, mut offset: u32) -> BTreeMap<String, Vec<u8>> {
        let mut data = BTreeMap::new();
        for _ in 0..MAX_FLAVORS {
            let Some((next, mime_type, body)) = self.flavor(offset) else {
                break;
            };
            data.entry(mime_type).or_insert(body);
            offset = next;
        }
        if data.is_empty() {
            data.insert("text/plain".to_string(), Vec::new());
        }
        data
    }

    /// The flavor at block `offset`: the next flavor's offset, the MIME
    /// type, and the body as stored.
    fn flavor(&self, offset: u32) -> Option<(u32, String, Vec<u8>)> {
        let mut r = SliceReader::new(self.block(offset).ok()?);
        let next = r.u32().ok()?;
        let size = r.u32().ok()?;
        let type_len = usize::from(r.u8().ok()?);
        let padded_type = r.bytes((type_len + 1).next_multiple_of(4) - 1).ok()?;
        let mime_type = String::from_utf8_lossy(&padded_type[..type_len]).into_owned();
        let body = r.bytes(size as usize).ok()?.to_vec();
        let body = body_from_wire(&mime_type, body, self.encoding);
        Some((next, mime_type, body))
    }
}

impl NewsCategory {
    /// The category stored in an openline `HLNZ` file, with its text in
    /// `encoding`. A text flavor is stored as UTF-8. An article with no
    /// flavor gets an empty `text/plain` body.
    pub fn from_hlnz(file: &[u8], encoding: &'static Encoding) -> Result<Self, &'static str> {
        let header = Header::decode(
            file.first_chunk()
                .copied()
                .ok_or("HLNZ header is cut short")?,
        )?;
        let mut reader = Reader {
            file,
            block_size: header.block_size,
            encoding,
            remaining: header.article_count,
            visited_blocks: HashSet::new(),
            article_ids: HashSet::new(),
            articles: HashMap::new(),
        };
        let roots = reader.siblings(header.first_article, 0, 0)?;
        let next_article_id = reader
            .articles
            .keys()
            .max()
            .map_or(1, |&id| id.saturating_add(1))
            .max(header.next_article_id);
        Ok(Self::from_articles(
            header.guid,
            next_article_id,
            reader.articles,
            roots,
        ))
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::wire::{PacketWriter, VecWriter};

    use super::*;

    const BLOCK: usize = 256;
    const NEXT_ARTICLE_AT: usize = 44;
    const FIRST_CHILD_AT: usize = 52;
    const TITLE_AT: usize = 68;

    /// One article to lay out in a test file.
    struct Art {
        id: u32,
        title: &'static [u8],
        flavors: Vec<(&'static str, &'static [u8])>,
        children: Vec<Art>,
    }

    fn art(id: u32, title: &'static [u8], body: &'static [u8], children: Vec<Art>) -> Art {
        Art {
            id,
            title,
            flavors: vec![("text/plain", body)],
            children,
        }
    }

    fn pfield<const N: usize>(text: &[u8]) -> [u8; N] {
        let mut out = [0; N];
        out[0] = text.len() as u8;
        out[1..=text.len()].copy_from_slice(text);
        out
    }

    /// An `HLNZ` file under construction.
    struct Image {
        blocks: Vec<u8>,
    }

    impl Image {
        /// Appends `bytes` at a block boundary. Returns its block offset.
        fn put(&mut self, bytes: &[u8]) -> u32 {
            let offset = (self.blocks.len() / BLOCK + 1) as u32;
            self.blocks.extend_from_slice(bytes);
            self.blocks
                .resize(self.blocks.len().next_multiple_of(BLOCK), 0);
            offset
        }

        fn patch(&mut self, offset: u32, at: usize, value: u32) {
            let start = (offset as usize - 1) * BLOCK + at;
            self.blocks[start..start + 4].copy_from_slice(&value.to_be_bytes());
        }

        fn siblings(&mut self, arts: &[Art]) -> u32 {
            let offsets: Vec<u32> = arts.iter().map(|a| self.article(a)).collect();
            for pair in offsets.windows(2) {
                self.patch(pair[0], NEXT_ARTICLE_AT, pair[1]);
            }
            offsets.first().copied().unwrap_or(0)
        }

        fn article(&mut self, art: &Art) -> u32 {
            let item = PacketWriter::<{ ItemBlock::LEN }>::new()
                .u32(0)
                .u32(0)
                .bytes(b"\x18application/x-hlnewsitem\0\0\0")
                .u32(art.id)
                .u32(0)
                .u32(0)
                .u32(0)
                .u32(0)
                .bytes(&date().encode())
                .u32(art.id * 10)
                .bytes(&pfield::<64>(art.title))
                .bytes(&pfield::<32>(b"bob"))
                .bytes(&[0; 64])
                .finish();
            let offset = self.put(&item);
            let mut last = offset;
            for (mime_type, body) in &art.flavors {
                let flavor = VecWriter::new()
                    .u32(0)
                    .u32(body.len() as u32)
                    .u8(mime_type.len() as u8)
                    .bytes(mime_type.as_bytes())
                    .bytes(
                        &[0; 3][..(mime_type.len() + 1).next_multiple_of(4) - mime_type.len() - 1],
                    )
                    .bytes(body)
                    .finish();
                let flavor = self.put(&flavor);
                self.patch(last, 0, flavor);
                last = flavor;
            }
            let first_child = self.siblings(&art.children);
            self.patch(offset, FIRST_CHILD_AT, first_child);
            offset
        }
    }

    fn date() -> HlDate {
        HlDate {
            year: 2001,
            milliseconds: 0,
            seconds: 3600,
        }
    }

    fn header(count: u32, first: u32, next_id: u32) -> [u8; Header::LEN] {
        PacketWriter::new()
            .bytes(HLNZ_MAGIC)
            .u32(VERSION)
            .bytes(&[7; 16])
            .u32(0)
            .u32(0)
            .u32(BLOCK as u32)
            .bytes(&[0; 8])
            .u32(count)
            .u32(first)
            .u32(0)
            .u32(next_id)
            .bytes(&[0; 12 + 8 + 12 + 32 + 256])
            .finish()
    }

    /// The bytes of a file that holds `roots`.
    fn file(roots: &[Art], count: u32, next_id: u32) -> Vec<u8> {
        let mut image = Image { blocks: Vec::new() };
        let first = image.siblings(roots);
        let mut out = header(count, first, next_id).to_vec();
        out.resize(HEADER_AREA as usize, 0);
        out.extend(image.blocks);
        out
    }

    fn thread() -> Vec<Art> {
        vec![
            art(
                1,
                b"caf\x8e",
                b"na\x95ve",
                vec![art(3, b"re", b"reply", vec![])],
            ),
            art(2, b"second", b"two", vec![]),
        ]
    }

    #[test]
    fn from_hlnz_reads_a_thread() {
        let category =
            NewsCategory::from_hlnz(&file(&thread(), 3, 9), text_encoding::DEFAULT).unwrap();

        assert_eq!(category.guid, Uuid::from_bytes([7; 16]));
        assert_eq!(category.roots, [1, 2]);
        let first = &category.articles[&1];
        assert_eq!(&*first.title, "caf\u{e9}");
        assert_eq!(&*first.poster, "bob");
        assert_eq!(first.flags, 10);
        assert_eq!(first.date, date());
        assert_eq!(first.data["text/plain"], "na\u{ef}ve".as_bytes());
        assert_eq!(first.children, [3]);
        assert_eq!(category.articles[&3].parent_id, 1);
        assert_eq!(category.articles[&2].parent_id, 0);
        assert_eq!(category.add_sn, 9);
    }

    #[test]
    fn from_hlnz_keeps_every_flavor() {
        let roots = [Art {
            id: 1,
            title: b"pic",
            flavors: vec![("text/plain", b"see"), ("image/gif", b"\x8e\x00")],
            children: vec![],
        }];

        let category =
            NewsCategory::from_hlnz(&file(&roots, 1, 2), text_encoding::DEFAULT).unwrap();

        let data = &category.articles[&1].data;
        assert_eq!(data["text/plain"], b"see");
        assert_eq!(data["image/gif"], b"\x8e\x00");
    }

    #[test]
    fn from_hlnz_gives_an_article_with_no_flavor_an_empty_body() {
        let roots = [Art {
            id: 1,
            title: b"empty",
            flavors: vec![],
            children: vec![],
        }];

        let category =
            NewsCategory::from_hlnz(&file(&roots, 1, 2), text_encoding::DEFAULT).unwrap();

        assert_eq!(category.articles[&1].data["text/plain"], b"");
    }

    #[test]
    fn from_hlnz_keeps_an_article_whose_flavor_is_cut_short() {
        let roots = [Art {
            id: 1,
            title: b"cut",
            flavors: vec![("text/plain", b"body")],
            children: vec![],
        }];
        let bytes = file(&roots, 1, 2);

        let category =
            NewsCategory::from_hlnz(&bytes[..bytes.len() - BLOCK + 8], text_encoding::DEFAULT)
                .unwrap();

        assert_eq!(&*category.articles[&1].title, "cut");
        assert_eq!(category.articles[&1].data["text/plain"], b"");
    }

    #[test]
    fn from_hlnz_cuts_a_title_to_63_bytes_and_a_poster_to_31() {
        let mut bytes = file(&[art(1, &[b'x'; 63], b"", vec![])], 1, 2);
        bytes[HEADER_AREA as usize + TITLE_AT] = 200;
        bytes[HEADER_AREA as usize + TITLE_AT + 64] = 200;

        let category = NewsCategory::from_hlnz(&bytes, text_encoding::DEFAULT).unwrap();

        assert_eq!(category.articles[&1].title.len(), 63);
        assert_eq!(category.articles[&1].poster.len(), 31);
    }

    #[test]
    fn from_hlnz_rejects_an_article_id_of_0() {
        assert_eq!(
            NewsCategory::from_hlnz(
                &file(&[art(0, b"a", b"a", vec![])], 1, 2),
                text_encoding::DEFAULT
            )
            .unwrap_err(),
            "HLNZ article id is 0"
        );
    }

    #[test]
    fn from_hlnz_reads_only_the_header_article_count() {
        let category =
            NewsCategory::from_hlnz(&file(&thread(), 2, 4), text_encoding::DEFAULT).unwrap();

        assert_eq!(category.roots, [1]);
        assert_eq!(category.articles[&1].children, [3]);
        assert!(!category.articles.contains_key(&2));
    }

    #[test]
    fn from_hlnz_sets_the_next_id_past_every_article() {
        let category =
            NewsCategory::from_hlnz(&file(&thread(), 3, 1), text_encoding::DEFAULT).unwrap();

        assert_eq!(category.add_sn, 4);
    }

    #[test]
    fn from_hlnz_rejects_another_file_type_or_version() {
        let mut bytes = file(&thread(), 3, 4);
        bytes[3] = b'X';
        assert!(NewsCategory::from_hlnz(&bytes, text_encoding::DEFAULT).is_err());

        let mut bytes = file(&thread(), 3, 4);
        bytes[7] = 3;
        assert!(NewsCategory::from_hlnz(&bytes, text_encoding::DEFAULT).is_err());
    }

    #[test]
    fn from_hlnz_rejects_a_loop_in_the_article_list() {
        let mut bytes = file(&[art(1, b"a", b"a", vec![])], 5, 2);
        bytes[HEADER_AREA as usize + NEXT_ARTICLE_AT + 3] = 1;

        assert_eq!(
            NewsCategory::from_hlnz(&bytes, text_encoding::DEFAULT).unwrap_err(),
            "HLNZ article list has a loop"
        );
    }

    #[test]
    fn from_hlnz_rejects_a_child_with_its_parents_id() {
        let roots = [art(1, b"a", b"a", vec![art(1, b"b", b"b", vec![])])];

        assert_eq!(
            NewsCategory::from_hlnz(&file(&roots, 2, 2), text_encoding::DEFAULT).unwrap_err(),
            "duplicate article id in HLNZ file"
        );
    }

    #[test]
    fn from_hlnz_rejects_a_cut_short_file() {
        let bytes = file(&thread(), 3, 4);

        assert!(NewsCategory::from_hlnz(&bytes[..600], text_encoding::DEFAULT).is_err());
        assert!(NewsCategory::from_hlnz(&bytes[..100], text_encoding::DEFAULT).is_err());
    }
}
