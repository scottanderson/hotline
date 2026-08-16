//! `serde` support for [`NewsArticle`], kept separate from the plain data
//! types in the parent module: its `date`/`data` fields need conversion
//! logic ([`hotline_proto::types::HlDate`] to an ISO 8601 string via
//! [`super::iso_date`], article bytes to plain text when they read as
//! one) that a derive alone can't express.

use serde::{Deserialize, Serialize};

use super::{iso_date, NewsArticle};

/// Mirrors [`NewsArticle`] field-for-field for (de)serialization, storing
/// `data` as [`ArticleData`] instead of a plain `Vec<u8>` so a
/// hand-inspected news file reads like the post it holds rather than an
/// opaque byte array.
#[derive(Serialize, Deserialize)]
struct NewsArticleRepr {
    parent_id: u32,
    flags: u32,
    #[serde(with = "iso_date")]
    date: hotline_proto::types::HlDate,
    title: String,
    poster: String,
    flavor: String,
    data: ArticleData,
    children: Vec<u32>,
}

/// An article's body, stored as plain text when it reads as one (`flavor`
/// starts with `"text/"`, or the bytes are ASCII regardless of flavor),
/// otherwise as a byte array.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ArticleData {
    Text(String),
    Bytes(Vec<u8>),
}

/// Borrowing counterpart to [`NewsArticleRepr`]/[`ArticleData`], used only
/// for `Serialize`: it only needs to borrow from `&NewsArticle` for the
/// duration of one `serialize` call, so there's no need to clone every
/// field into an owned copy first the way `Deserialize` (which must
/// produce new owned data) does.
#[derive(Serialize)]
struct NewsArticleReprRef<'a> {
    parent_id: u32,
    flags: u32,
    #[serde(with = "iso_date")]
    date: hotline_proto::types::HlDate,
    title: &'a str,
    poster: &'a str,
    flavor: &'a str,
    data: ArticleDataRef<'a>,
    children: &'a [u32],
}

#[derive(Serialize)]
#[serde(untagged)]
enum ArticleDataRef<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
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
