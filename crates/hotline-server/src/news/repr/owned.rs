use serde::{Deserialize, Serialize};

use crate::news::iso_date;

/// Mirrors [`crate::news::NewsArticle`] field-for-field for (de)serialization, storing
/// `data` as [`ArticleData`] instead of a plain `Vec<u8>` so a
/// hand-inspected news file reads like the post it holds rather than an
/// opaque byte array.
#[derive(Serialize, Deserialize)]
pub(crate) struct NewsArticleRepr {
    pub(crate) parent_id: u32,
    pub(crate) flags: u32,
    #[serde(with = "iso_date")]
    pub(crate) date: hotline_proto::types::HlDate,
    pub(crate) title: String,
    pub(crate) poster: String,
    pub(crate) flavor: String,
    pub(crate) data: ArticleData,
    pub(crate) children: Vec<u32>,
}

/// An article's body, stored as plain text when it reads as one (`flavor`
/// starts with `"text/"`, or the bytes are ASCII regardless of flavor),
/// otherwise as a byte array.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ArticleData {
    Text(String),
    Bytes(Vec<u8>),
}
