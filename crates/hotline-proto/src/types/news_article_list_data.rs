//! [`NewsArticleListData`]: the whole `myField_NewsArtListData` (321) field.

use crate::error::Result;
use crate::text_encoding::Encoding;
use crate::types::NewsArticleSummary;
use crate::wire::{SliceReader, VecWriter};

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

    /// Decode from a field's raw data.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<Self> {
        let mut r = SliceReader::new(buf).with_encoding(encoding);
        let _category_id = r.u32()?;
        let count = r.u32()?;
        let _name = r.pstring()?;
        let _description = r.pstring()?;
        let articles = (0..count)
            .map(|_| NewsArticleSummary::read(&mut r))
            .collect::<Result<_>>()?;
        Ok(Self { articles })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_encoding;
    use crate::types::{HlDate, NewsFlavor};

    #[test]
    fn a_listing_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let article = |id, parent_id| NewsArticleSummary {
            id,
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 60,
            },
            parent_id,
            flags: 0,
            title: format!("Article {id}").into(),
            poster: "alice".into(),
            flavors: vec![NewsFlavor {
                mime_type: "text/plain".into(),
                size: 4,
            }],
        };
        let listing = NewsArticleListData {
            articles: vec![article(1, 0), article(2, 1)],
        };
        let buf = listing.encode(encoding).unwrap();
        assert_eq!(
            NewsArticleListData::decode(&buf, encoding).unwrap(),
            listing
        );
    }

    #[test]
    fn a_count_past_the_data_is_an_error() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = NewsArticleListData::default().encode(encoding).unwrap();
        buf[7] = 1;
        assert!(NewsArticleListData::decode(&buf, encoding).is_err());
    }
}
