use std::io;

use async_trait::async_trait;

use crate::banner::{BannerKind, BannerProvider};
use crate::file::BoxedReader;

/// A [`BannerProvider`] that always reports a fixed URL banner; there's no
/// image to fetch over `HTXF`.
#[derive(Debug, Clone)]
pub struct UrlBannerProvider {
    url: String,
}

impl UrlBannerProvider {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }
}

#[async_trait]
impl BannerProvider for UrlBannerProvider {
    async fn kind(&self) -> Option<BannerKind> {
        Some(BannerKind::Url(self.url.clone()))
    }

    async fn open_read(&self) -> io::Result<BoxedReader> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    async fn size(&self) -> io::Result<u64> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn url_banner_reports_its_url_and_type_tag() {
        let provider = UrlBannerProvider::new("https://example.com/banner");
        let kind = provider.kind().await.unwrap();
        assert_eq!(
            kind,
            BannerKind::Url("https://example.com/banner".to_string())
        );
        assert_eq!(&kind.type_tag(), b"URL ");
    }
}
