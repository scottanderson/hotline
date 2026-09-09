//! A local-image-file-backed [`BannerProvider`]. `hotline-server`
//! deliberately doesn't own persistence (see its module docs); this is
//! the concrete backend `hls` plugs in when the configured banner is an
//! image fetched over `HTXF` rather than a URL.

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use hotline_server::{BannerKind, BannerProvider, BoxedReader};

/// Serves a single local file as the image a `myTran_DownloadBanner`
/// download returns.
pub struct FileBannerProvider {
    path: PathBuf,
    format: [u8; 4],
}

impl FileBannerProvider {
    /// Opens `path`, inferring the wire format tag from its extension
    /// (`.jpg`/`.jpeg` -> `JPEG`, `.gif` -> `GIFf`, `.pct`/`.pict` ->
    /// `PICT`, `.bmp`/`.dib` -> `BMP `), the same mapping openline
    /// applies (`HotlineServ.cp`). Fails if `path` doesn't exist or its extension
    /// isn't one of those.
    pub async fn open(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        tokio::fs::metadata(&path).await?;
        let format = format_from_extension(&path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{}: unrecognized banner image extension", path.display()),
            )
        })?;
        Ok(Self { path, format })
    }
}

fn format_from_extension(path: &Path) -> Option<[u8; 4]> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "jpg" | "jpeg" => *b"JPEG",
        "gif" => *b"GIFf",
        "pct" | "pict" => *b"PICT",
        "bmp" | "dib" => *b"BMP ",
        _ => return None,
    })
}

#[async_trait]
impl BannerProvider for FileBannerProvider {
    async fn kind(&self) -> Option<BannerKind> {
        Some(BannerKind::Image {
            format: self.format,
        })
    }

    async fn open_read(&self) -> io::Result<BoxedReader> {
        Ok(Box::new(tokio::fs::File::open(&self.path).await?))
    }

    async fn size(&self) -> io::Result<u64> {
        Ok(tokio::fs::metadata(&self.path).await?.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_the_image_format_from_its_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("banner.gif");
        tokio::fs::write(&path, b"GIF89a...").await.unwrap();

        let provider = FileBannerProvider::open(&path).await.unwrap();
        assert_eq!(
            provider.kind().await,
            Some(BannerKind::Image { format: *b"GIFf" })
        );
    }

    #[tokio::test]
    async fn serves_the_files_bytes_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("banner.jpg");
        tokio::fs::write(&path, b"hello banner").await.unwrap();

        let provider = FileBannerProvider::open(&path).await.unwrap();
        assert_eq!(provider.size().await.unwrap(), 12);

        let mut reader = provider.open_read().await.unwrap();
        let mut bytes = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes)
            .await
            .unwrap();
        assert_eq!(bytes, b"hello banner");
    }

    #[tokio::test]
    async fn open_fails_for_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.jpg");

        assert!(FileBannerProvider::open(&path).await.is_err());
    }

    #[tokio::test]
    async fn open_fails_for_an_unrecognized_extension() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("banner.webp");
        tokio::fs::write(&path, b"...").await.unwrap();

        assert!(FileBannerProvider::open(&path).await.is_err());
    }
}
