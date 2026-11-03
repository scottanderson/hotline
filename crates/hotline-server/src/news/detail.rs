use std::collections::BTreeMap;

use hotline_proto::types::HlDate;

use crate::news::NewsArticleView;

/// One threaded news article's full data, for a
/// [`myTran_GetNewsArtData`] (400) reply.
///
/// [`myTran_GetNewsArtData`]: hotline_proto::Opcode::GetNewsArtData
#[derive(Debug, Clone)]
pub struct NewsArticleDetail {
    /// The article's subject line.
    pub title: Box<str>,
    /// The name of the user who posted it.
    pub poster: Box<str>,
    /// When it was posted.
    pub date: HlDate,
    /// The preceding sibling's article ID, or `0` if it's the first.
    pub prev_id: u32,
    /// The following sibling's article ID, or `0` if it's the last.
    pub next_id: u32,
    /// The article it replies to, or `0` if it's a top-level post.
    pub parent_id: u32,
    /// Its first reply's article ID, or `0` if it has none.
    pub first_child_id: u32,
    /// This article's stored bodies, keyed by MIME type.
    pub data: BTreeMap<Box<str>, Box<[u8]>>,
}

impl From<NewsArticleView<'_>> for NewsArticleDetail {
    fn from(view: NewsArticleView<'_>) -> Self {
        Self {
            title: view.article.title.clone(),
            poster: view.article.poster.clone(),
            date: view.article.date,
            prev_id: view.prev_id,
            next_id: view.next_id,
            parent_id: view.article.parent_id,
            first_child_id: view.article.children.first().copied().unwrap_or(0),
            data: view.article.data.clone(),
        }
    }
}
