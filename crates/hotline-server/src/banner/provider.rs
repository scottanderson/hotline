use std::io;

use async_trait::async_trait;

use super::BannerKind;
use crate::file::BoxedReader;

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
