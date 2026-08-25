//! Threaded news storage: a tree of bundles (folders) and categories
//! (article threads), addressed by `myField_NewsPath` (325).
//!
//! [`NewsBundle`] mirrors [`crate::file::Dir`]'s nested-tree shape: an
//! in-memory tree keyed by path component, with no per-login scoping;
//! [`NewsProvider`] persists it. A [`NewsCategory`] leaf holds a thread of
//! [`NewsArticle`]s; sibling order (`myField_NewsArtPrevArt`/
//! `_NEXTTHREADID`) comes from each article's position in its parent's
//! `children` list, not stored previous/next pointers.

mod article;
mod bundle;
mod category;
mod detail;
mod iso_date;
mod item;
mod listing;
mod memory;
mod node;
mod provider;
mod repr;
mod summary;
mod view;

pub use article::NewsArticle;
pub use bundle::NewsBundle;
pub use category::NewsCategory;
pub use detail::NewsArticleDetail;
pub use item::NewsItemKind;
pub use listing::NewsListKind;
pub use memory::InMemoryNewsProvider;
pub use node::NewsNode;
pub use provider::NewsProvider;
pub use summary::NewsCategorySummary;
pub use view::NewsArticleView;
