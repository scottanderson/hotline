use std::net::IpAddr;
use std::time::SystemTime;

use async_trait::async_trait;

use super::BanRecord;

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
