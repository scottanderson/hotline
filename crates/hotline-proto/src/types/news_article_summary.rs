//! [`NewsArticleSummary`]: one entry inside a `myField_NewsArtListData` (321) field.

use crate::error::Result;
use crate::text_encoding::Encoding;
use crate::types::HlDate;
use crate::wire::VecWriter;

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
    pub title: String,
    /// Article poster name.
    pub poster: String,
    /// This article's stored MIME type, e.g. `"text/plain"`.
    pub flavor: String,
    /// This article's data size in bytes for `flavor`.
    pub flavor_size: u16,
}

impl NewsArticleSummary {
    /// Encode to the packed wire representation; only `title`/`poster`/
    /// `flavor` are text.
    pub(crate) fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(VecWriter::new()
            .u32(self.id)
            .bytes(&self.date.encode())
            .u32(self.parent_id)
            .u32(self.flags)
            .u16(1) // flavor count: this server stores one flavor per article
            .with_encoding(encoding)
            .pstring(&self.title)?
            .pstring(&self.poster)?
            .pstring(&self.flavor)?
            .u16(self.flavor_size)
            .finish())
    }
}
