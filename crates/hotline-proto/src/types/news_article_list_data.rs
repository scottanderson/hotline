//! [`NewsArticleListData`]: the whole `myField_NewsArtListData` (321) field.

use crate::error::Result;
use crate::text_encoding::Encoding;
use crate::types::NewsArticleSummary;
use crate::wire::VecWriter;

/// The whole [`myField_NewsArtListData`] (321) field: one category's thread,
/// flattened into listing order.
///
/// [`myField_NewsArtListData`]: crate::FieldId::NewsArtListData
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct NewsArticleListData {
    /// Every article in the category, in listing order.
    pub articles: Vec<NewsArticleSummary>,
}

impl NewsArticleListData {
    /// Encode to the packed wire representation.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        let mut w = VecWriter::new()
            .u32(0) // category ID, unused by the client
            .u32(self.articles.len() as u32)
            .with_encoding(encoding)
            .pstring("")? // name, unused by the client
            .pstring("")?; // description, unused by the client
        for article in &self.articles {
            w = w.bytes(&article.encode(encoding)?);
        }
        Ok(w.finish())
    }
}
