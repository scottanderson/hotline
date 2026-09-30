use std::borrow::Cow;
use std::net::IpAddr;
use std::time::SystemTime;

use async_trait::async_trait;

use super::BanRecord;

/// Ban storage, checked on connect.
#[async_trait]
pub trait BanProvider: Send + Sync {
    /// The active ban on `ip`, or `None` if it isn't banned or its ban
    /// already expired.
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord>;
    /// Ban `ip` until `until`, or permanently if `None`.
    async fn ban(&self, ip: IpAddr, until: Option<SystemTime>) -> Result<(), Cow<'static, str>>;
    /// Lift `ip`'s ban, returning the active ban it removed.
    async fn unban(&self, ip: IpAddr) -> Result<Option<BanRecord>, Cow<'static, str>>;
}
