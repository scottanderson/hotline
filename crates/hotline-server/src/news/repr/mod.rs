//! `serde` support for [`NewsArticle`](crate::news::NewsArticle), kept
//! separate from the plain data types in the parent module. A derive
//! alone can't express its conversion logic: `date`
//! ([`hotline_proto::types::HlDate`]) becomes an ISO 8601 string via
//! [`super::iso_date`], and `data` becomes plain text when it reads as
//! one, or a byte array otherwise.

mod borrowed;
mod owned;

pub(crate) use borrowed::{ArticleDataRef, NewsArticleReprRef};
pub(crate) use owned::{ArticleData, NewsArticleRepr};
