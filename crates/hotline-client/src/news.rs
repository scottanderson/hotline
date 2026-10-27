//! Threaded news requests.

use hotline_proto::types::{
    HlDate, HlPath, NewsArticleListData, NewsArticleSummary, NewsCategoryListEntry,
};
use hotline_proto::{Field, FieldId, Opcode, Received, Transaction};

use crate::client::HotlineClient;
use crate::error::Result;

/// The entries of a [`myTran_GetNewsCatNameList`] (370) reply. A reply whose
/// first field is not [`myField_NewsCatListData15`] (323) lists
/// [`myField_NewsCatListData`] (320) entries.
///
/// [`myTran_GetNewsCatNameList`]: hotline_proto::Opcode::GetNewsCatNameList
/// [`myField_NewsCatListData15`]: hotline_proto::FieldId::NewsCatListData15
/// [`myField_NewsCatListData`]: hotline_proto::FieldId::NewsCatListData
fn category_entries(
    reply: &Transaction<Received>,
) -> hotline_proto::Result<Vec<NewsCategoryListEntry>> {
    if reply
        .fields
        .first()
        .is_some_and(|f| f.id == FieldId::NewsCatListData15)
    {
        return reply
            .fields(FieldId::NewsCatListData15)
            .map(|f| NewsCategoryListEntry::decode(f.as_bytes(), f.encoding()))
            .collect();
    }
    let entries = reply
        .fields(FieldId::NewsCatListData)
        .map(|f| NewsCategoryListEntry::decode_old(f.as_bytes(), f.encoding()))
        .collect::<hotline_proto::Result<Vec<_>>>()?;
    Ok(entries.into_iter().flatten().collect())
}

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
        Ok(category_entries(&reply)?)
    }

    /// List the articles in the category at `path`, in thread order,
    /// [`myTran_GetNewsArtNameList`] (371).
    ///
    /// [`myTran_GetNewsArtNameList`]: hotline_proto::Opcode::GetNewsArtNameList
    pub async fn get_news_art_name_list(&self, path: &HlPath) -> Result<Vec<NewsArticleSummary>> {
        let fields = self.news_path_field(path)?.into_iter().collect();
        let reply = self.request(Opcode::GetNewsArtNameList, fields).await?;
        match reply.field(FieldId::NewsArtListData) {
            Some(field) => {
                let listing = NewsArticleListData::decode(field.as_bytes(), field.encoding())?;
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
        let text = |id| reply.string(id).unwrap_or_default().into();
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
                .map(Field::as_bytes)
                .and_then(<[u8]>::first_chunk)
                .copied()
                .map(HlDate::decode),
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

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::text_encoding;

    fn reply(fields: Vec<Field>) -> Transaction<Received> {
        Transaction::request(Opcode::GetNewsCatNameList, 1, vec![])
            .received(text_encoding::DEFAULT)
            .reply(fields)
            .received(text_encoding::DEFAULT)
    }

    #[test]
    fn category_entries_reads_the_old_field_without_a_323_first() {
        let entries = category_entries(&reply(vec![
            Field::bytes(FieldId::NewsCatListData, b"\x01Folder".to_vec()),
            Field::bytes(FieldId::NewsCatListData, b"\xffReadMe".to_vec()),
            Field::bytes(FieldId::NewsCatListData, b"\x0aGeneral".to_vec()),
        ]))
        .unwrap();
        let names: Vec<_> = entries.iter().map(NewsCategoryListEntry::name).collect();
        assert_eq!(names, ["Folder", "General"]);
    }

    #[test]
    fn category_entries_reads_only_323_when_it_comes_first() {
        let category = NewsCategoryListEntry::Bundle {
            count: 1,
            name: "New".into(),
        };
        let entries = category_entries(&reply(vec![
            Field::bytes(
                FieldId::NewsCatListData15,
                category.encode(text_encoding::DEFAULT).unwrap(),
            ),
            Field::bytes(FieldId::NewsCatListData, b"\x01Old".to_vec()),
        ]))
        .unwrap();
        assert_eq!(entries, [category]);
    }
}
