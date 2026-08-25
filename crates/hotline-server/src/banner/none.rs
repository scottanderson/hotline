use std::io;

use async_trait::async_trait;

use crate::banner::{BannerKind, BannerProvider};
use crate::file::BoxedReader;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn no_banner_reports_nothing_configured() {
        assert_eq!(NoBanner.kind().await, None);
    }
}
