//! Connection bans, keyed by IP address.
//!
//! [`BanProvider`] is the storage interface; `hotline-server` only defines
//! it and provides [`InMemoryBanProvider`] as a default. The embedding
//! application supplies the real backend to [`crate::Server::new`], the
//! same way it supplies an [`crate::AccountProvider`].

mod memory;
mod provider;
mod record;

pub use memory::InMemoryBanProvider;
pub use provider::BanProvider;
pub use record::BanRecord;
