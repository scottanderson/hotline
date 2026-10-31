//! Threaded news: a tree of bundles and categories, the article threads of
//! one category, and a preview of one article.

use hotline_client::{HotlineClient, TEXT_PLAIN};
use hotline_proto::types::{HlDate, HlPath, NewsArticleSummary, NewsCategoryListEntry};
use hotline_proto::{text_encoding, Access};

use super::ClientSession;
use crate::line::LineKind;

/// The kind of folder in threaded news.
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

    /// The icon of the folder in the tree.
    pub const fn icon(self) -> &'static str {
        match self {
            Self::Bundle => "\u{1f5de}\u{fe0f}",
            Self::Category => "\u{1f4f0}",
        }
    }
}

/// One row of the folder tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsFolderNode {
    pub kind: NewsFolder,
    /// The path of the folder, with its own name last.
    pub path: HlPath,
    /// The items in a bundle, or the articles in a category.
    pub count: u16,
    /// `true` for a bundle that shows its contents.
    pub expanded: bool,
}

impl NewsFolderNode {
    fn new(dir: &HlPath, entry: NewsCategoryListEntry) -> Self {
        let (kind, name, count) = match entry {
            NewsCategoryListEntry::Bundle { count, name } => (NewsFolder::Bundle, name, count),
            NewsCategoryListEntry::Category { count, name, .. } => {
                (NewsFolder::Category, name, count)
            }
        };
        let mut path = dir.clone();
        path.components.push(name.into());
        Self {
            kind,
            path,
            count,
            expanded: false,
        }
    }

    /// The folder's own name.
    pub fn name(&self) -> &str {
        self.path.components.last().map_or("", String::as_str)
    }

    /// How deep the folder is in the tree. 0 at the root.
    pub const fn depth(&self) -> usize {
        self.path.components.len().saturating_sub(1)
    }

    /// The cells of the row: the indented name with an icon, and the count.
    pub fn cells(&self) -> [String; 2] {
        let marker = match (self.kind, self.expanded) {
            (NewsFolder::Category, _) => " ",
            (NewsFolder::Bundle, false) => "\u{25b8}",
            (NewsFolder::Bundle, true) => "\u{25be}",
        };
        [
            format!(
                "{}{marker} {} {}",
                "  ".repeat(self.depth()),
                self.kind.icon(),
                self.name()
            ),
            self.count.to_string(),
        ]
    }
}

/// One article in the thread list, `depth` replies deep in its thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewsThread {
    pub summary: NewsArticleSummary,
    pub depth: usize,
}

impl NewsThread {
    /// The cells of the row: the indented title, the poster, and the date.
    pub fn cells(&self) -> [String; 3] {
        [
            format!("{}{}", "  ".repeat(self.depth), self.summary.title),
            self.summary.poster.to_string(),
            self.summary.date.to_string(),
        ]
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

/// A folder or an article, by its row in the tree or the thread list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsTarget {
    Folder(usize),
    Article(usize),
}

/// `articles` as a tree: each reply after the last descendant of its parent.
fn threaded(articles: Vec<NewsArticleSummary>) -> Vec<NewsThread> {
    let mut rows: Vec<NewsThread> = Vec::with_capacity(articles.len());
    for summary in articles {
        let parent = match summary.parent_id {
            0 => None,
            id => rows.iter().position(|row| row.summary.id == id),
        };
        let parent = parent.and_then(|p| rows.get(p).map(|row| (p, row.depth + 1)));
        let (at, depth) = parent.map_or((rows.len(), 0), |(p, depth)| {
            let end = rows.iter().skip(p + 1).position(|row| row.depth < depth);
            (end.map_or(rows.len(), |n| p + 1 + n), depth)
        });
        rows.insert(at, NewsThread { summary, depth });
    }
    rows
}

impl ClientSession {
    /// List the root of the tree, and close every bundle.
    pub async fn list_threaded_news(&mut self, client: &HotlineClient) {
        self.news_category = None;
        self.news_threads.clear();
        self.news_preview = None;
        let root = HlPath::default();
        match client.get_news_cat_name_list(&root).await {
            Ok(entries) => {
                let nodes = entries.into_iter().map(|e| NewsFolderNode::new(&root, e));
                self.news_folders = Some(nodes.collect());
            }
            Err(e) => {
                self.news_folders = Some(Vec::new());
                self.push(format!("failed to list news: {e}"), LineKind::Error);
            }
        }
    }

    fn news_folder(&self, index: usize) -> Option<&NewsFolderNode> {
        self.news_folders.as_ref()?.get(index)
    }

    /// Show the threads of the category at `index`. A bundle shows none.
    pub async fn highlight_news_folder(&mut self, client: &HotlineClient, index: usize) {
        let Some(node) = self.news_folder(index) else {
            return;
        };
        let category = (node.kind == NewsFolder::Category).then(|| node.path.clone());
        if category == self.news_category {
            return;
        }
        self.news_category = category;
        self.news_threads.clear();
        self.news_preview = None;
        self.list_news_threads(client).await;
    }

    /// List the threads of `news_category` again.
    async fn list_news_threads(&mut self, client: &HotlineClient) {
        let Some(path) = &self.news_category else {
            return;
        };
        match client.get_news_art_name_list(path).await {
            Ok(articles) => self.news_threads = threaded(articles),
            Err(e) => self.push(format!("failed to list articles: {e}"), LineKind::Error),
        }
    }

    /// Show the contents of the bundle at `index`, or hide them.
    pub async fn toggle_news_folder(&mut self, client: &HotlineClient, index: usize) {
        match self.news_folder(index) {
            Some(node) if node.kind == NewsFolder::Bundle && node.expanded => {
                self.collapse_news_folder(index);
            }
            Some(node) if node.kind == NewsFolder::Bundle => {
                self.expand_news_folder(client, index).await;
            }
            _ => {}
        }
    }

    /// Show the contents of the bundle at `index`.
    pub async fn expand_news_folder(&mut self, client: &HotlineClient, index: usize) {
        let Some(node) = self.news_folder(index) else {
            return;
        };
        if node.kind != NewsFolder::Bundle || node.expanded {
            return;
        }
        let path = node.path.clone();
        let entries = match client.get_news_cat_name_list(&path).await {
            Ok(entries) => entries,
            Err(e) => {
                self.push(format!("failed to list news: {e}"), LineKind::Error);
                return;
            }
        };
        let Some(folders) = &mut self.news_folders else {
            return;
        };
        let Some(node) = folders.get_mut(index) else {
            return;
        };
        node.expanded = true;
        let children = entries.into_iter().map(|e| NewsFolderNode::new(&path, e));
        folders.splice(index + 1..index + 1, children);
    }

    /// Hide the contents of the bundle at `index`.
    pub fn collapse_news_folder(&mut self, index: usize) {
        let Some(folders) = &mut self.news_folders else {
            return;
        };
        let Some(node) = folders.get_mut(index) else {
            return;
        };
        node.expanded = false;
        let depth = node.depth();
        let end = folders
            .iter()
            .skip(index + 1)
            .position(|child| child.depth() <= depth)
            .map_or(folders.len(), |n| index + 1 + n);
        folders.drain(index + 1..end);
    }

    /// The row of the bundle that holds the folder at `index`.
    pub fn news_folder_parent(&self, index: usize) -> Option<usize> {
        let folders = self.news_folders.as_ref()?;
        let depth = folders.get(index)?.depth();
        folders
            .get(..index)?
            .iter()
            .rposition(|node| node.depth() < depth)
    }

    /// The path of the folder at `index`.
    pub fn news_folder_path(&self, index: usize) -> Option<HlPath> {
        Some(self.news_folders.as_ref()?.get(index)?.path.clone())
    }

    /// The row of the folder at `path`.
    pub fn news_folder_row(&self, path: &HlPath) -> Option<usize> {
        self.news_folders
            .as_ref()?
            .iter()
            .position(|node| node.path == *path)
    }

    /// Show the article at `index` of the thread list in the preview.
    pub async fn preview_article(&mut self, client: &HotlineClient, index: usize) {
        let Some(summary) = self.news_threads.get(index).map(|t| t.summary.clone()) else {
            return;
        };
        if self
            .news_preview
            .as_ref()
            .is_some_and(|a| a.id == summary.id)
        {
            return;
        }
        self.news_preview = self.read_article(client, &summary).await;
    }

    /// Read the article of `summary` in `news_category`.
    pub async fn read_article(
        &mut self,
        client: &HotlineClient,
        summary: &NewsArticleSummary,
    ) -> Option<ArticleText> {
        let path = self.news_category.as_ref()?;
        match client.get_news_art_data(path, summary.id, TEXT_PLAIN).await {
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

    /// `true` with a category open. Elsewhere it shows an error line.
    pub fn can_post(&mut self) -> bool {
        if self.news_category.is_none() {
            self.push("select a category to post in it", LineKind::Error);
        }
        self.news_category.is_some()
    }

    /// Post an article to the open category as a reply to `parent_id`, or
    /// `0` for a new thread.
    pub async fn post_article(
        &mut self,
        client: &HotlineClient,
        parent_id: u32,
        title: &str,
        text: &str,
    ) {
        let Some(path) = &self.news_category else {
            return;
        };
        let posted = client
            .post_news_art(path, parent_id, title, &text_encoding::to_wire_lines(text))
            .await;
        match posted {
            Ok(()) => self.list_news_threads(client).await,
            Err(e) => self.push(format!("failed to post: {e}"), LineKind::Error),
        }
    }

    /// `true` if the user may delete the folder at `index`. A bundle needs
    /// the delete bundle and the delete category privileges. Else it shows
    /// an error line.
    pub fn may_delete_news_folder(&mut self, index: usize) -> bool {
        let bundle = self
            .news_folder(index)
            .is_some_and(|node| node.kind == NewsFolder::Bundle);
        let access = self.granted();
        if bundle && !access.contains(Access::NEWS_DELETE_FLDR) {
            self.push(
                "You are not allowed to delete news bundles.",
                LineKind::Error,
            );
            return false;
        }
        if !access.contains(Access::NEWS_DELETE_CAT) {
            self.push(
                "You are not allowed to delete news categories.",
                LineKind::Error,
            );
            return false;
        }
        true
    }

    /// The name of the folder or the title of the article at `target`.
    pub fn news_name(&self, target: NewsTarget) -> Option<&str> {
        match target {
            NewsTarget::Folder(index) => self.news_folder(index).map(NewsFolderNode::name),
            NewsTarget::Article(index) => {
                let thread = self.news_threads.get(index)?;
                Some(&thread.summary.title)
            }
        }
    }

    /// Delete the folder or the article at `target`. An article goes with
    /// its replies.
    pub async fn delete_news_item(&mut self, client: &HotlineClient, target: NewsTarget) {
        match target {
            NewsTarget::Folder(index) => {
                let Some(path) = self.news_folder(index).map(|node| node.path.clone()) else {
                    return;
                };
                if let Err(e) = client.del_news_item(&path).await {
                    self.push(format!("failed to delete: {e}"), LineKind::Error);
                    return;
                }
                self.collapse_news_folder(index);
                if let Some(folders) = &mut self.news_folders {
                    folders.remove(index);
                }
                let open = self.news_category.as_ref();
                if open.is_some_and(|open| open.components.starts_with(&path.components)) {
                    self.news_category = None;
                    self.news_threads.clear();
                    self.news_preview = None;
                }
            }
            NewsTarget::Article(index) => {
                let (Some(path), Some(thread)) =
                    (&self.news_category, self.news_threads.get(index))
                else {
                    return;
                };
                let id = thread.summary.id;
                if let Err(e) = client.del_news_art(path, id, true).await {
                    self.push(format!("failed to delete: {e}"), LineKind::Error);
                    return;
                }
                if self
                    .news_preview
                    .as_ref()
                    .is_some_and(|article| article.id == id)
                {
                    self.news_preview = None;
                }
                self.list_news_threads(client).await;
            }
        }
    }

    /// Create a `kind` called `name` in the bundle that the folder at
    /// `index` is, or is in. `None` creates it at the root.
    pub async fn new_news_item(
        &mut self,
        client: &HotlineClient,
        index: Option<usize>,
        name: &str,
        kind: NewsFolder,
    ) {
        let bundle = index.and_then(|index| {
            let node = self.news_folder(index)?;
            match node.kind {
                NewsFolder::Bundle => Some(index),
                NewsFolder::Category => self.news_folder_parent(index),
            }
        });
        let path = bundle
            .and_then(|index| self.news_folder(index))
            .map_or_else(HlPath::default, |node| node.path.clone());
        let created = match kind {
            NewsFolder::Category => client.new_news_cat(&path, name).await,
            NewsFolder::Bundle => client.new_news_fldr(&path, name).await,
        };
        if let Err(e) = created {
            self.push(format!("failed to create {name}: {e}"), LineKind::Error);
            return;
        }
        match bundle {
            Some(index) => {
                self.collapse_news_folder(index);
                self.expand_news_folder(client, index).await;
            }
            None => self.list_threaded_news(client).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::text_encoding::EncodingHandle;
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
        let titles: Vec<_> = items.iter().map(|item| item.cells()[0].clone()).collect();
        assert_eq!(titles, ["A1", "  A2", "    A3", "A4"]);
    }

    #[test]
    fn threaded_puts_each_reply_after_the_last_reply_to_its_parent() {
        let items = threaded(vec![
            summary(1, 0),
            summary(2, 1),
            summary(3, 0),
            summary(4, 1),
            summary(5, 9),
        ]);
        let rows: Vec<_> = items
            .iter()
            .map(|item| (item.summary.id, item.depth))
            .collect();
        assert_eq!(rows, [(1, 0), (2, 1), (4, 1), (3, 0), (5, 0)]);
    }

    #[test]
    fn a_folder_row_shows_its_depth_icon_and_count() {
        let node = NewsFolderNode {
            kind: NewsFolder::Bundle,
            path: HlPath::new(["Top", "Sub"]),
            count: 3,
            expanded: false,
        };
        let icon = NewsFolder::Bundle.icon();
        assert_eq!(
            node.cells(),
            [format!("  \u{25b8} {icon} Sub"), "3".to_string()]
        );
    }

    #[test]
    fn may_delete_news_folder_needs_both_privileges_for_a_bundle() {
        let mut session = ClientSession::new("me".into(), 0, EncodingHandle::default());
        session.server_version = Some(190);
        session.news_folders = Some(
            [NewsFolder::Bundle, NewsFolder::Category]
                .map(|kind| NewsFolderNode {
                    kind,
                    path: HlPath::new(["A"]),
                    count: 0,
                    expanded: false,
                })
                .into(),
        );
        session.access = Access::NEWS_DELETE_CAT;
        assert!(!session.may_delete_news_folder(0));
        assert!(session.may_delete_news_folder(1));
        session.access = Access::NEWS_DELETE_FLDR;
        assert!(!session.may_delete_news_folder(0));
        session.access |= Access::NEWS_DELETE_CAT;
        assert!(session.may_delete_news_folder(0));
    }

    #[tokio::test]
    async fn a_session_browses_posts_and_reads_threaded_news() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());

        session
            .new_news_item(&client, None, "Top", NewsFolder::Bundle)
            .await;
        session
            .new_news_item(&client, Some(0), "General", NewsFolder::Category)
            .await;
        let names = |session: &ClientSession| -> Vec<String> {
            let folders = session.news_folders.as_deref().unwrap_or_default();
            folders.iter().map(|node| node.name().to_string()).collect()
        };
        assert_eq!(names(&session), ["Top", "General"]);
        assert_eq!(session.news_folder_parent(1), Some(0));

        session.highlight_news_folder(&client, 1).await;
        assert_eq!(session.news_category, Some(HlPath::new(["Top", "General"])));
        session
            .post_article(&client, 0, "Hello", "line one\nline two")
            .await;
        session.preview_article(&client, 0).await;
        let article = session.news_preview.clone().unwrap();
        assert_eq!(&*article.title, "Hello");
        assert_eq!(article.text, "line one\rline two");

        session
            .delete_news_item(&client, NewsTarget::Article(0))
            .await;
        assert!(session.news_threads.is_empty());
        assert_eq!(session.news_preview, None);

        session.toggle_news_folder(&client, 0).await;
        assert_eq!(names(&session), ["Top"]);
        session.toggle_news_folder(&client, 0).await;
        assert_eq!(names(&session), ["Top", "General"]);

        session
            .delete_news_item(&client, NewsTarget::Folder(0))
            .await;
        assert_eq!(names(&session), Vec::<String>::new());
        assert_eq!(session.news_category, None);
    }

    #[tokio::test]
    async fn delete_news_item_of_the_open_bundle_leaves_a_row_to_select() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());

        session
            .new_news_item(&client, None, "Top", NewsFolder::Bundle)
            .await;
        session
            .new_news_item(&client, None, "Other", NewsFolder::Category)
            .await;
        let row = |session: &ClientSession, name: &str| {
            let folders = session.news_folders.as_deref().unwrap_or_default();
            folders.iter().position(|node| node.name() == name).unwrap()
        };
        let top = row(&session, "Top");
        session
            .new_news_item(&client, Some(top), "General", NewsFolder::Category)
            .await;
        let general = row(&session, "General");
        session.highlight_news_folder(&client, general).await;
        assert_eq!(session.news_category, Some(HlPath::new(["Top", "General"])));

        session
            .delete_news_item(&client, NewsTarget::Folder(top))
            .await;
        assert_eq!(session.news_category, None);
        let folders = session.news_folders.clone().unwrap_or_default();
        assert_eq!(folders.len(), 1);
        session.highlight_news_folder(&client, 0).await;
        assert_eq!(session.news_category, Some(folders[0].path.clone()));
    }

    #[tokio::test]
    async fn new_news_item_at_the_root_closes_the_open_category() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me".to_string(), 0, client.encoding());

        session
            .new_news_item(&client, None, "General", NewsFolder::Category)
            .await;
        session.highlight_news_folder(&client, 0).await;
        assert_eq!(session.news_category, Some(HlPath::new(["General"])));

        session
            .new_news_item(&client, None, "Other", NewsFolder::Category)
            .await;
        assert_eq!(session.news_category, None);
        let folders = session.news_folders.clone().unwrap_or_default();
        session.highlight_news_folder(&client, 0).await;
        assert_eq!(session.news_category, Some(folders[0].path.clone()));
    }
}
