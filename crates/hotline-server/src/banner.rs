//! Server banner, pushed to a client at login (`myTran_ServerBanner`, 122)
//! and, for an image banner, fetched over `HTXF` by `myTran_DownloadBanner`
//! (212).
//!
//! [`BannerProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`NoBanner`] (no banner configured) and
//! [`UrlBannerProvider`] (a fixed URL, nothing to fetch over `HTXF`) as
//! defaults. The embedding application supplies the real backend to
//! [`crate::Server::new`], the same way it supplies a [`crate::FileProvider`].

use std::io;

use async_trait::async_trait;

use crate::files::BoxedReader;

/// A configured banner's shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerKind {
    /// Show a URL to the client directly; nothing to fetch over `HTXF`.
    Url(String),
    /// A fetchable image; `format` is the 4-byte type tag sent as
    /// `myField_ServerBannerType`, e.g. `*b"JPEG"`, `*b"GIFf"`, `*b"PICT"`,
    /// or `*b"BMP "`.
    Image { format: [u8; 4] },
}

impl BannerKind {
    /// The 4-byte tag sent as `myField_ServerBannerType`.
    pub fn type_tag(&self) -> [u8; 4] {
        match self {
            Self::Url(_) => *b"URL ",
            Self::Image { format } => *format,
        }
    }
}

/// Server banner storage. `kind` answers the login-time push; `open_read`/
/// `size` serve `myTran_DownloadBanner`'s `HTXF` download and are only ever
/// called for a [`BannerKind::Image`] banner.
#[async_trait]
pub trait BannerProvider: Send + Sync {
    /// The currently configured banner, or `None` if none is set.
    async fn kind(&self) -> Option<BannerKind>;

    /// Open the banner image for reading.
    async fn open_read(&self) -> io::Result<BoxedReader>;

    /// The banner image's byte size.
    async fn size(&self) -> io::Result<u64>;
}

/// A [`BannerProvider`] that never configures a banner.
#[derive(Debug, Default)]
pub struct NoBanner;

#[async_trait]
impl BannerProvider for NoBanner {
    async fn kind(&self) -> Option<BannerKind> {
        None
    }

    async fn open_read(&self) -> io::Result<BoxedReader> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }

    async fn size(&self) -> io::Result<u64> {
        Err(io::Error::from(io::ErrorKind::NotFound))
    }
}

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
    async fn no_banner_reports_nothing_configured() {
        assert_eq!(NoBanner.kind().await, None);
    }

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

    #[test]
    fn image_banner_type_tag_is_its_format() {
        let kind = BannerKind::Image { format: *b"JPEG" };
        assert_eq!(&kind.type_tag(), b"JPEG");
    }
}
