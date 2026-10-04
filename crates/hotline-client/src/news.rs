//! Threaded news requests.

use hotline_proto::types::{
    HlDate, HlPath, NewsArticleListData, NewsArticleSummary, NewsCategoryListEntry,
};
use hotline_proto::{Field, FieldId, Opcode};

use crate::client::HotlineClient;
use crate::error::Result;

/// The MIME type of a plain text article.
pub const TEXT_PLAIN: &str = "text/plain";

/// One threaded news article, from [`myTran_GetNewsArtData`] (400).
///
/// [`myTran_GetNewsArtData`]: hotline_proto::Opcode::GetNewsArtData
#[derive(Debug, Clone, Default)]
pub struct NewsArticle {
    /// The subject line.
    pub title: Box<str>,
    /// The name of the user who posted it.
    pub poster: Box<str>,
    /// When it was posted, if the server sent a date.
    pub date: Option<HlDate>,
    /// The article before this one, `0` for none.
    pub prev_id: u32,
    /// The article after this one, `0` for none.
    pub next_id: u32,
    /// The article this one replies to, `0` for none.
    pub parent_id: u32,
    /// The first reply, `0` for none.
    pub first_child_id: u32,
    /// The data in the requested flavor, empty if the article has no such flavor.
    pub data: Vec<u8>,
}

impl HotlineClient {
    /// List the bundles and categories in the bundle at `path`,
    /// [`myTran_GetNewsCatNameList`] (370).
    ///
    /// [`myTran_GetNewsCatNameList`]: hotline_proto::Opcode::GetNewsCatNameList
    pub async fn get_news_cat_name_list(
        &self,
        path: &HlPath,
    ) -> Result<Vec<NewsCategoryListEntry>> {
        let fields = self.news_path_field(path)?.into_iter().collect();
        let reply = self.request(Opcode::GetNewsCatNameList, fields).await?;
        let encoding = self.encoding().get();
        Ok(reply
            .fields(FieldId::NewsCatListData15)
            .map(|f| NewsCategoryListEntry::decode(f.as_bytes(), encoding))
            .collect::<hotline_proto::Result<_>>()?)
    }

    /// List the articles in the category at `path`, in thread order,
    /// [`myTran_GetNewsArtNameList`] (371).
    ///
    /// [`myTran_GetNewsArtNameList`]: hotline_proto::Opcode::GetNewsArtNameList
    pub async fn get_news_art_name_list(&self, path: &HlPath) -> Result<Vec<NewsArticleSummary>> {
        let fields = self.news_path_field(path)?.into_iter().collect();
        let reply = self.request(Opcode::GetNewsArtNameList, fields).await?;
        let encoding = self.encoding().get();
        match reply.field(FieldId::NewsArtListData) {
            Some(field) => {
                let listing = NewsArticleListData::decode(field.as_bytes(), encoding)?;
                Ok(listing.articles)
            }
            None => Ok(Vec::new()),
        }
    }

    /// Fetch the article `id` in the category at `path` in `flavor`,
    /// [`myTran_GetNewsArtData`] (400).
    ///
    /// [`myTran_GetNewsArtData`]: hotline_proto::Opcode::GetNewsArtData
    pub async fn get_news_art_data(
        &self,
        path: &HlPath,
        id: u32,
        flavor: &str,
    ) -> Result<NewsArticle> {
        let mut fields: Vec<_> = self.news_path_field(path)?.into_iter().collect();
        fields.push(Field::u32(FieldId::NewsArtID, id));
        fields.push(Field::string(FieldId::NewsArtDataFlav, flavor));
        let reply = self.request(Opcode::GetNewsArtData, fields).await?;
        let encoding = self.encoding().get();
        let text = |id| reply.string(id, encoding).unwrap_or_default().into();
        let article_id = |field| {
            reply
                .field(field)
                .and_then(Field::as_u32)
                .unwrap_or_default()
        };
        Ok(NewsArticle {
            title: text(FieldId::NewsArtTitle),
            poster: text(FieldId::NewsArtPoster),
            date: reply
                .field(FieldId::NewsArtDate)
                .and_then(|f| HlDate::decode(f.as_bytes()).ok()),
            prev_id: article_id(FieldId::NewsArtPrevArt),
            next_id: article_id(FieldId::NewsArtNextArt),
            parent_id: article_id(FieldId::NewsArtParentArt),
            first_child_id: article_id(FieldId::NewsArt1stChildArt),
            data: reply
                .field(FieldId::NewsArtData)
                .map(|f| f.as_bytes().to_vec())
                .unwrap_or_default(),
        })
    }

    /// Post a plain text article to the category at `path`, as a reply to
    /// `parent_id` or `0` for a new thread, [`myTran_PostNewsArt`] (410).
    ///
    /// [`myTran_PostNewsArt`]: hotline_proto::Opcode::PostNewsArt
    pub async fn post_news_art(
        &self,
        path: &HlPath,
        parent_id: u32,
        title: &str,
        text: &str,
    ) -> Result<()> {
        let mut fields: Vec<_> = self.news_path_field(path)?.into_iter().collect();
        fields.extend([
            Field::u32(FieldId::NewsArtID, parent_id),
            Field::string(FieldId::NewsArtTitle, title),
            Field::u32(FieldId::NewsArtFlags, 0),
            Field::string(FieldId::NewsArtDataFlav, TEXT_PLAIN),
            Field::string(FieldId::NewsArtData, text),
        ]);
        self.request(Opcode::PostNewsArt, fields).await?;
        Ok(())
    }

    /// Delete the article `id` in the category at `path`, and its replies if
    /// `recursive`, [`myTran_DelNewsArt`] (411).
    ///
    /// [`myTran_DelNewsArt`]: hotline_proto::Opcode::DelNewsArt
    pub async fn del_news_art(&self, path: &HlPath, id: u32, recursive: bool) -> Result<()> {
        let mut fields: Vec<_> = self.news_path_field(path)?.into_iter().collect();
        fields.push(Field::u32(FieldId::NewsArtID, id));
        fields.push(Field::u32(FieldId::NewsArtRecurseDel, u32::from(recursive)));
        self.request(Opcode::DelNewsArt, fields).await?;
        Ok(())
    }

    /// Delete the bundle or category at `path`, [`myTran_DelNewsItem`] (380).
    ///
    /// [`myTran_DelNewsItem`]: hotline_proto::Opcode::DelNewsItem
    pub async fn del_news_item(&self, path: &HlPath) -> Result<()> {
        let fields = self.news_path_field(path)?.into_iter().collect();
        self.request(Opcode::DelNewsItem, fields).await?;
        Ok(())
    }

    /// Create the bundle `name` in the bundle at `path`,
    /// [`myTran_NewNewsFldr`] (381).
    ///
    /// [`myTran_NewNewsFldr`]: hotline_proto::Opcode::NewNewsFldr
    pub async fn new_news_fldr(&self, path: &HlPath, name: &str) -> Result<()> {
        let mut fields: Vec<_> = self.news_path_field(path)?.into_iter().collect();
        fields.push(Field::string(FieldId::FileName, name));
        self.request(Opcode::NewNewsFldr, fields).await?;
        Ok(())
    }

    /// Create the category `name` in the bundle at `path`,
    /// [`myTran_NewNewsCat`] (382).
    ///
    /// [`myTran_NewNewsCat`]: hotline_proto::Opcode::NewNewsCat
    pub async fn new_news_cat(&self, path: &HlPath, name: &str) -> Result<()> {
        let mut fields: Vec<_> = self.news_path_field(path)?.into_iter().collect();
        fields.push(Field::string(FieldId::NewsCatName, name));
        self.request(Opcode::NewNewsCat, fields).await?;
        Ok(())
    }

    /// A [`myField_NewsPath`] (325) field for `path`, `None` at the root.
    ///
    /// [`myField_NewsPath`]: hotline_proto::FieldId::NewsPath
    fn news_path_field(&self, path: &HlPath) -> Result<Option<Field>> {
        self.path_field(FieldId::NewsPath, path)
    }
}
