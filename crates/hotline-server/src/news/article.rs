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
    /// This article's one stored MIME type, e.g. `"text/plain"`.
    pub flavor: String,
    pub data: Vec<u8>,
    /// Reply article IDs, oldest posted first.
    pub children: Vec<u32>,
}

impl Serialize for NewsArticle {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let as_text = (self.flavor.starts_with("text/") || self.data.is_ascii())
            .then(|| std::str::from_utf8(&self.data).ok())
            .flatten();
        let data = match as_text {
            Some(text) => ArticleDataRef::Text(text),
            None => ArticleDataRef::Bytes(&self.data),
        };
        NewsArticleReprRef {
            parent_id: self.parent_id,
            flags: self.flags,
            date: self.date,
            title: &self.title,
            poster: &self.poster,
            flavor: &self.flavor,
            data,
            children: &self.children,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for NewsArticle {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let repr = NewsArticleRepr::deserialize(deserializer)?;
        let data = match repr.data {
            ArticleData::Text(s) => s.into_bytes(),
            ArticleData::Bytes(b) => b,
        };
        Ok(NewsArticle {
            parent_id: repr.parent_id,
            flags: repr.flags,
            date: repr.date,
            title: repr.title,
            poster: repr.poster,
            flavor: repr.flavor,
            data,
            children: repr.children,
        })
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::types::HlDate;

    use super::NewsArticle;

    fn article(flavor: &str, data: &[u8]) -> NewsArticle {
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
            flavor: flavor.to_string(),
            data: data.to_vec(),
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
    fn ascii_article_data_serializes_as_a_string_regardless_of_flavor() {
        let json = serde_json::to_value(article("application/octet-stream", b"hello")).unwrap();
        assert_eq!(json["data"], "hello");
    }

    #[test]
    fn text_flavored_utf8_data_serializes_as_a_string() {
        let json = serde_json::to_value(article("text/plain", "café".as_bytes())).unwrap();
        assert_eq!(json["data"], "café");
    }

    #[test]
    fn non_ascii_non_text_data_serializes_as_a_byte_array() {
        let data = [0xff, 0x00, 0x80];
        let json = serde_json::to_value(article("application/octet-stream", &data)).unwrap();
        assert!(json["data"].is_array(), "{json}");
    }

    #[test]
    fn article_data_round_trips_through_either_representation() {
        for (flavor, data) in [
            ("text/plain", b"hello world".as_slice()),
            ("application/octet-stream", &[0xff, 0x00, 0x80]),
        ] {
            let original = article(flavor, data);
            let json = serde_json::to_string(&original).unwrap();
            let round_tripped: NewsArticle = serde_json::from_str(&json).unwrap();
            assert_eq!(round_tripped.data, original.data, "for flavor {flavor}");
        }
    }
}
