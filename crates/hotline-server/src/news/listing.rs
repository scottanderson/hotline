use super::{NewsCategorySummary, NewsNode};

/// What kind of item [`NewsBundle::list_dir`](super::NewsBundle::list_dir) found, plus the listing
/// fields that differ between the two.
#[derive(Debug, Clone)]
pub enum NewsListKind {
    /// A folder, and how many entries it holds.
    Bundle {
        /// The bundle's own entry count.
        count: u16,
    },
    /// An article thread.
    Category(NewsCategorySummary),
}

impl From<&NewsNode> for NewsListKind {
    fn from(node: &NewsNode) -> Self {
        match node {
            NewsNode::Bundle(_) => Self::Bundle {
                count: node.count(),
            },
            NewsNode::Category(c) => Self::Category(NewsCategorySummary {
                count: node.count(),
                guid: c.guid.into_bytes(),
                add_sn: c.add_sn,
                del_sn: c.del_sn,
            }),
        }
    }
}
