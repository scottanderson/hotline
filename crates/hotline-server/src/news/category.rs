use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::SystemTime;

use hotline_proto::types::HlDate;
use serde::de::Error as _;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

use crate::news::flavor_map::{self, FlavorMapRef};
use crate::news::iso_date;
use crate::news::{NewsArticle, PostError};

/// Longest reply chain, our own limit. Keeps a saved thread within
/// serde_json's nesting limit.
pub(crate) const MAX_REPLY_CHAIN_LEN: usize = 32;

/// A leaf category: a thread of articles plus the bookkeeping mirrored
/// into [`myField_NewsCatListData15`] (323).
///
/// [`myField_NewsCatListData15`]: hotline_proto::FieldId::NewsCatListData15
#[derive(Debug, Clone)]
pub struct NewsCategory {
    /// The category's unique ID.
    pub guid: Uuid,
    /// Incremented by one each time an article is added, and wraps.
    pub add_sn: u32,
    /// Always `1`: openline's own `delSN` never actually updates past its
    /// initial value once a category is created.
    pub del_sn: u32,
    /// The ID of the next article, or 0 after the category gave out
    /// `u32::MAX` articles.
    next_article_id: u32,
    /// Every article, keyed by ID.
    pub articles: HashMap<u32, NewsArticle>,
    /// Top-level (`parent_id == 0`) article IDs, oldest posted first.
    pub roots: Vec<u32>,
}

impl NewsCategory {
    pub(crate) fn new() -> Self {
        Self::from_articles(Uuid::new_v4(), 1, HashMap::new(), Vec::new())
    }

    /// A category that holds `articles`, with `roots` at the top level.
    pub(crate) const fn from_articles(
        guid: Uuid,
        next_article_id: u32,
        articles: HashMap<u32, NewsArticle>,
        roots: Vec<u32>,
    ) -> Self {
        Self {
            guid,
            // add_sn always equals next_article_id; see add_article.
            add_sn: next_article_id,
            del_sn: 1,
            next_article_id,
            articles,
            roots,
        }
    }

    /// This article's sibling list: `roots` for a top-level article, or
    /// its parent's `children` for a reply.
    pub(crate) fn siblings_of(&self, id: u32) -> &[u32] {
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

    /// Number of ancestors `id` has (0 for a root article). `id` must
    /// already exist in `self.articles`.
    pub(crate) fn depth_of(&self, mut id: u32) -> usize {
        let mut depth = 0_usize;
        while let Some(article) = self.articles.get(&id) {
            if article.parent_id == 0 {
                break;
            }
            depth = depth.saturating_add(1);
            id = article.parent_id;
        }
        depth
    }

    fn siblings_of_mut(&mut self, parent_id: u32) -> Option<&mut Vec<u32>> {
        if parent_id == 0 {
            Some(&mut self.roots)
        } else {
            self.articles
                .get_mut(&parent_id)
                .map(|parent| &mut parent.children)
        }
    }

    /// Add an article under `parent_id`, or at the top level when no
    /// article has that ID.
    pub(crate) fn add_article(
        &mut self,
        mut parent_id: u32,
        title: Box<str>,
        poster: Box<str>,
        flags: u32,
        mime_type: Box<str>,
        data: Box<[u8]>,
    ) -> Result<u32, PostError> {
        let id = self.next_article_id;
        if id == 0 {
            return Err(PostError::NoArticleId);
        }
        let siblings = match self.articles.get_mut(&parent_id) {
            Some(parent) => &mut parent.children,
            None => {
                parent_id = 0;
                &mut self.roots
            }
        };
        siblings.push(id);
        self.next_article_id = id.wrapping_add(1);
        self.articles.insert(
            id,
            NewsArticle {
                parent_id,
                flags,
                date: HlDate::from(SystemTime::now()),
                title,
                poster,
                data: [(mime_type, data)].into(),
                children: Vec::new(),
            },
        );
        self.add_sn = self.add_sn.wrapping_add(1);
        Ok(id)
    }

    pub(crate) fn delete_article(&mut self, id: u32, recursive: bool) -> Result<(), &'static str> {
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

        // The first child takes the article's place and adopts the other children.
        let Some((&first_child, rest)) = children.split_first() else {
            self.remove_from_siblings(parent_id, id);
            self.articles.remove(&id);
            return Ok(());
        };
        if let Some(slot) = self
            .siblings_of_mut(parent_id)
            .and_then(|siblings| siblings.iter_mut().find(|sid| **sid == id))
        {
            *slot = first_child;
        }

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
        if let Some(siblings) = self.siblings_of_mut(parent_id) {
            siblings.retain(|&sid| sid != id);
        }
    }

    pub(crate) fn push_article_and_children<'a>(
        &'a self,
        id: u32,
        out: &mut Vec<(u32, &'a NewsArticle)>,
    ) {
        if let Some(article) = self.articles.get(&id) {
            out.push((id, article));
            for &child in &article.children {
                self.push_article_and_children(child, out);
            }
        }
    }
}

/// A category's JSON shape nests each article under its parent, not by
/// id link.
impl Serialize for NewsCategory {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("NewsCategory", 3)?;
        s.serialize_field("guid", &self.guid)?;
        s.serialize_field("next_article_id", &self.next_article_id)?;
        let roots: Vec<_> = self
            .roots
            .iter()
            .filter_map(|&id| self.node_ref(id))
            .collect();
        s.serialize_field("articles", &roots)?;
        s.end()
    }
}

impl NewsCategory {
    fn node_ref(&self, id: u32) -> Option<ArticleNodeRef<'_>> {
        self.articles.get(&id).map(|article| ArticleNodeRef {
            id,
            article,
            category: self,
        })
    }
}

/// One article plus its nested replies, borrowed from a [`NewsCategory`]
/// for serialization.
struct ArticleNodeRef<'a> {
    id: u32,
    article: &'a NewsArticle,
    category: &'a NewsCategory,
}

impl Serialize for ArticleNodeRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut s = serializer.serialize_struct("NewsArticle", 7)?;
        s.serialize_field("id", &self.id)?;
        match self.article.flags {
            0 => s.skip_field("flags")?,
            flags => s.serialize_field("flags", &flags)?,
        }
        s.serialize_field("date", &iso_date::to_string(&self.article.date))?;
        s.serialize_field("title", &self.article.title)?;
        s.serialize_field("poster", &self.article.poster)?;
        s.serialize_field("data", &FlavorMapRef(&self.article.data))?;
        let children: Vec<_> = self
            .article
            .children
            .iter()
            .filter_map(|&id| self.category.node_ref(id))
            .collect();
        match children.is_empty() {
            true => s.skip_field("children")?,
            false => s.serialize_field("children", &children)?,
        }
        s.end()
    }
}

/// One article node read back from JSON, nested under its parent.
#[derive(Deserialize)]
struct ArticleNode {
    id: u32,
    #[serde(default)]
    flags: u32,
    #[serde(with = "iso_date")]
    date: HlDate,
    title: Box<str>,
    poster: Box<str>,
    #[serde(with = "flavor_map")]
    data: BTreeMap<Box<str>, Box<[u8]>>,
    #[serde(default)]
    children: Vec<ArticleNode>,
}

/// Stores `node` and its descendants in `out`, setting each `parent_id`
/// from the nesting. Returns `node`'s own id.
///
/// Rejects an id reused elsewhere in the tree.
fn flatten(
    node: ArticleNode,
    parent_id: u32,
    seen: &mut HashSet<u32>,
    out: &mut HashMap<u32, NewsArticle>,
) -> Result<u32, &'static str> {
    let id = node.id;
    if !seen.insert(id) {
        return Err("duplicate article id in news data");
    }
    let children = node
        .children
        .into_iter()
        .map(|child| flatten(child, id, seen, out))
        .collect::<Result<Vec<u32>, _>>()?;
    out.insert(
        id,
        NewsArticle {
            parent_id,
            flags: node.flags,
            date: node.date,
            title: node.title,
            poster: node.poster,
            data: node.data,
            children,
        },
    );
    Ok(id)
}

impl<'de> Deserialize<'de> for NewsCategory {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            guid: Uuid,
            next_article_id: u32,
            articles: Vec<ArticleNode>,
        }

        let raw = Raw::deserialize(deserializer)?;
        let mut articles = HashMap::new();
        let mut seen = HashSet::new();
        let roots = raw
            .articles
            .into_iter()
            .map(|node| flatten(node, 0, &mut seen, &mut articles))
            .collect::<Result<Vec<u32>, _>>()
            .map_err(D::Error::custom)?;
        Ok(NewsCategory::from_articles(
            raw.guid,
            raw.next_article_id,
            articles,
            roots,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn a_guid_serializes_as_a_hyphenated_uuid_string() {
        let mut category = NewsCategory::new();
        category.guid = Uuid::from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 255]);

        let json = serde_json::to_value(&category).unwrap();

        assert_eq!(
            json["guid"], "00010203-0405-0607-0809-0a0b0c0d0eff",
            "{json}"
        );
        let round_tripped: NewsCategory = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.guid, category.guid);
    }

    #[test]
    fn new_categories_get_a_random_uuid_v4() {
        let a = NewsCategory::new();
        let b = NewsCategory::new();

        assert_ne!(a.guid, b.guid);
        assert_eq!(a.guid.get_version_num(), 4);
        assert_eq!(b.guid.get_version_num(), 4);
    }

    #[test]
    fn add_article_gives_out_u32_max_and_then_no_id() {
        let mut category =
            NewsCategory::from_articles(Uuid::new_v4(), u32::MAX, HashMap::new(), vec![]);
        let mut post = || category.add_article(0, "".into(), "".into(), 0, "".into(), [].into());

        assert_eq!(post(), Ok(u32::MAX));
        assert_eq!(post(), Err(PostError::NoArticleId));
        assert_eq!(category.add_sn, 0);
        assert_eq!(category.roots, [u32::MAX]);
    }

    #[test]
    fn a_reply_nests_under_its_parent_in_json_instead_of_linking_by_id() {
        let mut category = NewsCategory::new();
        let root = category
            .add_article(
                0,
                "Root".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"hi".as_slice().into(),
            )
            .unwrap();
        category
            .add_article(
                root,
                "Reply".into(),
                "bob".into(),
                0,
                "text/plain".into(),
                b"hey".as_slice().into(),
            )
            .unwrap();

        let json = serde_json::to_value(&category).unwrap();

        let root_json = &json["articles"][0];
        assert_eq!(root_json["id"], root);
        assert_eq!(root_json["title"], "Root");
        assert_eq!(root_json["children"][0]["title"], "Reply");
        // No field links one article to another by id.
        assert!(root_json.get("parent_id").is_none(), "{json}");
        assert!(
            root_json["children"][0].get("parent_id").is_none(),
            "{json}"
        );
    }

    #[test]
    fn a_thread_round_trips_through_the_nested_json_shape() {
        let mut category = NewsCategory::new();
        let root = category
            .add_article(
                0,
                "Root".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"hi".as_slice().into(),
            )
            .unwrap();
        let reply = category
            .add_article(
                root,
                "Reply".into(),
                "bob".into(),
                0,
                "text/plain".into(),
                b"hey".as_slice().into(),
            )
            .unwrap();

        let json = serde_json::to_value(&category).unwrap();
        let round_tripped: NewsCategory = serde_json::from_value(json).unwrap();

        assert_eq!(round_tripped.roots, vec![root]);
        assert_eq!(round_tripped.articles[&root].children, vec![reply]);
        assert_eq!(round_tripped.articles[&reply].parent_id, root);
        assert_eq!(&*round_tripped.articles[&reply].title, "Reply");
    }

    #[test]
    fn add_sn_and_del_sn_are_recovered_from_next_article_id_not_stored() {
        let mut category = NewsCategory::new();
        category
            .add_article(
                0,
                "Root".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"hi".as_slice().into(),
            )
            .unwrap();

        let json = serde_json::to_value(&category).unwrap();

        assert!(json.get("add_sn").is_none(), "{json}");
        assert!(json.get("del_sn").is_none(), "{json}");
        let round_tripped: NewsCategory = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.add_sn, category.add_sn);
        assert_eq!(round_tripped.del_sn, category.del_sn);
    }

    #[test]
    fn a_leaf_article_with_no_flags_omits_both_from_json() {
        let mut category = NewsCategory::new();
        category
            .add_article(
                0,
                "Root".into(),
                "alice".into(),
                0,
                "text/plain".into(),
                b"hi".as_slice().into(),
            )
            .unwrap();

        let json = serde_json::to_value(&category).unwrap();

        let root_json = &json["articles"][0];
        assert!(root_json.get("flags").is_none(), "{json}");
        assert!(root_json.get("children").is_none(), "{json}");
        let round_tripped: NewsCategory = serde_json::from_value(json).unwrap();
        let article = round_tripped.articles.values().next().unwrap();
        assert_eq!(article.flags, 0);
        assert!(article.children.is_empty());
    }

    #[test]
    fn a_flagged_article_with_a_reply_keeps_both_in_json() {
        let mut category = NewsCategory::new();
        let root = category
            .add_article(
                0,
                "Root".into(),
                "alice".into(),
                7,
                "text/plain".into(),
                b"hi".as_slice().into(),
            )
            .unwrap();
        category
            .add_article(
                root,
                "Reply".into(),
                "bob".into(),
                0,
                "text/plain".into(),
                b"hey".as_slice().into(),
            )
            .unwrap();

        let json = serde_json::to_value(&category).unwrap();

        let root_json = &json["articles"][0];
        assert_eq!(root_json["flags"], 7);
        assert_eq!(root_json["children"][0]["title"], "Reply");
        let round_tripped: NewsCategory = serde_json::from_value(json).unwrap();
        assert_eq!(round_tripped.articles[&root].flags, 7);
        assert_eq!(round_tripped.articles[&root].children, vec![root + 1]);
    }

    #[test]
    fn a_duplicate_article_id_is_rejected_on_load() {
        let json = serde_json::json!({
            "guid": Uuid::new_v4(),
            "next_article_id": 3,
            "articles": [
                {
                    "id": 1,
                    "date": "2024-01-01T00:00:00.000Z",
                    "title": "Root",
                    "poster": "alice",
                    "data": {"text/plain": ""},
                    "children": [
                        {
                            "id": 1,
                            "date": "2024-01-01T00:00:00.000Z",
                            "title": "Reply",
                            "poster": "bob",
                            "data": {"text/plain": ""},
                        },
                    ],
                },
            ],
        });

        let result: Result<NewsCategory, _> = serde_json::from_value(json);

        let err = result
            .expect_err("duplicate id must be rejected")
            .to_string();
        assert!(err.contains("duplicate article id"), "{err}");
    }

    /// Raw JSON text, not `serde_json::Value`: only the text parser has
    /// the recursion cap the real load path relies on.
    #[test]
    fn deeply_nested_json_text_fails_to_parse_instead_of_overflowing_the_stack() {
        let depth = 1000;
        let mut json = String::new();
        for id in 0..depth {
            write!(
                json,
                "{{\"id\":{id},\"date\":\"2024-01-01T00:00:00.000Z\",\
                 \"title\":\"x\",\"poster\":\"alice\",\
                 \"data\":{{\"text/plain\":\"\"}},\"children\":["
            )
            .unwrap();
        }
        write!(
            json,
            "{{\"id\":{depth},\"date\":\"2024-01-01T00:00:00.000Z\",\
             \"title\":\"x\",\"poster\":\"alice\",\"data\":{{\"text/plain\":\"\"}}}}"
        )
        .unwrap();
        json.push_str(&"]}".repeat(depth as usize));
        let json = format!(
            "{{\"guid\":\"{}\",\"next_article_id\":{},\"articles\":[{json}]}}",
            Uuid::new_v4(),
            depth + 1,
        );

        let result: Result<NewsCategory, _> = serde_json::from_str(&json);

        let err = result.expect_err("nesting must exceed the parser's cap");
        assert!(
            err.to_string().contains("recursion limit exceeded"),
            "{err}"
        );
    }
}
