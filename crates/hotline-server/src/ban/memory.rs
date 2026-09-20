use std::borrow::Cow;
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::SystemTime;

use async_trait::async_trait;

use crate::ban::{BanProvider, BanRecord};

/// A [`BanProvider`] that keeps bans in memory only; nothing survives a
/// restart.
#[derive(Debug, Default)]
pub struct InMemoryBanProvider {
    bans: RwLock<HashMap<IpAddr, BanRecord>>,
}

#[async_trait]
impl BanProvider for InMemoryBanProvider {
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord> {
        let record = self.bans.read().unwrap().get(&ip).cloned()?;
        match record.until {
            Some(until) if until <= SystemTime::now() => None,
            _ => Some(record),
        }
    }

    async fn ban(&self, ip: IpAddr, until: Option<SystemTime>) -> Result<(), Cow<'static, str>> {
        self.bans
            .write()
            .unwrap()
            .insert(ip, BanRecord { ip, until });
        Ok(())
    }

    async fn unban(&self, ip: IpAddr) -> Result<(), Cow<'static, str>> {
        self.bans.write().unwrap().remove(&ip);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn addr() -> IpAddr {
        "192.0.2.1".parse().unwrap()
    }

    #[tokio::test]
    async fn banned_ip_is_reported_until_unbanned() {
        let provider = InMemoryBanProvider::default();
        assert_eq!(provider.is_banned(addr()).await, None);

        provider.ban(addr(), None).await.unwrap();
        assert_eq!(
            provider.is_banned(addr()).await,
            Some(BanRecord {
                ip: addr(),
                until: None,
            })
        );

        provider.unban(addr()).await.unwrap();
        assert_eq!(provider.is_banned(addr()).await, None);
    }

    #[tokio::test]
    async fn expired_temp_ban_is_not_reported() {
        let provider = InMemoryBanProvider::default();
        let past = SystemTime::now() - Duration::from_secs(1);
        provider.ban(addr(), Some(past)).await.unwrap();
        assert_eq!(provider.is_banned(addr()).await, None);
    }

    #[tokio::test]
    async fn unexpired_temp_ban_is_reported() {
        let provider = InMemoryBanProvider::default();
        let future = SystemTime::now() + Duration::from_secs(3600);
        provider.ban(addr(), Some(future)).await.unwrap();
        assert!(provider.is_banned(addr()).await.is_some());
    }
}
