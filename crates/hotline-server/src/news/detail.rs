use std::collections::BTreeMap;

use hotline_proto::types::HlDate;

/// One threaded news article's full data, for a `myTran_GetNewsArtData`
/// (400) reply.
#[derive(Debug, Clone)]
pub struct NewsArticleDetail {
    pub title: String,
    pub poster: String,
    pub date: HlDate,
    pub prev_id: u32,
    pub next_id: u32,
    pub parent_id: u32,
    pub first_child_id: u32,
    /// This article's stored bodies, keyed by MIME type.
    pub data: BTreeMap<String, Vec<u8>>,
}
