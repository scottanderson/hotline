use std::collections::BTreeMap;

use hotline_proto::types::HlDate;
use serde::{Deserialize, Serialize};

use crate::news::repr::{ArticleData, ArticleDataRef, NewsArticleRepr, NewsArticleReprRef};

/// One posted article.
#[derive(Debug, Clone)]
pub struct NewsArticle {
    pub parent_id: u32,
    pub flags: u32,
    pub date: HlDate,
    pub title: String,
    pub poster: String,
    /// Stored bodies, keyed by MIME type. Always has at least one entry.
    pub data: BTreeMap<String, Vec<u8>>,
    /// Reply article IDs, oldest posted first.
    pub children: Vec<u32>,
}

impl NewsArticle {
    /// The article's first stored MIME type and body.
    pub fn primary_flavor(&self) -> (&str, &[u8]) {
        let (mime_type, data) = self
            .data
            .iter()
            .next()
            .expect("post_article always stores at least one flavor");
        (mime_type.as_str(), data.as_slice())
    }
}

impl Serialize for NewsArticle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let data = self
            .data
            .iter()
            .map(|(mime_type, body)| {
                let as_text = std::str::from_utf8(body).ok();
                let data = match as_text {
                    Some(text) => ArticleDataRef::Text(text),
                    None => ArticleDataRef::Bytes(body),
                };
                (mime_type.as_str(), data)
            })
            .collect();
        NewsArticleReprRef {
            parent_id: self.parent_id,
            flags: self.flags,
            date: self.date,
            title: &self.title,
            poster: &self.poster,
            data,
            children: &self.children,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NewsArticle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = NewsArticleRepr::deserialize(deserializer)?;
        let data = repr
            .data
            .into_iter()
            .map(|(mime_type, data)| {
                let data = match data {
                    ArticleData::Text(s) => s.into_bytes(),
                    ArticleData::Bytes(b) => b,
                };
                (mime_type, data)
            })
            .collect();
        Ok(NewsArticle {
            parent_id: repr.parent_id,
            flags: repr.flags,
            date: repr.date,
            title: repr.title,
            poster: repr.poster,
            data,
            children: repr.children,
        })
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::types::HlDate;

    use super::NewsArticle;

    fn article(mime_type: &str, data: &[u8]) -> NewsArticle {
        NewsArticle {
            parent_id: 0,
            flags: 0,
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 0,
            },
            title: "Hi".to_string(),
            poster: "alice".to_string(),
            data: [(mime_type.to_string(), data.to_vec())].into(),
            children: Vec::new(),
        }
    }

    #[test]
    fn date_serializes_as_an_iso_8601_string() {
        // 2024-03-05 is day 64 (0-indexed) of 2024 (leap year); 13:14:15
        // is 47655 seconds into that day.
        let mut a = article("text/plain", b"x");
        a.date = HlDate {
            year: 2024,
            milliseconds: 789,
            seconds: 64 * 86400 + 47655,
        };

        let json = serde_json::to_value(&a).unwrap();

        assert_eq!(json["date"], "2024-03-05T13:14:15.789Z", "{json}");
        let round_tripped: NewsArticle = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.date, a.date);
    }

    #[test]
    fn ascii_article_data_serializes_as_a_string_regardless_of_mime_type() {
        let json = serde_json::to_value(article("application/octet-stream", b"hello")).unwrap();
        assert_eq!(json["data"]["application/octet-stream"], "hello");
    }

    #[test]
    fn text_mime_typed_utf8_data_serializes_as_a_string() {
        let json = serde_json::to_value(article("text/plain", "café".as_bytes())).unwrap();
        assert_eq!(json["data"]["text/plain"], "café");
    }

    #[test]
    fn non_text_mime_typed_utf8_data_still_serializes_as_a_string() {
        // Any data that decodes cleanly as UTF-8 is stored as a string, regardless of MIME type.
        let json =
            serde_json::to_value(article("application/octet-stream", "café".as_bytes())).unwrap();
        assert_eq!(json["data"]["application/octet-stream"], "café");
    }

    #[test]
    fn non_ascii_non_text_data_serializes_as_a_byte_array() {
        let data = [0xff, 0x00, 0x80];
        let json = serde_json::to_value(article("application/octet-stream", &data)).unwrap();
        assert!(
            json["data"]["application/octet-stream"].is_array(),
            "{json}"
        );
    }

    #[test]
    fn article_data_round_trips_through_either_representation() {
        for (mime_type, data) in [
            ("text/plain", b"hello world".as_slice()),
            ("application/octet-stream", &[0xff, 0x00, 0x80]),
        ] {
            let original = article(mime_type, data);
            let json = serde_json::to_string(&original).unwrap();
            let round_tripped: NewsArticle = serde_json::from_str(&json).unwrap();
            assert_eq!(
                round_tripped.data, original.data,
                "for mime type {mime_type}"
            );
        }
    }

    #[test]
    fn primary_flavor_returns_the_only_stored_entry() {
        let a = article("text/plain", b"hello");

        let (mime_type, data) = a.primary_flavor();

        assert_eq!(mime_type, "text/plain");
        assert_eq!(data, b"hello");
    }

    #[test]
    fn an_article_can_store_more_than_one_flavor() {
        let mut a = article("text/plain", b"hello");
        a.data
            .insert("text/html".to_string(), b"<p>hello</p>".to_vec());

        assert_eq!(a.data.len(), 2);
        assert_eq!(a.data["text/html"], b"<p>hello</p>");
    }
}
