use std::collections::BTreeMap;

use serde::Serialize;

use crate::news::iso_date;

/// Borrowing counterpart to [`crate::news::repr::NewsArticleRepr`]/[`ArticleDataRef`], used only
/// for `Serialize`: it only needs to borrow from `&NewsArticle` for the
/// duration of one `serialize` call, so there's no need to clone every
/// field into an owned copy first the way `Deserialize` (which must
/// produce new owned data) does.
#[derive(Serialize)]
pub(crate) struct NewsArticleReprRef<'a> {
    pub(crate) parent_id: u32,
    pub(crate) flags: u32,
    #[serde(with = "iso_date")]
    pub(crate) date: hotline_proto::types::HlDate,
    pub(crate) title: &'a str,
    pub(crate) poster: &'a str,
    pub(crate) data: BTreeMap<&'a str, ArticleDataRef<'a>>,
    pub(crate) children: &'a [u32],
}

#[derive(Serialize)]
#[serde(untagged)]
pub(crate) enum ArticleDataRef<'a> {
    Text(&'a str),
    Bytes(&'a [u8]),
}
