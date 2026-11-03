use std::sync::Arc;

use async_trait::async_trait;

use super::{NewsArticle, NewsArticleDetail, NewsItemKind, NewsListKind};

/// Classic (flat) news persistence, supplied by the embedding application
/// like [`crate::AccountProvider`]/[`crate::FileProvider`].
#[async_trait]
pub trait FlatNewsProvider: Send + Sync {
    /// Loads the flat news text.
    async fn load(&self) -> Arc<str>;
    /// Prepends `entry` to the flat news text.
    async fn post(&self, entry: &str);
}

/// Threaded news persistence, supplied by the embedding application like
/// [`crate::AccountProvider`]/[`crate::FileProvider`]. Each method is one
/// storage operation.
#[async_trait]
pub trait ThreadedNewsProvider: Send + Sync {
    /// Lists `components`' own contents, name-sorted. `None` if it doesn't
    /// resolve to a bundle.
    async fn list_dir(&self, components: &[String]) -> Option<Vec<(String, NewsListKind)>>;
    /// Lists a category's articles at `components`. `None` if it doesn't
    /// resolve to a category.
    async fn list_articles(&self, components: &[String]) -> Option<Vec<(u32, NewsArticle)>>;
    /// One article's data, plus its sibling/parent/child linkage. `None` if
    /// the category or the article itself doesn't exist.
    async fn article(&self, components: &[String], id: u32) -> Option<NewsArticleDetail>;
    /// Whether `components` names a bundle or a category. `None` if it
    /// doesn't exist.
    async fn item_kind(&self, components: &[String]) -> Option<NewsItemKind>;
    /// Creates a bundle named `name` under `components`.
    async fn mkdir(&self, components: &[String], name: &str) -> Result<(), &'static str>;
    /// Creates a category named `name` under `components`.
    async fn mkcategory(&self, components: &[String], name: &str) -> Result<(), &'static str>;
    /// Removes the bundle or category named by `components` (its last
    /// component is the item's own name).
    async fn delete(&self, components: &[String]) -> Result<(), &'static str>;
    /// Posts a new article into the category at `components`.
    #[allow(clippy::too_many_arguments)]
    async fn post_article(
        &self,
        components: &[String],
        parent_id: u32,
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
    ) -> Result<u32, &'static str>;
    /// Deletes an article from the category at `components`. If
    /// `recursive`, every reply under it is deleted too; otherwise its
    /// first reply is promoted into its place.
    async fn delete_article(
        &self,
        components: &[String],
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str>;
}
