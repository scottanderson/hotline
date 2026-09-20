//! Classic (flat) and threaded news handlers.

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::opcode::Opcode;
use hotline_proto::text_encoding;
use hotline_proto::types::HlPath;
use hotline_proto::FieldId;
use hotline_proto::{Field, Transaction};

use crate::state::SharedState;

use super::connection::{Session, MAX_FLAT_NEWS_LEN};

impl Session {
    /// 101: read the classic (non-threaded) news.
    pub(super) async fn on_newsfile_get(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::NEWS_READ_ART) {
            return vec![tx.reply_error(reason)];
        }
        let mut news = text_encoding::encode(self.encoding(), &state.news().await);
        news.truncate(MAX_FLAT_NEWS_LEN);
        vec![tx.reply(vec![Field::bytes(FieldId::Text, news)])]
    }

    /// 103: post to the classic (non-threaded) news.
    pub(super) async fn on_newsfile_post(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        if let Err(reason) = self.access.require(Access::POST_NEWS_ART) {
            return vec![tx.reply_error(reason)];
        }
        let encoding = self.encoding();
        let Some((text, len)) = tx.string_len(FieldId::Text, encoding) else {
            return vec![tx.reply_error("missing post text")];
        };
        if len > 8192 {
            return vec![tx.reply_error("Your news post was not accepted because it was too big.")];
        }
        let entry = state.post_news(&self.name, &text).await;

        state
            .publish(Transaction::push(
                Opcode::NewMsg,
                vec![Field::string(FieldId::Text, entry)],
            ))
            .await;
        vec![tx.reply(vec![])]
    }

    /// A `myField_NewsPath` (325) field, decoded, or root if absent.
    pub(super) fn news_path(&self, tx: &Transaction) -> std::result::Result<HlPath, &'static str> {
        let encoding = self.encoding();
        match tx.news_path(encoding) {
            Some(Ok(path)) => Ok(path),
            Some(Err(_)) => Err("bad path"),
            None => Ok(HlPath::default()),
        }
    }

    /// 370: list a threaded news bundle's own contents.
    pub(super) async fn on_news_listdir(
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
                Ok(bytes) => fields.push(Field::bytes(FieldId::NewsDirlistExtended, bytes)),
                Err(_) => return vec![tx.reply_error("news listing too large to encode")],
            }
        }
        vec![tx.reply(fields)]
    }

    /// 371: list a threaded news category's articles.
    pub(super) async fn on_news_listcategory(
        &self,
        tx: &Transaction,
        state: &SharedState,
    ) -> Vec<Transaction> {
        let path = match self.news_path(tx) {
            Ok(path) => path,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let listing = match state.news_list_articles(&path).await {
            Ok(listing) => listing,
            Err(reason) => return vec![tx.reply_error(reason)],
        };
        let bytes = match listing.encode(self.encoding()) {
            Ok(bytes) => bytes,
            Err(_) => return vec![tx.reply_error("news listing too large to encode")],
        };
        vec![tx.reply(vec![Field::bytes(FieldId::NewsArtListData, bytes)])]
    }

    /// 380: delete a threaded news bundle or category.
    pub(super) async fn on_news_delete(
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

    /// 381: create a threaded news bundle (folder).
    pub(super) async fn on_news_mkdir(
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

    /// 382: create a threaded news category.
    pub(super) async fn on_news_mkcategory(
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

    /// 400: fetch one threaded news article's data.
    pub(super) async fn on_news_getthread(
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
            FieldId::NewsArtDataFlavor,
            requested_flavor.clone(),
        ));
        if let Some(data) = detail.data.get(&requested_flavor) {
            fields.push(Field::bytes(FieldId::NewsArtData, data.clone()));
        }
        vec![tx.reply(fields)]
    }

    /// 410: post a threaded news article.
    pub(super) async fn on_news_postthread(
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

        if let Err(reason) = state
            .news_post_article(
                &path,
                parent_id,
                title,
                self.name.clone(),
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

    /// 411: delete a threaded news article.
    pub(super) async fn on_news_deletethread(
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
            vec![Field::string(FieldId::Text, "hello news")],
        );
        session.on_newsfile_post(&post, &state).await;

        let get = Transaction::request(Opcode::NewsfileGet, 2, vec![]);
        let reply = session.on_newsfile_get(&get, &state).await;
        let [reply] = reply.try_into().unwrap();
        let news = reply.string(FieldId::Text, encoding).unwrap();
        assert!(news.contains("hello news"));
        assert!(news.contains("From Alice"));
    }

    /// A board past a field's `u16` length sends its newest 65535 bytes.
    #[tokio::test]
    async fn newsfile_get_clamps_a_board_past_the_field_limit() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();

        for i in 0..20 {
            let body = format!("{i}{}", "x".repeat(8000));
            let post = Transaction::request(
                Opcode::OldPostNews,
                1,
                vec![Field::string(FieldId::Text, body)],
            );
            session.on_newsfile_post(&post, &state).await;
        }
        assert!(state.news().await.len() > MAX_FLAT_NEWS_LEN);

        let get = Transaction::request(Opcode::NewsfileGet, 2, vec![]);
        let [reply] = session
            .on_newsfile_get(&get, &state)
            .await
            .try_into()
            .unwrap();

        let data = reply.field(FieldId::Text).unwrap().as_bytes(encoding);
        assert_eq!(data.len(), MAX_FLAT_NEWS_LEN);
        // Posts are prepended, so the clamp keeps the newest.
        assert!(data.starts_with(b"From Alice"));
        assert!(data.windows(6).any(|w| w == b"19xxxx"));
        assert!(!data.windows(5).any(|w| w == b"0xxxx"));
        reply.encode(encoding).expect("clamped reply encodes");
    }

    #[tokio::test]
    async fn newsfile_post_publishes_once() {
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Text, "hello news")],
        );
        session.on_newsfile_post(&post, &state).await;

        let notify = rx
            .try_recv()
            .ok()
            .and_then(RecipientMessage::into_push)
            .expect("expected a NewsfilePost push");
        assert_eq!(notify.r#type(), TransactionType::Push(Opcode::NewMsg));
        assert!(rx.try_recv().is_err(), "published only once");
    }

    #[tokio::test]
    async fn newsfile_post_rejects_oversized_text() {
        let encoding = text_encoding::DEFAULT;
        let session = test_session("Alice", Access::all());
        let state = SharedState::default();
        let mut rx = state.register_recipient(1).await;
        let post = Transaction::request(
            Opcode::OldPostNews,
            1,
            vec![Field::string(FieldId::Text, "a".repeat(8193))],
        );
        let replies = session.on_newsfile_post(&post, &state).await;

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
    async fn news_mkdir_and_mkcategory_create_items_listdir_can_see() {
        let state = SharedState::default();
        let session = test_session("Alice", Access::all());

        let mkdir = Transaction::request(
            Opcode::NewsMkdir,
            1,
            vec![Field::string(FieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_news_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let mkcategory = Transaction::request(
            Opcode::NewsMkcategory,
            2,
            vec![Field::string(FieldId::NewsCatName, "General")],
        );
        let [reply] = session
            .on_news_mkcategory(&mkcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listdir = Transaction::request(Opcode::NewsListdir, 3, vec![]);
        let [reply] = session
            .on_news_listdir(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 2);
    }

    #[tokio::test]
    async fn news_mkdir_denied_without_news_create_fldr() {
        let state = SharedState::default();
        let mut access = Access::all();
        access.remove(Access::NEWS_CREATE_FLDR);
        let session = test_session("Alice", access);

        let mkdir = Transaction::request(
            Opcode::NewsMkdir,
            1,
            vec![Field::string(FieldId::FileName, "Bundle")],
        );
        let [reply] = session
            .on_news_mkdir(&mkdir, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }

    #[tokio::test]
    async fn news_delete_removes_a_bundle_and_requires_news_delete_fldr() {
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
            Opcode::NewsDelete,
            1,
            vec![Field::bytes(
                FieldId::NewsPath,
                HlPath::new(["Bundle"]).encode(encoding).unwrap(),
            )],
        );
        let [reply] = denied_session
            .on_news_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());

        let [reply] = session
            .on_news_delete(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listdir = Transaction::request(Opcode::NewsListdir, 2, vec![]);
        let [reply] = session
            .on_news_listdir(&listdir, &state)
            .await
            .try_into()
            .unwrap();
        assert_eq!(reply.fields.len(), 0);
    }

    #[tokio::test]
    async fn news_postthread_getthread_and_listcategory_round_trip_an_article() {
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
            Opcode::NewsPostthread,
            1,
            vec![
                path_field(),
                Field::string(FieldId::NewsArtTitle, "Hello"),
                Field::string(FieldId::NewsArtDataFlavor, "text/plain"),
                Field::bytes(FieldId::NewsArtData, b"body text".to_vec()),
            ],
        );
        let [reply] = session
            .on_news_postthread(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());

        let listcategory = Transaction::request(Opcode::NewsListcategory, 2, vec![path_field()]);
        let [reply] = session
            .on_news_listcategory(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        assert_eq!(reply.fields.len(), 1);

        let getthread = Transaction::request(
            Opcode::NewsGetthread,
            3,
            vec![
                path_field(),
                Field::u32(FieldId::NewsArtId, 1),
                Field::string(FieldId::NewsArtDataFlavor, "text/plain"),
            ],
        );
        let [reply] = session
            .on_news_getthread(&getthread, &state)
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
            Opcode::NewsDeletethread,
            4,
            vec![path_field(), Field::u32(FieldId::NewsArtId, 1)],
        );
        let [reply] = session
            .on_news_deletethread(&delete, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
        let listcategory = Transaction::request(Opcode::NewsListcategory, 5, vec![path_field()]);
        let [reply] = session
            .on_news_listcategory(&listcategory, &state)
            .await
            .try_into()
            .unwrap();
        assert!(!reply.is_error());
    }

    #[tokio::test]
    async fn news_postthread_denied_without_post_news_art() {
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
            Opcode::NewsPostthread,
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
            .on_news_postthread(&post, &state)
            .await
            .try_into()
            .unwrap();
        assert!(reply.is_error());
    }
}
