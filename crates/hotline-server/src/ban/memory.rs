use std::borrow::Cow;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;

use async_trait::async_trait;

use crate::ban::{BanProvider, BanRecord};
use crate::sync::{read, write};

/// A [`BanProvider`] that keeps bans in memory only; nothing survives a
/// restart.
#[derive(Debug, Default)]
pub struct InMemoryBanProvider {
    bans: RwLock<HashMap<IpAddr, BanRecord>>,
}

#[async_trait]
impl BanProvider for InMemoryBanProvider {
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord> {
        let record = read(&self.bans).get(&ip).cloned()?;
        record.is_active().then_some(record)
    }

    async fn ban(&self, record: BanRecord) -> Result<(), Cow<'static, str>> {
        write(&self.bans).insert(record.ip, record);
        Ok(())
    }

    async fn unban(&self, ip: IpAddr) -> Result<Option<BanRecord>, Cow<'static, str>> {
        let record = write(&self.bans).remove(&ip);
        Ok(record.filter(BanRecord::is_active))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn addr() -> IpAddr {
        "192.0.2.1".parse().unwrap()
    }

    #[tokio::test]
    async fn banned_ip_is_reported_until_unbanned() {
        let provider = InMemoryBanProvider::default();
        assert_eq!(provider.is_banned(addr()).await, None);

        provider.ban(BanRecord::new(addr(), None)).await.unwrap();
        assert_eq!(
            provider.is_banned(addr()).await,
            Some(BanRecord::new(addr(), None))
        );

        assert!(provider.unban(addr()).await.unwrap().is_some());
        assert_eq!(provider.is_banned(addr()).await, None);
        assert_eq!(provider.unban(addr()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn unban_of_an_expired_ban_returns_none() {
        let provider = InMemoryBanProvider::default();
        let past = SystemTime::now() - Duration::from_secs(1);
        provider
            .ban(BanRecord::new(addr(), Some(past)))
            .await
            .unwrap();
        assert_eq!(provider.unban(addr()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn expired_temp_ban_is_not_reported() {
        let provider = InMemoryBanProvider::default();
        let past = SystemTime::now() - Duration::from_secs(1);
        provider
            .ban(BanRecord::new(addr(), Some(past)))
            .await
            .unwrap();
        assert_eq!(provider.is_banned(addr()).await, None);
    }

    #[tokio::test]
    async fn unexpired_temp_ban_is_reported() {
        let provider = InMemoryBanProvider::default();
        let future = SystemTime::now() + Duration::from_secs(3600);
        provider
            .ban(BanRecord::new(addr(), Some(future)))
            .await
            .unwrap();
        assert!(provider.is_banned(addr()).await.is_some());
    }
}
