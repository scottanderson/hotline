//! Threaded news: a tree of bundles and categories, the article threads of
//! one category, and a preview of one article.

use hotline_client::{HotlineClient, TEXT_PLAIN};
use hotline_proto::types::{HlDate, HlPath, NewsArticleSummary, NewsCategoryListEntry};
use hotline_proto::{text_encoding, Access};

use super::{ClientSession, Request};
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
            Self::Bundle => "\u{1f4da}",
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
        let parent = parent.and_then(|p| rows.get(p).map(|row| (p, row.depth.saturating_add(1))));
        let (at, depth) = parent.map_or((rows.len(), 0), |(p, depth)| {
            let after = p.saturating_add(1);
            let end = rows.iter().skip(after).position(|row| row.depth < depth);
            (end.map_or(rows.len(), |n| after.saturating_add(n)), depth)
        });
        rows.insert(at, NewsThread { summary, depth });
    }
    rows
}

impl ClientSession {
    /// List the root of the tree, and close every bundle.
    pub fn list_threaded_news(&mut self, client: &HotlineClient) -> Request<()> {
        self.news_category = None;
        self.news_threads.clear();
        self.news_preview = None;
        let client = client.clone();
        Request::new(async move {
            let root = HlPath::default();
            let entries = client.get_news_cat_name_list(&root).await;
            move |session: &mut Self| match entries {
                Ok(entries) => {
                    let nodes = entries.into_iter().map(|e| NewsFolderNode::new(&root, e));
                    session.news_folders = Some(nodes.collect());
                }
                Err(e) => {
                    session.news_folders = Some(Vec::new());
                    session.push(format!("failed to list news: {e}"), LineKind::Error);
                }
            }
        })
    }

    fn news_folder(&self, index: usize) -> Option<&NewsFolderNode> {
        self.news_folders.as_ref()?.get(index)
    }

    /// Show the threads of the category at `index`. A bundle shows none.
    pub fn highlight_news_folder(&mut self, client: &HotlineClient, index: usize) -> Request<()> {
        let Some(node) = self.news_folder(index) else {
            return Request::ready(());
        };
        let category = (node.kind == NewsFolder::Category).then(|| node.path.clone());
        if category == self.news_category {
            return Request::ready(());
        }
        self.news_category = category;
        self.news_threads.clear();
        self.news_preview = None;
        self.list_news_threads(client)
    }

    /// List the threads of `news_category` again.
    fn list_news_threads(&self, client: &HotlineClient) -> Request<()> {
        let Some(path) = self.news_category.clone() else {
            return Request::ready(());
        };
        let client = client.clone();
        Request::report(
            async move {
                client
                    .get_news_art_name_list(&path)
                    .await
                    .map(|articles| (articles, path))
            },
            "failed to list articles",
            |session: &mut Self, (articles, path)| {
                if session.news_category.as_ref() == Some(&path) {
                    session.news_threads = threaded(articles);
                }
            },
        )
    }

    /// Show the contents of the bundle at `index`, or hide them.
    pub fn toggle_news_folder(&mut self, client: &HotlineClient, index: usize) -> Request<()> {
        match self.news_folder(index) {
            Some(node) if node.kind == NewsFolder::Bundle && node.expanded => {
                self.collapse_news_folder(index);
                Request::ready(())
            }
            Some(node) if node.kind == NewsFolder::Bundle => self.expand_news_folder(client, index),
            _ => Request::ready(()),
        }
    }

    /// Show the contents of the bundle at `index`.
    pub fn expand_news_folder(&self, client: &HotlineClient, index: usize) -> Request<()> {
        let Some(node) = self.news_folder(index) else {
            return Request::ready(());
        };
        if node.kind != NewsFolder::Bundle || node.expanded {
            return Request::ready(());
        }
        let path = node.path.clone();
        let client = client.clone();
        Request::report(
            async move {
                client
                    .get_news_cat_name_list(&path)
                    .await
                    .map(|entries| (entries, path))
            },
            "failed to list news",
            |session: &mut Self, (entries, path)| session.add_news_children(&path, entries),
        )
    }

    /// Show `entries` under the closed bundle at `path`.
    fn add_news_children(&mut self, path: &HlPath, entries: Vec<NewsCategoryListEntry>) {
        let Some(index) = self.news_folder_row(path) else {
            return;
        };
        let Some(folders) = &mut self.news_folders else {
            return;
        };
        let Some(node) = folders.get_mut(index).filter(|node| !node.expanded) else {
            return;
        };
        node.expanded = true;
        let children = entries.into_iter().map(|e| NewsFolderNode::new(path, e));
        let after = index.saturating_add(1);
        folders.splice(after..after, children);
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
        let after = index.saturating_add(1);
        let end = folders
            .iter()
            .skip(after)
            .position(|child| child.depth() <= depth)
            .map_or(folders.len(), |n| after.saturating_add(n));
        folders.drain(after..end);
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
    pub fn preview_article(&self, client: &HotlineClient, index: usize) -> Request<()> {
        let Some(summary) = self.news_threads.get(index).map(|t| t.summary.clone()) else {
            return Request::ready(());
        };
        if self
            .news_preview
            .as_ref()
            .is_some_and(|a| a.id == summary.id)
        {
            return Request::ready(());
        }
        let category = self.news_category.clone();
        self.read_article(client, &summary)
            .map(move |session, article| {
                if session.news_category == category {
                    session.news_preview = article;
                }
            })
    }

    /// Read the article of `summary` in `news_category`.
    pub fn read_article(
        &self,
        client: &HotlineClient,
        summary: &NewsArticleSummary,
    ) -> Request<Option<ArticleText>> {
        let Some(path) = self.news_category.clone() else {
            return Request::ready(None);
        };
        let client = client.clone();
        let (id, date) = (summary.id, summary.date);
        Request::report(
            async move { client.get_news_art_data(&path, id, TEXT_PLAIN).await },
            "failed to read the article",
            move |session: &mut Self, article| {
                Some(ArticleText {
                    id,
                    title: article.title,
                    poster: article.poster,
                    date: article.date.unwrap_or(date),
                    text: text_encoding::decode(session.encoding.get(), &article.data),
                })
            },
        )
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
    pub fn post_article(
        &self,
        client: &HotlineClient,
        parent_id: u32,
        title: &str,
        text: &str,
    ) -> Request<()> {
        let Some(path) = self.news_category.clone() else {
            return Request::ready(());
        };
        let title = title.to_string();
        let text = text_encoding::to_wire_lines(text);
        Request::wait({
            let client = client.clone();
            async move { client.post_news_art(&path, parent_id, &title, &text).await }
        })
        .and_then({
            let client = client.clone();
            move |session, posted| match posted {
                Ok(()) => session.list_news_threads(&client),
                Err(e) => {
                    session.push(format!("failed to post: {e}"), LineKind::Error);
                    Request::ready(())
                }
            }
        })
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
    pub fn delete_news_item(&self, client: &HotlineClient, target: NewsTarget) -> Request<()> {
        match target {
            NewsTarget::Folder(index) => {
                let Some(path) = self.news_folder(index).map(|node| node.path.clone()) else {
                    return Request::ready(());
                };
                let client = client.clone();
                Request::report(
                    async move { client.del_news_item(&path).await.map(|()| path) },
                    "failed to delete",
                    |session: &mut Self, path| session.remove_news_folder(&path),
                )
            }
            NewsTarget::Article(index) => {
                let (Some(path), Some(thread)) =
                    (self.news_category.clone(), self.news_threads.get(index))
                else {
                    return Request::ready(());
                };
                let id = thread.summary.id;
                Request::wait({
                    let client = client.clone();
                    async move { client.del_news_art(&path, id, true).await }
                })
                .and_then({
                    let client = client.clone();
                    move |session, deleted| {
                        if let Err(e) = deleted {
                            session.push(format!("failed to delete: {e}"), LineKind::Error);
                            return Request::ready(());
                        }
                        if session
                            .news_preview
                            .as_ref()
                            .is_some_and(|article| article.id == id)
                        {
                            session.news_preview = None;
                        }
                        session.list_news_threads(&client)
                    }
                })
            }
        }
    }

    /// Take the folder at `path` out of the tree, with its contents.
    fn remove_news_folder(&mut self, path: &HlPath) {
        if let Some(index) = self.news_folder_row(path) {
            self.collapse_news_folder(index);
            if let Some(folders) = &mut self.news_folders {
                folders.remove(index);
            }
        }
        let open = self.news_category.as_ref();
        if open.is_some_and(|open| open.components.starts_with(&path.components)) {
            self.news_category = None;
            self.news_threads.clear();
            self.news_preview = None;
        }
    }

    /// Create a `kind` called `name` in the bundle that the folder at
    /// `index` is, or is in. `None` creates it at the root.
    pub fn new_news_item(
        &self,
        client: &HotlineClient,
        index: Option<usize>,
        name: &str,
        kind: NewsFolder,
    ) -> Request<()> {
        let bundle = index.and_then(|index| {
            let node = self.news_folder(index)?;
            match node.kind {
                NewsFolder::Bundle => Some(index),
                NewsFolder::Category => self.news_folder_parent(index),
            }
        });
        let path = bundle.and_then(|index| self.news_folder_path(index));
        let name = name.to_string();
        Request::wait({
            let client = client.clone();
            let path = path.clone().unwrap_or_default();
            async move {
                let created = match kind {
                    NewsFolder::Category => client.new_news_cat(&path, &name).await,
                    NewsFolder::Bundle => client.new_news_fldr(&path, &name).await,
                };
                (created, name)
            }
        })
        .and_then({
            let client = client.clone();
            move |session, (created, name)| {
                if let Err(e) = created {
                    session.push(format!("failed to create {name}: {e}"), LineKind::Error);
                    return Request::ready(());
                }
                match path.and_then(|path| session.news_folder_row(&path)) {
                    Some(index) => {
                        session.collapse_news_folder(index);
                        session.expand_news_folder(&client, index)
                    }
                    None => session.list_threaded_news(&client),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::text_encoding::EncodingHandle;
    use hotline_proto::types::HlPath;

    use super::*;
    use crate::session::test_support;

    #[test]
    fn icon_needs_no_variation_selector() {
        for folder in [NewsFolder::Bundle, NewsFolder::Category] {
            assert!(!folder.icon().contains('\u{fe0f}'));
        }
    }

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
        let mut session = ClientSession::new("me", 0, EncodingHandle::default());
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
        let mut session = ClientSession::new("me", 0, client.encoding());

        session
            .new_news_item(&client, None, "Top", NewsFolder::Bundle)
            .run(&mut session)
            .await;
        session
            .new_news_item(&client, Some(0), "General", NewsFolder::Category)
            .run(&mut session)
            .await;
        let names = |session: &ClientSession| -> Vec<String> {
            let folders = session.news_folders.as_deref().unwrap_or_default();
            folders.iter().map(|node| node.name().to_string()).collect()
        };
        assert_eq!(names(&session), ["Top", "General"]);
        assert_eq!(session.news_folder_parent(1), Some(0));

        session
            .highlight_news_folder(&client, 1)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, Some(HlPath::new(["Top", "General"])));
        session
            .post_article(&client, 0, "Hello", "line one\nline two")
            .run(&mut session)
            .await;
        session.preview_article(&client, 0).run(&mut session).await;
        let article = session.news_preview.clone().unwrap();
        assert_eq!(&*article.title, "Hello");
        assert_eq!(article.text, "line one\rline two");

        session
            .delete_news_item(&client, NewsTarget::Article(0))
            .run(&mut session)
            .await;
        assert!(session.news_threads.is_empty());
        assert_eq!(session.news_preview, None);

        session
            .toggle_news_folder(&client, 0)
            .run(&mut session)
            .await;
        assert_eq!(names(&session), ["Top"]);
        session
            .toggle_news_folder(&client, 0)
            .run(&mut session)
            .await;
        assert_eq!(names(&session), ["Top", "General"]);

        session
            .delete_news_item(&client, NewsTarget::Folder(0))
            .run(&mut session)
            .await;
        assert_eq!(names(&session), Vec::<String>::new());
        assert_eq!(session.news_category, None);
    }

    #[tokio::test]
    async fn delete_news_item_of_the_open_bundle_leaves_a_row_to_select() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());

        session
            .new_news_item(&client, None, "Top", NewsFolder::Bundle)
            .run(&mut session)
            .await;
        session
            .new_news_item(&client, None, "Other", NewsFolder::Category)
            .run(&mut session)
            .await;
        let row = |session: &ClientSession, name: &str| {
            let folders = session.news_folders.as_deref().unwrap_or_default();
            folders.iter().position(|node| node.name() == name).unwrap()
        };
        let top = row(&session, "Top");
        session
            .new_news_item(&client, Some(top), "General", NewsFolder::Category)
            .run(&mut session)
            .await;
        let general = row(&session, "General");
        session
            .highlight_news_folder(&client, general)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, Some(HlPath::new(["Top", "General"])));

        session
            .delete_news_item(&client, NewsTarget::Folder(top))
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, None);
        let folders = session.news_folders.clone().unwrap_or_default();
        assert_eq!(folders.len(), 1);
        session
            .highlight_news_folder(&client, 0)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, Some(folders[0].path.clone()));
    }

    #[tokio::test]
    async fn new_news_item_at_the_root_closes_the_open_category() {
        let addr = test_support::start_server(hotline_server::Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());

        session
            .new_news_item(&client, None, "General", NewsFolder::Category)
            .run(&mut session)
            .await;
        session
            .highlight_news_folder(&client, 0)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, Some(HlPath::new(["General"])));

        session
            .new_news_item(&client, None, "Other", NewsFolder::Category)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, None);
        let folders = session.news_folders.clone().unwrap_or_default();
        session
            .highlight_news_folder(&client, 0)
            .run(&mut session)
            .await;
        assert_eq!(session.news_category, Some(folders[0].path.clone()));
    }
}
