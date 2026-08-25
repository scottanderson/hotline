//! Named accounts: login, password, display name, and the `USER_ACCESS`
//! privileges granted on successful login.
//!
//! [`AccountProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`InMemoryAccountProvider`] as a default. The
//! embedding application supplies the real backend to [`crate::Server::new`].

mod access_extra;
mod memory;
mod provider;

pub use access_extra::AccessExtra;
pub use memory::InMemoryAccountProvider;
pub use provider::AccountProvider;

use hotline_proto::consts::Access;

/// One named account.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Account {
    /// Compared for exact equality against the submitted password; an
    /// empty stored password requires an empty submission too.
    pub password: String,
    pub name: String,
    #[serde(with = "crate::bitflags_serde")]
    pub access: Access,
    /// See [`AccessExtra`].
    #[serde(with = "crate::bitflags_serde")]
    pub access_extra: AccessExtra,
}
