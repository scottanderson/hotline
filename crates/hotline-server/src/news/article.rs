use std::collections::BTreeMap;

use hotline_proto::types::HlDate;

/// One posted article. [`crate::news::NewsCategory`] owns this type's
/// JSON shape.
#[derive(Debug, Clone)]
pub struct NewsArticle {
    /// The article it replies to, or `0` if it's a top-level post.
    pub parent_id: u32,
    /// `myField_NewsArtFlags` (334).
    pub flags: u32,
    /// When it was posted.
    pub date: HlDate,
    /// The article's subject line.
    pub title: String,
    /// The name of the user who posted it.
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

#[cfg(test)]
mod tests {
    use hotline_proto::types::HlDate;

    use super::NewsArticle;

    #[test]
    fn primary_flavor_returns_the_only_stored_entry() {
        let a = NewsArticle {
            parent_id: 0,
            flags: 0,
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 0,
            },
            title: "Hi".to_string(),
            poster: "alice".to_string(),
            data: [("text/plain".to_string(), b"hello".to_vec())].into(),
            children: Vec::new(),
        };

        let (mime_type, data) = a.primary_flavor();

        assert_eq!(mime_type, "text/plain");
        assert_eq!(data, b"hello");
    }
}
