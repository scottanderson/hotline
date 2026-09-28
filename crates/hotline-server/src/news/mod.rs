//! Threaded news storage: a tree of bundles (folders) and categories
//! (article threads), addressed by [`myField_NewsPath`] (325).
//!
//! [`NewsBundle`] mirrors the nested-tree shape of `file::dir::Dir`: an
//! in-memory tree keyed by path component, with no per-login scoping;
//! [`ThreadedNewsProvider`] persists it. A [`NewsCategory`] leaf holds a
//! thread of [`NewsArticle`]s; sibling order ([`myField_NewsArtPrevArt`] (331)/
//! [`myField_NewsArtNextArt`] (332)) comes from each article's position in
//! its parent's `children` list, not stored previous/next pointers.
//!
//! [`myField_NewsPath`]: hotline_proto::FieldId::NewsPath
//! [`myField_NewsArtPrevArt`]: hotline_proto::FieldId::NewsArtPrevArt
//! [`myField_NewsArtNextArt`]: hotline_proto::FieldId::NewsArtNextArt

mod article;
mod bundle;
mod category;
mod detail;
mod flavor_map;
mod iso_date;
mod item;
mod listing;
mod memory;
mod node;
mod provider;
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
pub use provider::{FlatNewsProvider, ThreadedNewsProvider};
pub use summary::NewsCategorySummary;
pub use view::NewsArticleView;
