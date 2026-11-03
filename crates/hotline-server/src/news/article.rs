use std::collections::BTreeMap;

use hotline_proto::types::HlDate;

/// One posted article. `NewsCategory` owns this type's
/// JSON shape.
#[derive(Debug, Clone)]
pub struct NewsArticle {
    /// The article it replies to, or `0` if it's a top-level post.
    pub parent_id: u32,
    /// [`myField_NewsArtFlags`] (334).
    ///
    /// [`myField_NewsArtFlags`]: hotline_proto::FieldId::NewsArtFlags
    pub flags: u32,
    /// When it was posted.
    pub date: HlDate,
    /// The article's subject line.
    pub title: Box<str>,
    /// The name of the user who posted it.
    pub poster: Box<str>,
    /// Stored bodies, keyed by MIME type. Always has at least one entry.
    pub data: BTreeMap<Box<str>, Box<[u8]>>,
    /// Reply article IDs, oldest posted first.
    pub children: Vec<u32>,
}
