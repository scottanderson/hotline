//! Classic (flat) and threaded news handlers.

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::text_encoding;
use hotline_proto::types::HlPath;
use hotline_proto::FieldId;
use hotline_proto::{Field, Transaction};

use crate::flood::FloodEvent;
use crate::news::{body_from_wire, body_to_wire};
use crate::state::SharedState;

use super::connection::{Session, MAX_FLAT_NEWS_LEN};

impl Session {
    /// [`myTran_GetMsgs`] (101): read the classic (non-threaded) news.
    ///
    /// [`myTran_GetMsgs`]: hotline_proto::Opcode::GetMsgs
    pub(super) async fn on_get_msgs(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_READ_ART) {
            return vec![tx.reply_error(reason)];
        }
        let news = text_encoding::encode_truncated(
            self.encoding(),
            &state.news().await,
            MAX_FLAT_NEWS_LEN,
        );
        vec![tx.reply(vec![Field::bytes(FieldId::Data, news)])]
    }

    /// [`myTran_OldPostNews`] (103): post to the classic (non-threaded) news.
    ///
    /// [`myTran_OldPostNews`]: hotline_proto::Opcode::OldPostNews
    pub(super) async fn on_old_post_news(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::POST_NEWS_ART) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some((text, len)) = tx.string_len(FieldId::Data, encoding) else {
            return vec![tx.reply_error("missing post text")];
        };
        if len > 8192 {
            return vec![tx.reply_error("Your news post was not accepted because it was too big.")];
        }
        let entry = state.post_news(&self.name, &text).await;
        if !self
            .flood_banned(FloodEvent::Text(&self.data_bytes(tx)), state)
            .await
        {
            state
                .publish(Transaction::push(
                    Opcode::NewMsg,
                    vec![Field::string(FieldId::Data, entry)],
                ))
                .await;
        }
        vec![tx.reply(vec![])]
    }

    /// A [`myField_NewsPath`] (325) field, decoded, or root if absent.
    ///
    /// [`myField_NewsPath`]: hotline_proto::FieldId::NewsPath
    pub(super) fn news_path(&self, tx: &Transaction) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx.news_path(encoding) {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("bad path"),
            None => Ok(HlPath::default()),
        }
    }

    /// [`myTran_GetNewsCatNameList`] (370): list a threaded news bundle's own
    /// contents.
    ///
    /// [`myTran_GetNewsCatNameList`]: hotline_proto::Opcode::GetNewsCatNameList
    pub(super) async fn on_get_news_cat_name_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let entries = match state.news_list_dir(&path).await {
            Ok(entries) => entries,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let mut fields = Vec::with_capacity(entries.len());
        for entry in entries {
            match entry.encode(encoding) {
                Ok(bytes) => fields.push(Field::bytes(FieldId::NewsCatListData15, bytes)),
                Err(_) => return vec![tx.reply_error("news listing too large to encode")],
            }
        }
        vec![tx.reply(fields)]
    }

    /// [`myTran_GetNewsArtNameList`] (371): list a threaded news category's
    /// articles.
    ///
    /// [`myTran_GetNewsArtNameList`]: hotline_proto::Opcode::GetNewsArtNameList
    pub(super) async fn on_get_news_art_name_list(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let listing = match state.news_list_articles(&path, self.encoding()).await {
            Ok(listing) => listing,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let bytes = match listing.encode(self.encoding()) {
            Ok(bytes) => bytes,
            Err(_) => return vec![tx.reply_error("news listing too large to encode")],
        };
        vec![tx.reply(vec![Field::bytes(FieldId::NewsArtListData, bytes)])]
    }

    /// [`myTran_DelNewsItem`] (380): delete a threaded news bundle or category.
    ///
    /// [`myTran_DelNewsItem`]: hotline_proto::Opcode::DelNewsItem
    pub(super) async fn on_del_news_item(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_delete(&path, self.access).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_NewNewsFldr`] (381): create a threaded news bundle (folder).
    ///
    /// [`myTran_NewNewsFldr`]: hotline_proto::Opcode::NewNewsFldr
    pub(super) async fn on_new_news_fldr(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_CREATE_FLDR) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(name) = tx.string(FieldId::FileName, encoding) else {
            return vec![tx.reply_error("missing name")];
        };
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_mkdir(&path, &name).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_NewNewsCat`] (382): create a threaded news category.
    ///
    /// [`myTran_NewNewsCat`]: hotline_proto::Opcode::NewNewsCat
    pub(super) async fn on_new_news_cat(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_CREATE_CAT) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some(name) = tx.news_category_name(encoding) else {
            return vec![tx.reply_error("missing name")];
        };
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        if let Err(reason) = state.news_mkcategory(&path, &name).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_GetNewsArtData`] (400): fetch one threaded news article's data.
    ///
    /// [`myTran_GetNewsArtData`]: hotline_proto::Opcode::GetNewsArtData
    pub(super) async fn on_get_news_art_data(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_READ_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let Some(id) = tx.news_article_id() else {
            return vec![tx.reply_error("missing article id")];
        };
        let detail = match state.news_article(&path, id).await {
            Ok(detail) => detail,
            Err(reason) => return vec![tx.reply_error(reason)],
        };

        let mut fields = vec![
            Field::string(FieldId::NewsArtTitle, detail.title),
            Field::string(FieldId::NewsArtPoster, detail.poster),
            Field::bytes(FieldId::NewsArtDate, detail.date.encode().to_vec()),
            Field::u32(FieldId::NewsArtPrevArt, detail.prev_id),
            Field::u32(FieldId::NewsArtNextArt, detail.next_id),
            Field::u32(FieldId::NewsArtParentArt, detail.parent_id),
            Field::u32(FieldId::NewsArt1stChildArt, detail.first_child_id),
        ];
        // The reply always echoes back the requested flavor, but only
        // includes the article body when it matches what's stored.
        let encoding = self.encoding();
        let requested_flavor = tx.news_article_flavor(encoding).unwrap_or_default();
        fields.push(Field::string(
            FieldId::NewsArtDataFlav,
            requested_flavor.clone(),
        ));
        if let Some(data) = detail.data.get(&requested_flavor) {
            let data = body_to_wire(&requested_flavor, data, encoding);
            fields.push(Field::bytes(FieldId::NewsArtData, data));
        }
        vec![tx.reply(fields)]
    }

    /// [`myTran_PostNewsArt`] (410): post a threaded news article.
    ///
    /// [`myTran_PostNewsArt`]: hotline_proto::Opcode::PostNewsArt
    pub(super) async fn on_post_news_art(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::POST_NEWS_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let encoding = self.encoding();
        let Some(title) = tx.news_article_title(encoding) else {
            return vec![tx.reply_error("missing title")];
        };
        let Some(data) = tx.news_article_data(encoding) else {
            return vec![tx.reply_error("missing article data")];
        };
        let parent_id = tx.news_article_id().unwrap_or(0);
        let flags = tx.news_article_flags().unwrap_or(0);
        // Openline stores an empty-string flavor when the client omits
        // this field; a real client always sends it, so defaulting to a
        // real MIME type here instead is a deliberate divergence.
        let mime_type = tx
            .news_article_flavor(encoding)
            .unwrap_or_else(|| "text/plain".to_string());
        let data = body_from_wire(&mime_type, data, encoding);

        if let Err(reason) = state
            .news_post_article(
                &path,
                parent_id,
                title,
                self.name.to_string(),
                flags,
                mime_type,
                data,
            )
            .await
        {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }

    /// [`myTran_DelNewsArt`] (411): delete a threaded news article.
    ///
    /// [`myTran_DelNewsArt`]: hotline_proto::Opcode::DelNewsArt
    pub(super) async fn on_del_news_art(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_DELETE_ART) {
            return vec![tx.reply_error(reason)];
        }
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let Some(id) = tx.news_article_id() else {
            return vec![tx.reply_error("missing article id")];
        };
        let recursive = tx.news_article_recurse_delete();
        if let Err(reason) = state.news_delete_article(&path, id, recursive).await {
            return vec![tx.reply_error(reason)];
        }
        vec![tx.reply(vec![])]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RecipientMessage;
    use hotline_proto::opcode::TransactionType;

    use crate::session::test_support::*;

    #[tokio::test]
    async fn newsfile_round_trips_through_post_and_get() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();

        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Data, "hello news")],
        );
        session.on_old_post_news(&post, &state).await;

        let get = Transaction::request(Opcode::GetMsgs, 2, vec![]);
        let reply = session.on_get_msgs(&get, &state).await;
        let [reply] = reply.try_into().unwrap();
        let news = reply.string(FieldId::Data, encoding).unwrap();
        assert!(news.contains("hello news"));
        assert!(news.contains("From Alice"));
    }

    /// A board past a field's `u16` length sends its newest 65535 bytes.
    #[tokio::test]
    async fn get_msgs_clamps_a_board_past_the_field_limit() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();

        for i in 0..20 {
            let body = format!("{i}{}", "x".repeat(8000));
            let post = Transaction::request(
                Opcode::OldPostNews,
                1,
                vec![Field::string(FieldId::Data, body)],
            );
            session.on_old_post_news(&post, &state).await;
        }
        assert!(state.news().await.len() > MAX_FLAT_NEWS_LEN);

        let get = Transaction::request(Opcode::GetMsgs, 2, vec![]);
        let [reply] = session.on_get_msgs(&get, &state).await.try_into().unwrap();

        let data = reply.field(FieldId::Data).unwrap().as_bytes(encoding);
        assert_eq!(data.len(), MAX_FLAT_NEWS_LEN);
        // Posts are prepended, so the clamp keeps the newest.
        assert!(data.starts_with(b"From Alice"));
        assert!(data.windows(6).any(|w| w == b"19xxxx"));
        assert!(!data.windows(5).any(|w| w == b"0xxxx"));
        reply.encode(encoding).expect("clamped reply encodes");
    }

    #[tokio::test]
    async fn old_post_news_publishes_once() {
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Data, "hello news")],
        );
        session.on_old_post_news(&post, &state).await;

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a NewsfilePost push");
        assert_eq!(notify.r#type(), TransactionType::Push(Opcode::NewMsg));
        assert!(rx.try_recv().is_err(), "published only once");
    }

    #[tokio::test]
    async fn old_post_news_still_replies_to_the_post_that_bans() {
        let state = state_with_flood();
        let session = test_session("Alice", Access::all());
        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Data, "buy now")],
        );
        for _ in 0..5 {
            session.on_old_post_news(&post, &state).await;
        }
        let mut rx = state.register_recipient(session.user_id).await;

        let replies = session.on_old_post_news(&post, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(!replies[0].is_error());
        let msg = rx.try_recv().expect("a disconnect");
        assert!(msg.is_disconnect(), "the banning post is not published");
    }

    #[tokio::test]
    async fn old_post_news_rejects_oversized_text() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Data, "a".repeat(8193))],
        );
        let replies = session.on_old_post_news(&post, &state).await;

        assert_eq!(replies.len(), 1);
        assert!(replies[0].is_error());
        assert_eq!(
            replies[0].string(FieldId::ErrorText, encoding),
            Some("Your news post was not accepted because it was too big.".to_string())
        );
        assert!(state.news().await.is_empty(), "oversized post not stored");
        assert!(rx.try_recv().is_err(), "oversized post not published");
    }

    #[tokio::test]
    async fn new_news_fldr_and_new_news_cat_create_items_get_news_cat_name_list_can_see() {
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            Opcode::NewNewsFldr,
            1,
            vec![Field::string(FieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_new_news_fldr(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let mkcategory = Transaction::request(
            Opcode::NewNewsCat,
            2,
            vec![Field::string(FieldId::NewsCatName, "General")],
        );
        let [reply] = session
            .on_new_news_cat(&mkcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listdir = Transaction::request(Opcode::GetNewsCatNameList, 3, vec![]);
        let [reply] = session
            .on_get_news_cat_name_list(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 2);
    }

    #[tokio::test]
    async fn new_news_fldr_denied_without_news_create_fldr() {
        let state = SharedState::default();
        let mut access = Access::all();
        access.remove(Access::NEWS_CREATE_FLDR);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            Opcode::NewNewsFldr,
            1,
            vec![Field::string(FieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_new_news_fldr(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn del_news_item_removes_a_bundle_and_requires_news_delete_fldr() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());
        state
            .news_mkdir(&HlPath::default(), "Bundle")
            .await
            .unwrap();

        let mut denied_access = Access::all();
        denied_access.remove(Access::NEWS_DELETE_FLDR);
        let denied_session = test_session("Bob", denied_access);
        let delete = Transaction::request(
            Opcode::DelNewsItem,
            1,
            vec![Field::bytes(
                FieldId::NewsPath,
                HlPath::new(["Bundle"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = denied_session
            .on_del_news_item(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());

        let [reply] = session
            .on_del_news_item(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listdir = Transaction::request(Opcode::GetNewsCatNameList, 2, vec![]);
        let [reply] = session
            .on_get_news_cat_name_list(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(reply.fields.len(), 0);
    }

    #[tokio::test]
    async fn post_news_art_get_news_art_data_and_get_news_art_name_list_round_trip_an_article() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();
        let path_field = || {
            Field::bytes(
                FieldId::NewsPath,
                HlPath::new(["General"]).encode(encoding).unwrap(),
            )
        };

        let post = Transaction::request(
            Opcode::PostNewsArt,
            1,
            vec![
                path_field(),
                Field::string(FieldId::NewsArtTitle, "Hello"),
                Field::string(FieldId::NewsArtDataFlav, "text/plain"),
                Field::bytes(FieldId::NewsArtData, b"body text".to_vec()),
            ],
        );
        let [reply] = session
            .on_post_news_art(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listcategory = Transaction::request(Opcode::GetNewsArtNameList, 2, vec![path_field()]);
        let [reply] = session
            .on_get_news_art_name_list(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 1);

        let getthread = Transaction::request(
            Opcode::GetNewsArtData,
            3,
            vec![
                path_field(),
                Field::u32(FieldId::NewsArtID, 1),
                Field::string(FieldId::NewsArtDataFlav, "text/plain"),
            ],
        );
        let [reply] = session
            .on_get_news_art_data(&getthread, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let data = reply
            .field(FieldId::NewsArtData)
            .map(|f| f.as_bytes(encoding).into_owned());
        assert_eq!(data, Some(b"body text".to_vec()));
        assert_eq!(
            reply.string(FieldId::NewsArtTitle, encoding),
            Some("Hello".to_string())
        );

        let delete = Transaction::request(
            Opcode::DelNewsArt,
            4,
            vec![path_field(), Field::u32(FieldId::NewsArtID, 1)],
        );
        let [reply] = session
            .on_del_news_art(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listcategory = Transaction::request(Opcode::GetNewsArtNameList, 5, vec![path_field()]);
        let [reply] = session
            .on_get_news_art_name_list(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn post_news_art_stores_text_as_utf8_and_sends_it_in_each_encoding() {
        let encoding = text_encoding::DEFAULT;
        let utf8 = text_encoding::Encoding::for_label(b"utf-8").unwrap();
        let state = SharedState::default();
        let roman = test_session("Alice", Access::all());
        let unicode = test_session("Bob", Access::all());
        unicode.encoding.set(utf8);
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();
        let path = HlPath::new(["General"]);
        let path_field = || Field::bytes(FieldId::NewsPath, path.encode(encoding).unwrap());

        let post = Transaction::request(
            Opcode::PostNewsArt,
            1,
            vec![
                path_field(),
                Field::string(FieldId::NewsArtTitle, "Hello"),
                Field::string(FieldId::NewsArtDataFlav, "text/plain"),
                Field::bytes(FieldId::NewsArtData, b"caf\x8e".to_vec()),
            ],
        );
        let [reply] = roman
            .on_post_news_art(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let stored = state.news_article(&path, 1).await.unwrap();
        assert_eq!(stored.data["text/plain"], "caf\u{e9}".as_bytes());

        let listing = state.news_list_articles(&path, utf8).await.unwrap();
        assert_eq!(listing.articles[0].flavors[0].size, 5);

        let get = Transaction::request(
            Opcode::GetNewsArtData,
            2,
            vec![
                Field::bytes(FieldId::NewsPath, path.encode(utf8).unwrap()),
                Field::u32(FieldId::NewsArtID, 1),
                Field::string(FieldId::NewsArtDataFlav, "text/plain"),
            ],
        );
        let [reply] = unicode
            .on_get_news_art_data(&get, &state)
            .await
            .try_into()
            .unwrap();
        let data = reply.field(FieldId::NewsArtData).unwrap().as_bytes(utf8);
        assert_eq!(&*data, "caf\u{e9}".as_bytes());
    }

    #[tokio::test]
    async fn get_msgs_cuts_a_utf8_board_at_a_whole_character() {
        let utf8 = text_encoding::Encoding::for_label(b"utf-8").unwrap();
        let session = test_session("Alice", Access::all());
        session.encoding.set(utf8);
        let state = SharedState::default();

        for _ in 0..20 {
            let post = Transaction::request(
                Opcode::OldPostNews,
                1,
                vec![Field::string(FieldId::Data, "\u{e9}".repeat(4000))],
            );
            session.on_old_post_news(&post, &state).await;
        }

        let get = Transaction::request(Opcode::GetMsgs, 2, vec![]);
        let [reply] = session.on_get_msgs(&get, &state).await.try_into().unwrap();

        let data = reply.field(FieldId::Data).unwrap().as_bytes(utf8);
        assert!(data.len() <= MAX_FLAT_NEWS_LEN);
        assert!(std::str::from_utf8(&data).is_ok());
    }

    #[tokio::test]
    async fn post_news_art_denied_without_post_news_art() {
        let encoding = text_encoding::DEFAULT;
        let state = SharedState::default();
        let mut access = Access::all();
        access.remove(Access::POST_NEWS_ART);
        let session = test_session("Alice", access);
        state
            .news_mkcategory(&HlPath::default(), "General")
            .await
            .unwrap();

        let post = Transaction::request(
            Opcode::PostNewsArt,
            1,
            vec![
                Field::bytes(
                    FieldId::NewsPath,
                    HlPath::new(["General"]).encode(encoding).unwrap(),
                ),
                Field::string(FieldId::NewsArtTitle, "Hello"),
                Field::bytes(FieldId::NewsArtData, b"x".to_vec()),
            ],
        );
        let [reply] = session
            .on_post_news_art(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }
}
