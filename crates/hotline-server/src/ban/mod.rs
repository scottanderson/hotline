//! Connection bans, keyed by IP address.
//!
//! [`BanProvider`] is the storage interface; `hotline-server` only defines
//! it and provides [`InMemoryBanProvider`] as a default. The embedding
//! application supplies the real backend to [`crate::Server::new`], the
//! same way it supplies an [`crate::AccountProvider`].

mod memory;
mod provider;
mod record;

use std::time::Duration;

pub use memory::InMemoryBanProvider;
pub use provider::BanProvider;
pub use record::BanRecord;

/// How long a temporary ban lasts.
pub(crate) const TEMP_BAN_DURATION: Duration = Duration::from_secs(30 * 60);
