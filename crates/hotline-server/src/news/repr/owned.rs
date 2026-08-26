use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::news::iso_date;

/// Mirrors [`crate::news::NewsArticle`] field-for-field, storing `data`
/// as a map of MIME type to [`ArticleData`] for a readable news file.
#[derive(Serialize, Deserialize)]
pub(crate) struct NewsArticleRepr {
    pub(crate) parent_id: u32,
    pub(crate) flags: u32,
    #[serde(with = "iso_date")]
    pub(crate) date: hotline_proto::types::HlDate,
    pub(crate) title: String,
    pub(crate) poster: String,
    pub(crate) data: BTreeMap<String, ArticleData>,
    pub(crate) children: Vec<u32>,
}

/// An article body, stored as plain text when it reads as one (the
/// bytes are valid UTF-8), otherwise as a byte array.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum ArticleData {
    Text(String),
    Bytes(Vec<u8>),
}
