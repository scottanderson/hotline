use std::io;

use async_trait::async_trait;

use super::BannerKind;
use crate::file::BoxedReader;

/// Server banner storage. `kind` answers the login-time push; `open_read`/
/// `size` serve the `HTXF` download for [`myTran_DownloadBanner`] (212), and
/// are only ever called for a [`BannerKind::Image`] banner.
///
/// [`myTran_DownloadBanner`]: hotline_proto::Opcode::DownloadBanner
#[async_trait]
pub trait BannerProvider: Send + Sync {
    /// The currently configured banner, or `None` if none is set.
    async fn kind(&self) -> Option<BannerKind>;

    /// Open the banner image for reading.
    async fn open_read(&self) -> io::Result<BoxedReader>;

    /// The banner image's byte size.
    async fn size(&self) -> io::Result<u64>;
}
