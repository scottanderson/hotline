use serde::{Deserialize, Serialize};

use super::{NewsBundle, NewsCategory};

/// One item in the news bundle tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NewsNode {
    /// A folder containing other bundles/categories.
    Bundle(NewsBundle),
    /// A leaf category holding a thread of articles.
    Category(NewsCategory),
}

impl NewsNode {
    /// Number of items a listing reports for this node: direct children
    /// for a bundle, total article count for a category.
    pub(super) fn count(&self) -> u16 {
        match self {
            Self::Bundle(b) => b.entries.len() as u16,
            Self::Category(c) => c.articles.len() as u16,
        }
    }
}
