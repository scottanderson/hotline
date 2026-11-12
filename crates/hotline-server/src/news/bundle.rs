use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::news::category::MAX_REPLY_CHAIN_LEN;
use crate::news::{
    NewsArticle, NewsArticleView, NewsCategory, NewsItemKind, NewsListKind, NewsNode, PostError,
};

/// Deepest bundle nesting, our own limit. Keeps a saved tree within
/// serde_json's nesting limit.
const MAX_BUNDLE_DEPTH: usize = 16;

/// A folder containing other bundles/categories, keyed by name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NewsBundle {
    /// This bundle's own bundles/categories, keyed by name.
    pub entries: HashMap<Box<str>, NewsNode>,
}

impl NewsBundle {
    /// Walk `components` through nested bundles. `None` if any component
    /// is missing or names a category instead of a bundle.
    fn bundle(&self, components: &[String]) -> Option<&NewsBundle> {
        let mut current = self;
        for component in components {
            current = match current.entries.get(component.as_str())? {
                NewsNode::Bundle(b) => b,
                NewsNode::Category(_) => return None,
            };
        }
        Some(current)
    }

    fn bundle_mut(&mut self, components: &[String]) -> Option<&mut NewsBundle> {
        let mut current = self;
        for component in components {
            current = match current.entries.get_mut(component.as_str())? {
                NewsNode::Bundle(b) => b,
                NewsNode::Category(_) => return None,
            };
        }
        Some(current)
    }

    /// The node named by `components` itself (not its parent). `None` for
    /// an empty path, since the root has no name of its own.
    fn node(&self, components: &[String]) -> Option<&NewsNode> {
        let (last, parent) = components.split_last()?;
        self.bundle(parent)?.entries.get(last.as_str())
    }

    fn category(&self, components: &[String]) -> Option<&NewsCategory> {
        match self.node(components)? {
            NewsNode::Category(c) => Some(c),
            NewsNode::Bundle(_) => None,
        }
    }

    fn category_mut(&mut self, components: &[String]) -> Option<&mut NewsCategory> {
        let (last, parent) = components.split_last()?;
        match self.bundle_mut(parent)?.entries.get_mut(last.as_str())? {
            NewsNode::Category(c) => Some(c),
            NewsNode::Bundle(_) => None,
        }
    }

    /// List `components`' own contents, name-sorted. `None` if it doesn't
    /// resolve to a bundle.
    pub fn list_dir(&self, components: &[String]) -> Option<Vec<(String, NewsListKind)>> {
        let bundle = self.bundle(components)?;
        let mut entries: Vec<(String, NewsListKind)> = bundle
            .entries
            .iter()
            .map(|(name, node)| (name.to_string(), NewsListKind::from(node)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        Some(entries)
    }

    /// List a category's articles, oldest-posted-first within each
    /// sibling level, depth-first (a parent immediately followed by its
    /// own children before its next sibling). `None` if `components`
    /// doesn't resolve to a category.
    pub fn list_articles(&self, components: &[String]) -> Option<Vec<(u32, &NewsArticle)>> {
        let category = self.category(components)?;
        let mut out = Vec::with_capacity(category.articles.len());
        for &id in &category.roots {
            category.push_article_and_children(id, &mut out);
        }
        Some(out)
    }

    /// Create a new bundle named `name` under `components`.
    /// `Err` if `components` isn't a bundle, `name` already exists, or
    /// `name` is too long for the [`myField_NewsCatListData15`] (323)
    /// listing entry it will need to encode into (a 1-byte length prefix).
    /// Also `Err` past `MAX_BUNDLE_DEPTH`.
    ///
    /// [`myField_NewsCatListData15`]: hotline_proto::FieldId::NewsCatListData15
    pub fn mkdir(&mut self, components: &[String], name: &str) -> Result<(), &'static str> {
        if components.len() >= MAX_BUNDLE_DEPTH {
            return Err("news bundles are nested too deep");
        }
        let parent = self.check_new_name(components, name)?;
        parent
            .entries
            .insert(name.into(), NewsNode::Bundle(NewsBundle::default()));
        Ok(())
    }

    /// Create a new category named `name` under `components`. See
    /// [`Self::mkdir`] for the error cases.
    pub fn mkcategory(&mut self, components: &[String], name: &str) -> Result<(), &'static str> {
        self.insert_category(components, name, NewsCategory::new())
    }

    /// Add `category` as `name` under `components`. See [`Self::mkdir`]
    /// for the error cases.
    pub fn insert_category(
        &mut self,
        components: &[String],
        name: &str,
        category: NewsCategory,
    ) -> Result<(), &'static str> {
        let parent = self.check_new_name(components, name)?;
        parent
            .entries
            .insert(name.into(), NewsNode::Category(category));
        Ok(())
    }

    fn check_new_name(
        &mut self,
        components: &[String],
        name: &str,
    ) -> Result<&mut NewsBundle, &'static str> {
        if name.len() > 255 {
            return Err("name too long");
        }
        let parent = self.bundle_mut(components).ok_or("no such news bundle")?;
        if parent.entries.contains_key(name) {
            return Err("an item with that name already exists");
        }
        Ok(parent)
    }

    /// Remove the bundle or category named by `components` (its last
    /// component is the item's own name). `Err` for the root, or a path
    /// that doesn't resolve.
    pub fn delete(&mut self, components: &[String]) -> Result<(), &'static str> {
        let (last, parent) = components.split_last().ok_or("cannot delete the root")?;
        let parent = self.bundle_mut(parent).ok_or("news item not found")?;
        if !parent.entries.contains_key(last.as_str()) {
            return Err("news item not found");
        }
        parent.entries.remove(last.as_str());
        Ok(())
    }

    /// Whether `components` (non-empty; the root has no kind of its own)
    /// names a bundle or a category. `None` if it doesn't exist.
    pub fn item_kind(&self, components: &[String]) -> Option<NewsItemKind> {
        match self.node(components)? {
            NewsNode::Bundle(_) => Some(NewsItemKind::Bundle),
            NewsNode::Category(_) => Some(NewsItemKind::Category),
        }
    }

    /// Post a new article into the category at `components`.
    /// `Err` if `components` isn't a category, or `parent_id` is nonzero
    /// and doesn't name an existing article in it.
    #[allow(clippy::too_many_arguments)]
    pub fn post_article(
        &mut self,
        components: &[String],
        parent_id: u32,
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
    ) -> Result<u32, PostError> {
        // Each is wire-encoded with a 1-byte length prefix in the
        // article listing (`myField_NewsArtListData` (321)). Openline
        // actually caps title/flavor at a much smaller fixed buffer and
        // truncates silently rather than rejecting; this server accepts
        // up to the wire format's real 255-byte limit and errors past
        // it, a deliberate, more permissive divergence.
        if title.len() > 255 || poster.len() > 255 || mime_type.len() > 255 {
            Err("post data too long")?;
        }
        let category = self
            .category_mut(components)
            .ok_or("no such news category")?;
        if parent_id != 0 && category.depth_of(parent_id) + 1 >= MAX_REPLY_CHAIN_LEN {
            Err("reply chain is too deep")?;
        }
        category.add_article(parent_id, title, poster, flags, mime_type, data)
    }

    /// Delete an article from the category at `components`. If
    /// `recursive`, every reply under it is deleted too; otherwise its
    /// first reply is promoted into its place, taking on its remaining
    /// replies as its own.
    pub fn delete_article(
        &mut self,
        components: &[String],
        id: u32,
        recursive: bool,
    ) -> Result<(), &'static str> {
        let category = self
            .category_mut(components)
            .ok_or("no such news category")?;
        category.delete_article(id, recursive)
    }

    /// One article's data, plus its sibling/child linkage for a
    /// [`myTran_GetNewsArtData`] (400) reply.
    ///
    /// [`myTran_GetNewsArtData`]: hotline_proto::Opcode::GetNewsArtData
    pub fn article(&self, components: &[String], id: u32) -> Option<NewsArticleView<'_>> {
        let category = self.category(components)?;
        let article = category.articles.get(&id)?;
        let siblings = category.siblings_of(id);
        let index = siblings.iter().position(|&sid| sid == id)?;
        Some(NewsArticleView {
            article,
            prev_id: index
                .checked_sub(1)
                .and_then(|i| siblings.get(i))
                .copied()
                .unwrap_or(0),
            next_id: siblings.get(index + 1).copied().unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bundle_serializes_as_a_bare_map_with_no_entries_wrapper() {
        let mut root = NewsBundle::default();
        root.mkdir(&[], "Games").unwrap();

        let json = serde_json::to_value(&root).unwrap();

        assert!(json.get("entries").is_none(), "{json}");
        assert!(json.get("Games").is_some(), "{json}");
        let round_tripped: NewsBundle = serde_json::from_value(json).unwrap();
        assert!(round_tripped.entries.contains_key("Games"));
    }

    #[test]
    fn mkdir_and_mkcategory_reject_a_duplicate_name() {
        let mut root = NewsBundle::default();
        root.mkdir(&[], "Games").unwrap();
        assert!(root.mkdir(&[], "Games").is_err());
        assert!(root.mkcategory(&[], "Games").is_err());
    }

    #[test]
    fn list_dir_reports_bundle_item_count_and_category_article_count() {
        let mut root = NewsBundle::default();
        root.mkdir(&[], "Bundle").unwrap();
        root.mkcategory(&[], "General").unwrap();
        root.post_article(
            &["General".to_string()],
            0,
            "Hi".into(),
            "alice".into(),
            0,
            "text/plain".into(),
            b"hello".as_slice().into(),
        )
        .unwrap();

        let mut entries = root.list_dir(&[]).unwrap();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].1, NewsListKind::Bundle { count: 0 }));
        match &entries[1].1 {
            NewsListKind::Category(summary) => assert_eq!(summary.count, 1),
            other => panic!("expected a category, got {other:?}"),
        }
    }

    #[test]
    fn delete_rejects_the_root() {
        let mut root = NewsBundle::default();
        assert!(root.delete(&[]).is_err());
    }

    #[test]
    fn post_article_threads_replies_under_their_parent() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let path = ["General".to_string()];
        let top = root
            .post_article(
                &path,
                0,
                "Top".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"1".as_slice().into(),
            )
            .unwrap();
        let reply = root
            .post_article(
                &path,
                top,
                "Re: Top".into(),
                "bob".into(),
                0,
                "text/plain".into(),
                b"2".as_slice().into(),
            )
            .unwrap();

        let listing = root.list_articles(&path).unwrap();
        assert_eq!(listing.len(), 2);

        let view = root.article(&path, reply).unwrap();
        assert_eq!(view.article.parent_id, top);
        assert_eq!(view.prev_id, 0);
        assert_eq!(view.next_id, 0);
    }

    #[test]
    fn post_article_rejects_an_unknown_parent() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let err = root
            .post_article(
                &["General".to_string()],
                999,
                "x".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"x".as_slice().into(),
            )
            .unwrap_err();
        assert_eq!(err, PostError::Invalid("no such parent article"));
    }

    /// Posts a reply to `parent_id` `count` times, each reply the parent
    /// of the next, returning the last posted article's id.
    fn post_reply_chain(
        root: &mut NewsBundle,
        path: &[String],
        mut parent_id: u32,
        count: u32,
    ) -> u32 {
        for _ in 0..count {
            parent_id = root
                .post_article(
                    path,
                    parent_id,
                    "Re".into(),
                    "alice".into(),
                    0,
                    "text/plain".into(),
                    b"x".as_slice().into(),
                )
                .unwrap();
        }
        parent_id
    }

    #[test]
    fn post_article_allows_a_reply_chain_up_to_the_length_cap() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let path = ["General".to_string()];

        let last = post_reply_chain(&mut root, &path, 0, MAX_REPLY_CHAIN_LEN as u32);

        let listing = root.list_articles(&path).unwrap();
        assert_eq!(listing.len(), MAX_REPLY_CHAIN_LEN);
        assert_eq!(
            root.category(&path).unwrap().depth_of(last),
            MAX_REPLY_CHAIN_LEN - 1
        );
    }

    #[test]
    fn post_article_rejects_a_reply_chain_past_the_length_cap() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let path = ["General".to_string()];

        let last = post_reply_chain(&mut root, &path, 0, MAX_REPLY_CHAIN_LEN as u32);

        let err = root
            .post_article(
                &path,
                last,
                "One reply too many".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"x".as_slice().into(),
            )
            .unwrap_err();
        assert_eq!(err, PostError::Invalid("reply chain is too deep"));
        // The rejected post left the chain exactly at the cap.
        assert_eq!(
            root.list_articles(&path).unwrap().len(),
            MAX_REPLY_CHAIN_LEN
        );
    }

    #[test]
    fn mkdir_rejects_a_bundle_past_the_depth_cap() {
        let mut root = NewsBundle::default();
        let path: Vec<String> = (0..MAX_BUNDLE_DEPTH).map(|i| format!("b{i}")).collect();
        for depth in 0..MAX_BUNDLE_DEPTH {
            root.mkdir(&path[..depth], &path[depth]).unwrap();
        }

        let err = root.mkdir(&path, "one too many").unwrap_err();

        assert_eq!(err, "news bundles are nested too deep");
    }

    #[test]
    fn the_deepest_allowed_tree_loads_back_from_json_text() {
        let mut root = NewsBundle::default();
        let mut path: Vec<String> = (0..MAX_BUNDLE_DEPTH).map(|i| format!("b{i}")).collect();
        for depth in 0..MAX_BUNDLE_DEPTH {
            root.mkdir(&path[..depth], &path[depth]).unwrap();
        }
        root.mkcategory(&path, "General").unwrap();
        path.push("General".to_string());
        post_reply_chain(&mut root, &path, 0, MAX_REPLY_CHAIN_LEN as u32);

        let json = serde_json::to_vec_pretty(&root).unwrap();
        let loaded: NewsBundle = serde_json::from_slice(&json).unwrap();

        assert_eq!(
            loaded.list_articles(&path).unwrap().len(),
            MAX_REPLY_CHAIN_LEN
        );
    }

    #[test]
    fn delete_article_recursive_removes_the_whole_subtree() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let path = ["General".to_string()];
        let top = root
            .post_article(
                &path,
                0,
                "Top".into(),
                "a".into(),
                0,
                "text/plain".into(),
                b"1".as_slice().into(),
            )
            .unwrap();
        let reply = root
            .post_article(
                &path,
                top,
                "Re".into(),
                "b".into(),
                0,
                "text/plain".into(),
                b"2".as_slice().into(),
            )
            .unwrap();

        root.delete_article(&path, top, true).unwrap();

        assert!(root.article(&path, top).is_none());
        assert!(root.article(&path, reply).is_none());
        assert!(root.list_articles(&path).unwrap().is_empty());
    }

    #[test]
    fn delete_article_non_recursive_promotes_the_first_child() {
        let mut root = NewsBundle::default();
        root.mkcategory(&[], "General").unwrap();
        let path = ["General".to_string()];
        let top = root
            .post_article(
                &path,
                0,
                "Top".into(),
                "a".into(),
                0,
                "text/plain".into(),
                b"1".as_slice().into(),
            )
            .unwrap();
        let first = root
            .post_article(
                &path,
                top,
                "First".into(),
                "b".into(),
                0,
                "text/plain".into(),
                b"2".as_slice().into(),
            )
            .unwrap();
        let second = root
            .post_article(
                &path,
                top,
                "Second".into(),
                "c".into(),
                0,
                "text/plain".into(),
                b"3".as_slice().into(),
            )
            .unwrap();

        root.delete_article(&path, top, false).unwrap();

        assert!(root.article(&path, top).is_none());
        assert_eq!(root.list_articles(&path).unwrap().len(), 2);

        // The first reply is promoted to top level, in `top`'s old spot.
        let promoted = root.article(&path, first).unwrap();
        assert_eq!(promoted.article.parent_id, 0);
        // The other reply becomes the promoted article's own child.
        assert_eq!(promoted.article.children, vec![second]);
        let demoted = root.article(&path, second).unwrap();
        assert_eq!(demoted.article.parent_id, first);
    }
}
