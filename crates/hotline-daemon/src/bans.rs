//! A JSON-file-backed [`BanProvider`]. `hotline-server` deliberately
//! doesn't own persistence (see its module docs); this is the concrete
//! backend `hls` plugs in, persisting every ban to a single sidecar file
//! so they survive a restart.

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::time::SystemTime;

use async_trait::async_trait;
use hotline_server::{BanProvider, BanRecord};
use tokio::sync::RwLock;

/// Persists every ban as one entry in a single `<path>` JSON file, with
/// an in-memory cache so `is_banned` never touches disk.
pub struct FileBanProvider {
    path: PathBuf,
    cache: RwLock<HashMap<IpAddr, BanRecord>>,
}

impl FileBanProvider {
    /// Loads bans from `path`, or starts empty if it doesn't exist yet.
    pub async fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        let cache = match tokio::fs::read(&path).await {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("{}: {e}", path.display()),
                )
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e),
        };
        Ok(Self {
            path,
            cache: RwLock::new(cache),
        })
    }

    async fn write_cache(&self, cache: &HashMap<IpAddr, BanRecord>) -> std::io::Result<()> {
        let bytes = serde_json::to_vec_pretty(cache)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(&self.path, bytes).await
    }
}

#[async_trait]
impl BanProvider for FileBanProvider {
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord> {
        let record = self.cache.read().await.get(&ip).cloned()?;
        match record.until {
            Some(until) if until <= SystemTime::now() => None,
            _ => Some(record),
        }
    }

    async fn ban(&self, ip: IpAddr, until: Option<SystemTime>) -> Result<(), String> {
        let mut cache = self.cache.write().await;
        let mut updated = cache.clone();
        updated.insert(ip, BanRecord { ip, until });
        self.write_cache(&updated)
            .await
            .map_err(|e| e.to_string())?;
        *cache = updated;
        Ok(())
    }

    async fn unban(&self, ip: IpAddr) -> Result<(), String> {
        let mut cache = self.cache.write().await;
        let mut updated = cache.clone();
        updated.remove(&ip);
        self.write_cache(&updated)
            .await
            .map_err(|e| e.to_string())?;
        *cache = updated;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr() -> IpAddr {
        "192.0.2.1".parse().unwrap()
    }

    #[tokio::test]
    async fn ban_round_trips_and_survives_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        {
            let provider = FileBanProvider::open(&path).await.unwrap();
            assert_eq!(provider.is_banned(addr()).await, None);
            provider.ban(addr(), None).await.unwrap();
            assert!(provider.is_banned(addr()).await.is_some());
        }

        let reopened = FileBanProvider::open(&path).await.unwrap();
        assert!(reopened.is_banned(addr()).await.is_some());

        reopened.unban(addr()).await.unwrap();
        assert_eq!(reopened.is_banned(addr()).await, None);
    }

    #[tokio::test]
    async fn a_malformed_file_reports_its_path_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        tokio::fs::write(&path, b"not json").await.unwrap();

        let error = FileBanProvider::open(&path).await.err().unwrap();
        assert!(error.to_string().contains(&path.display().to_string()));
    }

    #[tokio::test]
    async fn a_failed_write_leaves_the_cache_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        let provider = FileBanProvider::open(&path).await.unwrap();

        // Put a directory where the ban file should be, so every write fails.
        tokio::fs::create_dir(&path).await.unwrap();

        assert!(provider.ban(addr(), None).await.is_err());
        assert_eq!(provider.is_banned(addr()).await, None);
    }
}
