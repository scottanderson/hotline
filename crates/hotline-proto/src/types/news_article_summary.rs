//! [`NewsArticleSummary`]: one entry inside a `myField_NewsArtListData` (321) field.

use crate::error::{ProtoError, Result};
use crate::text_encoding::Encoding;
use crate::types::HlDate;
use crate::wire::{SliceReader, VecWriter, WithEncoding};

/// One article summary inside a [`myField_NewsArtListData`] (321) field.
///
/// [`myField_NewsArtListData`]: crate::FieldId::NewsArtListData
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsArticleSummary {
    /// Article ID.
    pub id: u32,
    /// Post date.
    pub date: HlDate,
    /// Parent article ID in the thread, `0` for a top-level article.
    pub parent_id: u32,
    /// Article flags, as set on post.
    pub flags: u32,
    /// Article title.
    pub title: Box<str>,
    /// Article poster name.
    pub poster: Box<str>,
    /// Every stored flavor of the article.
    pub flavors: Vec<NewsFlavor>,
}

/// One stored flavor of an article in a [`NewsArticleSummary`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsFlavor {
    /// The MIME type, such as `"text/plain"`.
    pub mime_type: Box<str>,
    /// The data size in bytes.
    pub size: u16,
}

impl NewsArticleSummary {
    /// Encode to the packed wire representation.
    pub(crate) fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let flavor_count = u16::try_from(self.flavors.len())
            .map_err(|_| ProtoError::Malformed("too many flavors".to_string()))?;
        let mut w = VecWriter::new()
            .u32(self.id)
            .bytes(&self.date.encode())
            .u32(self.parent_id)
            .u32(self.flags)
            .u16(flavor_count)
            .with_encoding(encoding)
            .pstring(&self.title)?
            .pstring(&self.poster)?;
        for flavor in &self.flavors {
            w = w.pstring(&flavor.mime_type)?.u16(flavor.size);
        }
        Ok(w.finish())
    }

    /// Read one summary from `r`.
    pub(crate) fn read(r: &mut SliceReader<'_, WithEncoding>) -> Result<Self> {
        let id = r.u32()?;
        let date = HlDate::decode(r.array()?);
        let parent_id = r.u32()?;
        let flags = r.u32()?;
        let flavor_count = r.u16()?;
        let title = r.pstring()?.into();
        let poster = r.pstring()?.into();
        let flavors = (0..flavor_count)
            .map(|_| {
                Ok(NewsFlavor {
                    mime_type: r.pstring()?.into(),
                    size: r.u16()?,
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self {
            id,
            date,
            parent_id,
            flags,
            title,
            poster,
            flavors,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_encoding;

    #[test]
    fn encode_writes_every_flavor() {
        let summary = NewsArticleSummary {
            id: 7,
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 0,
            },
            parent_id: 0,
            flags: 0,
            title: "Hi".into(),
            poster: "al".into(),
            flavors: vec![
                NewsFlavor {
                    mime_type: "text/plain".into(),
                    size: 5,
                },
                NewsFlavor {
                    mime_type: "image/gif".into(),
                    size: 300,
                },
            ],
        };
        let buf = summary.encode(text_encoding::DEFAULT).unwrap();
        // id(4) | date(8) | parent(4) | flags(4) | flavor count(2)
        assert_eq!(&buf[20..22], &[0, 2]);
        let tail: &[u8] = b"\x02Hi\x02al\x0atext/plain\x00\x05\x09image/gif\x01\x2c";
        assert_eq!(&buf[22..], tail);

        let mut r = SliceReader::new(&buf).with_encoding(text_encoding::DEFAULT);
        assert_eq!(NewsArticleSummary::read(&mut r).unwrap(), summary);
    }
}
