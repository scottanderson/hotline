//! Threaded news storage: a tree of bundles (folders) and categories
//! (article threads), addressed by `HTLC_DATA_NEWS_DIR` (325).
//!
//! [`NewsBundle`] mirrors [`crate::files::Dir`]'s nested-tree shape: an
//! in-memory tree keyed by path component, with no persistence and no
//! per-login scoping. A [`NewsCategory`] leaf holds a thread of
//! [`NewsArticle`]s; sibling order (used for `HTLC_DATA_NEWS_PREVTHREADID`/
//! `HTLC_DATA_NEWS_NEXTTHREADID`) is derived from each article's position
//! in its parent's `children` list rather than stored as explicit
//! previous/next pointers.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::SystemTime;

use hotline_proto::types::HlDate;

/// One item in the news bundle tree.
#[derive(Debug, Clone)]
pub enum NewsNode {
    /// A folder containing other bundles/categories.
    Bundle(NewsBundle),
    /// A leaf category holding a thread of articles.
    Category(NewsCategory),
}

impl NewsNode {
    /// Number of items a listing reports for this node: direct children
    /// for a bundle, total article count for a category.
    fn count(&self) -> u16 {
        match self {
            Self::Bundle(b) => b.entries.len() as u16,
            Self::Category(c) => c.articles.len() as u16,
        }
    }
}

/// A folder containing other bundles/categories, keyed by name.
#[derive(Debug, Clone, Default)]
pub struct NewsBundle {
    pub entries: HashMap<String, NewsNode>,
}

impl NewsBundle {
    /// Walk `components` through nested bundles. `None` if any component
    /// is missing or names a category instead of a bundle.
    fn bundle(&self, components: &[String]) -> Option<&NewsBundle> {
        let mut current = self;
        for component in components {
            current = match current.entries.get(component)? {
                NewsNode::Bundle(b) => b,
                NewsNode::Category(_) => return None,
            };
        }
        Some(current)
    }

    fn bundle_mut(&mut self, components: &[String]) -> Option<&mut NewsBundle> {
        let mut current = self;
        for component in components {
            current = match current.entries.get_mut(component)? {
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
        self.bundle(parent)?.entries.get(last)
    }

    fn category(&self, components: &[String]) -> Option<&NewsCategory> {
        match self.node(components)? {
            NewsNode::Category(c) => Some(c),
            NewsNode::Bundle(_) => None,
        }
    }

    fn category_mut(&mut self, components: &[String]) -> Option<&mut NewsCategory> {
        let (last, parent) = components.split_last()?;
        match self.bundle_mut(parent)?.entries.get_mut(last)? {
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
            .map(|(name, node)| (name.clone(), NewsListKind::from(node)))
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
    /// `name` is too long for the `HTLS_DATA_NEWS_DIRLIST_EXTENDED` (323)
    /// listing entry it will need to encode into (a 1-byte length prefix).
    pub fn mkdir(&mut self, components: &[String], name: &str) -> Result<(), &'static str> {
        let parent = self.check_new_name(components, name)?;
        parent
            .entries
            .insert(name.to_string(), NewsNode::Bundle(NewsBundle::default()));
        Ok(())
    }

    /// Create a new category named `name` under `components`. See
    /// [`Self::mkdir`] for the error cases.
    pub fn mkcategory(&mut self, components: &[String], name: &str) -> Result<(), &'static str> {
        let parent = self.check_new_name(components, name)?;
        parent
            .entries
            .insert(name.to_string(), NewsNode::Category(NewsCategory::new()));
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
        if !parent.entries.contains_key(last) {
            return Err("news item not found");
        }
        parent.entries.remove(last);
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
        title: String,
        poster: String,
        flags: u32,
        flavor: String,
        data: Vec<u8>,
    ) -> Result<u32, &'static str> {
        // Each is wire-encoded with a 1-byte length prefix in the
        // article listing (`HTLC_DATA_NEWS_CATLIST`, 321). Openline
        // actually caps title/flavor at a much smaller fixed buffer and
        // truncates silently rather than rejecting; this server accepts
        // up to the wire format's real 255-byte limit and errors past
        // it, a deliberate, more permissive divergence.
        if title.len() > 255 || poster.len() > 255 || flavor.len() > 255 {
            return Err("post data too long");
        }
        let category = self
            .category_mut(components)
            .ok_or("no such news category")?;
        if parent_id != 0 && !category.articles.contains_key(&parent_id) {
            return Err("no such parent article");
        }
        Ok(category.add_article(parent_id, title, poster, flags, flavor, data))
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
    /// `HTLC_HDR_NEWS_GETTHREAD` (400) reply.
    pub fn article(&self, components: &[String], id: u32) -> Option<ArticleView<'_>> {
        let category = self.category(components)?;
        let article = category.articles.get(&id)?;
        let siblings = category.siblings_of(id);
        let index = siblings.iter().position(|&sid| sid == id)?;
        Some(ArticleView {
            article,
            prev_id: index.checked_sub(1).map(|i| siblings[i]).unwrap_or(0),
            next_id: siblings.get(index + 1).copied().unwrap_or(0),
        })
    }
}

/// Which of the two node kinds a news path resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsItemKind {
    Bundle,
    Category,
}

/// What kind of item [`NewsBundle::list_dir`] found, plus the listing
/// fields that differ between the two.
#[derive(Debug, Clone)]
pub enum NewsListKind {
    Bundle { count: u16 },
    Category(NewsCategorySummary),
}

/// A category's listing fields: everything [`NewsBundle::list_dir`] needs
/// besides the item's name.
#[derive(Debug, Clone)]
pub struct NewsCategorySummary {
    pub count: u16,
    pub guid: [u8; 16],
    pub add_sn: u32,
    pub del_sn: u32,
}

impl From<&NewsNode> for NewsListKind {
    fn from(node: &NewsNode) -> Self {
        match node {
            NewsNode::Bundle(_) => Self::Bundle {
                count: node.count(),
            },
            NewsNode::Category(c) => Self::Category(NewsCategorySummary {
                count: node.count(),
                guid: c.guid,
                add_sn: c.add_sn,
                del_sn: c.del_sn,
            }),
        }
    }
}

/// One article's data plus its resolved sibling links, for a
/// `HTLC_HDR_NEWS_GETTHREAD` (400) reply.
pub struct ArticleView<'a> {
    pub article: &'a NewsArticle,
    pub prev_id: u32,
    pub next_id: u32,
}

/// A leaf category: a thread of articles plus the bookkeeping mirrored
/// into `HTLC_DATA_NEWS_DIRLIST_EXTENDED` (323).
#[derive(Debug, Clone)]
pub struct NewsCategory {
    pub guid: [u8; 16],
    /// Incremented by one each time an article is added. Never reset.
    pub add_sn: u32,
    /// Always `1`: the reference this mirrors never actually updates it
    /// past its initial value once a category is created.
    pub del_sn: u32,
    next_article_id: u32,
    pub articles: HashMap<u32, NewsArticle>,
    /// Top-level (`parent_id == 0`) article IDs, oldest posted first.
    pub roots: Vec<u32>,
}

/// Deterministic, process-unique category identifiers. Not a real GUID:
/// nothing about this server or the wire format ever validates its
/// contents, only that it's 16 bytes and stable for the category's life.
static NEXT_GUID: AtomicU32 = AtomicU32::new(1);

impl NewsCategory {
    fn new() -> Self {
        let mut guid = [0u8; 16];
        guid[12..].copy_from_slice(&NEXT_GUID.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        Self {
            guid,
            add_sn: 1,
            del_sn: 1,
            next_article_id: 1,
            articles: HashMap::new(),
            roots: Vec::new(),
        }
    }

    /// This article's sibling list: `roots` for a top-level article, or
    /// its parent's `children` for a reply.
    fn siblings_of(&self, id: u32) -> &[u32] {
        match self.articles.get(&id) {
            Some(article) if article.parent_id != 0 => {
                match self.articles.get(&article.parent_id) {
                    Some(parent) => &parent.children,
                    None => &[],
                }
            }
            _ => &self.roots,
        }
    }

    fn siblings_of_mut(&mut self, parent_id: u32) -> &mut Vec<u32> {
        if parent_id == 0 {
            &mut self.roots
        } else {
            &mut self
                .articles
                .get_mut(&parent_id)
                .expect("caller already checked the parent exists")
                .children
        }
    }

    fn add_article(
        &mut self,
        parent_id: u32,
        title: String,
        poster: String,
        flags: u32,
        flavor: String,
        data: Vec<u8>,
    ) -> u32 {
        let id = self.next_article_id;
        self.next_article_id += 1;
        self.articles.insert(
            id,
            NewsArticle {
                parent_id,
                flags,
                date: now_as_hl_date(),
                title,
                poster,
                flavor,
                data,
                children: Vec::new(),
            },
        );
        self.siblings_of_mut(parent_id).push(id);
        self.add_sn += 1;
        id
    }

    fn delete_article(&mut self, id: u32, recursive: bool) -> Result<(), &'static str> {
        let article = self.articles.get(&id).ok_or("no such article")?;
        let parent_id = article.parent_id;
        let children = article.children.clone();

        if recursive {
            for child in children {
                self.delete_article(child, true)?;
            }
            self.remove_from_siblings(parent_id, id);
            self.articles.remove(&id);
            return Ok(());
        }

        if children.is_empty() {
            self.remove_from_siblings(parent_id, id);
            self.articles.remove(&id);
            return Ok(());
        }

        // Promote the first child into the deleted article's place; its
        // own remaining siblings become the promoted child's own
        // (trailing) children.
        let (&first_child, rest) = children.split_first().expect("checked non-empty above");
        let siblings = self.siblings_of_mut(parent_id);
        let index = siblings
            .iter()
            .position(|&sid| sid == id)
            .expect("article is its own sibling");
        siblings[index] = first_child;

        for &sibling in rest {
            if let Some(a) = self.articles.get_mut(&sibling) {
                a.parent_id = first_child;
            }
        }
        if let Some(promoted) = self.articles.get_mut(&first_child) {
            promoted.parent_id = parent_id;
            promoted.children.extend_from_slice(rest);
        }

        self.articles.remove(&id);
        Ok(())
    }

    fn remove_from_siblings(&mut self, parent_id: u32, id: u32) {
        self.siblings_of_mut(parent_id).retain(|&sid| sid != id);
    }

    fn push_article_and_children<'a>(&'a self, id: u32, out: &mut Vec<(u32, &'a NewsArticle)>) {
        if let Some(article) = self.articles.get(&id) {
            out.push((id, article));
            for &child in &article.children {
                self.push_article_and_children(child, out);
            }
        }
    }
}

/// One posted article.
#[derive(Debug, Clone)]
pub struct NewsArticle {
    pub parent_id: u32,
    pub flags: u32,
    pub date: HlDate,
    pub title: String,
    pub poster: String,
    /// This article's one stored MIME type, e.g. `"text/plain"`.
    pub flavor: String,
    pub data: Vec<u8>,
    /// Reply article IDs, oldest posted first.
    pub children: Vec<u32>,
}

/// The current time as an [`HlDate`]: `year(2) | milliseconds(2) |
/// seconds-since-midnight-Jan-1(4)`.
fn now_as_hl_date() -> HlDate {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let days = (secs / 86400) as i64;
    let secs_of_day = (secs % 86400) as u32;
    let (year, _month, _day) = civil_from_days(days);
    let days_since_jan1 = days - days_from_civil(year, 1, 1);
    HlDate {
        year: year as u16,
        milliseconds: now.subsec_millis() as u16,
        seconds: days_since_jan1 as u32 * 86400 + secs_of_day,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian `(year, month, day)`.
/// Howard Hinnant's `days_from_civil`, a standard, well known algorithm.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Inverse of [`days_from_civil`]: proleptic Gregorian `(year, month,
/// day)` for days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_from_civil_round_trips_through_civil_from_days() {
        for days in [0, 1, 365, 366, 10957, 19723, -1, -365] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days, "for {days} days");
        }
    }

    #[test]
    fn epoch_is_1970_01_01() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(days_from_civil(1970, 1, 1), 0);
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
            "Hi".to_string(),
            "alice".to_string(),
            0,
            "text/plain".to_string(),
            b"hello".to_vec(),
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
                "Top".to_string(),
                "alice".to_string(),
                0,
                "text/plain".to_string(),
                b"1".to_vec(),
            )
            .unwrap();
        let reply = root
            .post_article(
                &path,
                top,
                "Re: Top".to_string(),
                "bob".to_string(),
                0,
                "text/plain".to_string(),
                b"2".to_vec(),
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
                "x".to_string(),
                "alice".to_string(),
                0,
                "text/plain".to_string(),
                b"x".to_vec(),
            )
            .unwrap_err();
        assert_eq!(err, "no such parent article");
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
                "Top".to_string(),
                "a".to_string(),
                0,
                "text/plain".to_string(),
                b"1".to_vec(),
            )
            .unwrap();
        let reply = root
            .post_article(
                &path,
                top,
                "Re".to_string(),
                "b".to_string(),
                0,
                "text/plain".to_string(),
                b"2".to_vec(),
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
                "Top".to_string(),
                "a".to_string(),
                0,
                "text/plain".to_string(),
                b"1".to_vec(),
            )
            .unwrap();
        let first = root
            .post_article(
                &path,
                top,
                "First".to_string(),
                "b".to_string(),
                0,
                "text/plain".to_string(),
                b"2".to_vec(),
            )
            .unwrap();
        let second = root
            .post_article(
                &path,
                top,
                "Second".to_string(),
                "c".to_string(),
                0,
                "text/plain".to_string(),
                b"3".to_vec(),
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
