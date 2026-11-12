//! Why a post made no article.

/// Why [`ThreadedNewsProvider::post_article`] made no article.
///
/// [`ThreadedNewsProvider::post_article`]: crate::ThreadedNewsProvider::post_article
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostError {
    /// The category gave out article ID `u32::MAX`, and has no ID left.
    NoArticleId,
    /// The request is not valid.
    Invalid(&'static str),
}

impl From<&'static str> for PostError {
    fn from(reason: &'static str) -> Self {
        Self::Invalid(reason)
    }
}
