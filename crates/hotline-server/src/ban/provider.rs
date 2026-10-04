use std::borrow::Cow;
use std::net::IpAddr;

use async_trait::async_trait;

use super::BanRecord;

/// Ban storage, checked on connect.
#[async_trait]
pub trait BanProvider: Send + Sync {
    /// The active ban on `ip`, or `None` if it isn't banned or its ban
    /// already expired.
    async fn is_banned(&self, ip: IpAddr) -> Option<BanRecord>;
    /// Store `record`, which replaces any ban on its address.
    async fn ban(&self, record: BanRecord) -> Result<(), Cow<'static, str>>;
    /// Lift `ip`'s ban, returning the active ban it removed.
    async fn unban(&self, ip: IpAddr) -> Result<Option<BanRecord>, Cow<'static, str>>;
}
