use crate::news::NewsArticle;

/// One article's data plus its resolved sibling links, for a
/// `myTran_GetNewsArtData` (400) reply.
pub struct NewsArticleView<'a> {
    pub article: &'a NewsArticle,
    pub prev_id: u32,
    pub next_id: u32,
}
