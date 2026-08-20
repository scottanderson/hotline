//! Connection bans, keyed by IP address.
//!
//! [`BanProvider`] is the storage interface; `hotline-server` only defines
//! it and provides [`InMemoryBanProvider`] as a default. The embedding
//! application supplies the real backend to [`crate::Server::new`], the
//! same way it supplies an [`crate::AccountProvider`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::RwLock;
use std::time::SystemTime;

use async_trait::async_trait;

/// One address's ban: when it was imposed and, unless permanent, when it lifts.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BanRecord {
    pub ip: IpAddr,
    /// `None` means the ban never expires.
    #[serde(with = "until_rfc3339")]
    pub until: Option<SystemTime>,
}

/// Serializes `Option<SystemTime>` as an RFC 3339 (ISO 8601) timestamp
/// string, so `bans.json` reads as a human date instead of raw epoch seconds.
mod until_rfc3339 {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::SystemTime;

    pub fn serialize<S: Serializer>(
        until: &Option<SystemTime>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        until
            .map(|t| humantime::format_rfc3339_seconds(t).to_string())
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<SystemTime>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map(|s| humantime::parse_rfc3339(&s).map_err(serde::de::Error::custom))
            .transpose()
    }
}

/// Ban storage, checked on connect and written by the kick-to-ban `Options`
/// bit on `myTran_DisconnectUser` (110).
#[async_trait]
pub trait BanProvider: Send + Sync {
    /// The active ban on `ip`, or `None` if it isn't banned or its ban
    /// already expired.
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord>;
    /// Ban `ip` until `until`, or permanently if `None`.
    async fn ban(&self, ip: IpAddr, until: Option<SystemTime>) -> Result<(), String>;
    /// Lift a ban.
    async fn unban(&self, ip: IpAddr) -> Result<(), String>;
}

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

    async fn ban(&self, ip: IpAddr, until: Option<SystemTime>) -> Result<(), String> {
        self.bans
            .write()
            .unwrap()
            .insert(ip, BanRecord { ip, until });
        Ok(())
    }

    async fn unban(&self, ip: IpAddr) -> Result<(), String> {
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

    #[test]
    fn until_serializes_as_an_rfc3339_timestamp_and_round_trips() {
        let record = BanRecord {
            ip: addr(),
            until: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"ip":"192.0.2.1","until":"2023-11-14T22:13:20Z"}"#);
        assert_eq!(serde_json::from_str::<BanRecord>(&json).unwrap(), record);
    }

    #[test]
    fn a_permanent_ban_serializes_until_as_null() {
        let record = BanRecord {
            ip: addr(),
            until: None,
        };
        let json = serde_json::to_string(&record).unwrap();
        assert_eq!(json, r#"{"ip":"192.0.2.1","until":null}"#);
        assert_eq!(serde_json::from_str::<BanRecord>(&json).unwrap(), record);
    }
}
