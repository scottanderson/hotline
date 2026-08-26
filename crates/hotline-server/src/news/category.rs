use std::collections::HashMap;
use std::time::SystemTime;

use hotline_proto::types::HlDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::news::iso_date::{civil_from_days, days_from_civil};
use crate::news::NewsArticle;

/// A leaf category: a thread of articles plus the bookkeeping mirrored
/// into `myField_NewsCatListData15` (323).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewsCategory {
    pub guid: Uuid,
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

impl NewsCategory {
    pub(crate) fn new() -> Self {
        Self {
            guid: Uuid::new_v4(),
            add_sn: 1,
            del_sn: 1,
            next_article_id: 1,
            articles: HashMap::new(),
            roots: Vec::new(),
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

    pub(crate) fn add_article(
        &mut self,
        parent_id: u32,
        title: String,
        poster: String,
        flags: u32,
        mime_type: String,
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
                data: [(mime_type, data)].into(),
                children: Vec::new(),
            },
        );
        self.siblings_of_mut(parent_id).push(id);
        self.add_sn += 1;
        id
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

#[cfg(test)]
mod tests {
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
}
