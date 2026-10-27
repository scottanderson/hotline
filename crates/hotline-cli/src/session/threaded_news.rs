//! Threaded news: bundles, categories, and article threads.

use std::collections::HashMap;

use hotline_client::{HotlineClient, TEXT_PLAIN};
use hotline_proto::text_encoding;
use hotline_proto::types::{HlDate, NewsArticleSummary, NewsCategoryListEntry};

use super::ClientSession;
use crate::line::LineKind;

/// One row of the Threaded News tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewsItem {
    Bundle {
        name: Box<str>,
        count: u16,
    },
    Category {
        name: Box<str>,
        count: u16,
    },
    /// An article, `depth` replies deep in its thread.
    Article {
        summary: NewsArticleSummary,
        depth: usize,
    },
}

impl NewsItem {
    /// The text of the item's row.
    pub fn label(&self) -> String {
        match self {
            Self::Bundle { name, count } => format!("{name}/  ({count})"),
            Self::Category { name, count } => format!("{name}  ({count})"),
            Self::Article { summary, depth } => format!(
                "{}{}  ({})",
                "  ".repeat(*depth),
                summary.title,
                summary.poster
            ),
        }
    }

    /// The name to show when the item is deleted.
    pub fn name(&self) -> &str {
        match self {
            Self::Bundle { name, .. } | Self::Category { name, .. } => name,
            Self::Article { summary, .. } => &summary.title,
        }
    }
}

impl From<NewsCategoryListEntry> for NewsItem {
    fn from(entry: NewsCategoryListEntry) -> Self {
        match entry {
            NewsCategoryListEntry::Bundle { count, name } => Self::Bundle { name, count },
            NewsCategoryListEntry::Category { count, name, .. } => Self::Category { name, count },
        }
    }
}

/// An article to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArticleText {
    pub id: u32,
    pub title: Box<str>,
    pub poster: Box<str>,
    pub date: HlDate,
    pub text: String,
}

impl ArticleText {
    /// The heading of the open article: the title, the poster and the date.
    pub fn heading(&self) -> String {
        format!("{} ({}, {})", self.title, self.poster, self.date)
    }
}

/// `articles` in listing order, each with its depth in its thread.
fn threaded(articles: Vec<NewsArticleSummary>) -> Vec<NewsItem> {
    let mut depths = HashMap::new();
    articles
        .into_iter()
        .map(|summary| {
            let depth = match summary.parent_id {
                0 => 0,
                parent => depths.get(&parent).map_or(0, |d| d + 1),
            };
            depths.insert(summary.id, depth);
            NewsItem::Article { summary, depth }
        })
        .collect()
}

/// The kind of folder to create in threaded news.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsFolder {
    /// A bundle, which holds bundles and categories.
    Bundle,
    /// A category, which holds articles.
    Category,
}

impl NewsFolder {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Bundle => "bundle",
            Self::Category => "category",
        }
    }
}

impl ClientSession {
    /// List the bundle or category at `threaded_path`.
    pub async fn list_threaded_news(&mut self, client: &HotlineClient) {
        let path = &self.threaded_path;
        let listed = if self.threaded_in_category {
            client.get_news_art_name_list(path).await.map(threaded)
        } else {
            client
                .get_news_cat_name_list(path)
                .await
                .map(|entries| entries.into_iter().map(NewsItem::from).collect())
        };
        match listed {
            Ok(items) => self.threaded = Some(items),
            Err(e) => {
                self.threaded = Some(Vec::new());
                self.push(format!("failed to list news: {e}"), LineKind::Error);
            }
        }
    }

    /// Open the bundle or category at `index`, or read the article there.
    pub async fn open_news_item(
        &mut self,
        client: &HotlineClient,
        index: usize,
    ) -> Option<ArticleText> {
        match self.threaded.as_ref()?.get(index)?.clone() {
            NewsItem::Bundle { name, .. } => {
                self.threaded_path.components.push(name.into());
                self.list_threaded_news(client).await;
                None
            }
            NewsItem::Category { name, .. } => {
                self.threaded_path.components.push(name.into());
                self.threaded_in_category = true;
                self.list_threaded_news(client).await;
                None
            }
            NewsItem::Article { summary, .. } => {
                let read = client
                    .get_news_art_data(&self.threaded_path, summary.id, TEXT_PLAIN)
                    .await;
                match read {
                    Ok(article) => Some(ArticleText {
                        id: summary.id,
                        title: article.title,
                        poster: article.poster,
                        date: article.date.unwrap_or(summary.date),
                        text: text_encoding::decode(self.encoding.get(), &article.data),
                    }),
                    Err(e) => {
                        self.push(format!("failed to read the article: {e}"), LineKind::Error);
                        None
                    }
                }
            }
        }
    }

    /// List the bundle that holds the current one.
    pub async fn parent_news(&mut self, client: &HotlineClient) {
        if self.threaded_path.components.pop().is_some() {
            self.threaded_in_category = false;
            self.list_threaded_news(client).await;
        }
    }

    /// `true` in a category. Elsewhere it shows an error line.
    pub fn can_post(&mut self) -> bool {
        if !self.threaded_in_category {
            self.push("open a category to post in it", LineKind::Error);
        }
        self.threaded_in_category
    }

    /// Post an article to the current category as a reply to `parent_id`, or
    /// `0` for a new thread.
    pub async fn post_article(
        &mut self,
        client: &HotlineClient,
        parent_id: u32,
        title: &str,
        text: &str,
    ) {
        let posted = client
            .post_news_art(
                &self.threaded_path,
                parent_id,
                title,
                &text_encoding::to_wire_lines(text),
            )
            .await;
        match posted {
            Ok(()) => self.list_threaded_news(client).await,
            Err(e) => self.push(format!("failed to post: {e}"), LineKind::Error),
        }
    }

    /// Delete the item at `index`. An article goes with its replies.
    pub async fn delete_news_item(&mut self, client: &HotlineClient, index: usize) {
        let Some(item) = self.threaded.as_ref().and_then(|items| items.get(index)) else {
            return;
        };
        let deleted = match item {
            NewsItem::Article { summary, .. } => {
                client
                    .del_news_art(&self.threaded_path, summary.id, true)
                    .await
            }
            NewsItem::Bundle { name, .. } | NewsItem::Category { name, .. } => {
                let mut path = self.threaded_path.clone();
                path.components.push(name.to_string());
                client.del_news_item(&path).await
            }
        };
        match deleted {
            Ok(()) => self.list_threaded_news(client).await,
            Err(e) => self.push(format!("failed to delete: {e}"), LineKind::Error),
        }
    }

    /// Create a `kind` called `name` in the current bundle.
    pub async fn new_news_item(&mut self, client: &HotlineClient, name: &str, kind: NewsFolder) {
        let created = match kind {
            NewsFolder::Category => client.new_news_cat(&self.threaded_path, name).await,
            NewsFolder::Bundle => client.new_news_fldr(&self.threaded_path, name).await,
        };
        match created {
            Ok(()) => self.list_threaded_news(client).await,
            Err(e) => self.push(format!("failed to create {name}: {e}"), LineKind::Error),
        }
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::types::HlPath;

    use super::*;
    use crate::session::test_support;

    fn summary(id: u32, parent_id: u32) -> NewsArticleSummary {
        NewsArticleSummary {
            id,
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 0,
            },
            parent_id,
            flags: 0,
            title: format!("A{id}").into(),
            poster: "al".into(),
            flavors: Vec::new(),
        }
    }

    #[test]
    fn article_heading_shows_the_poster_and_the_date() {
        let article = ArticleText {
            id: 1,
            title: "Hi".into(),
            poster: "al".into(),
            date: HlDate {
                year: 2024,
                milliseconds: 0,
                seconds: 0,
            },
            text: String::new(),
        };
        let date = article.date.to_string();
        assert_eq!(article.heading(), format!("Hi (al, {date})"));
    }

    #[test]
    fn threaded_indents_each_reply_under_its_parent() {
        let items = threaded(vec![
            summary(1, 0),
            summary(2, 1),
            summary(3, 2),
            summary(4, 0),
        ]);
        let labels: Vec<_> = items.iter().map(NewsItem::label).collect();
        assert_eq!(
            labels,
            ["A1  (al)", "  A2  (al)", "    A3  (al)", "A4  (al)"]
        );
    }

    #[tokio::test]
    async fn a_session_browses_posts_and_reads_threaded_news() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());

        session
            .new_news_item(&client, "General", NewsFolder::Category)
            .await;
        assert_eq!(
            session.threaded.as_deref().unwrap(),
            [NewsItem::Category {
                name: "General".into(),
                count: 0
            }]
        );

        assert_eq!(session.open_news_item(&client, 0).await, None);
        assert_eq!(session.threaded_path, HlPath::new(["General"]));
        session
            .post_article(&client, 0, "Hello", "line one\nline two")
            .await;
        let article = session.open_news_item(&client, 0).await.unwrap();
        assert_eq!(&*article.title, "Hello");
        assert_eq!(article.text, "line one\rline two");

        session.delete_news_item(&client, 0).await;
        assert_eq!(session.threaded.as_deref().unwrap(), []);

        session.parent_news(&client).await;
        assert_eq!(session.threaded_path, HlPath::default());
        assert!(!session.threaded_in_category);
    }
}
