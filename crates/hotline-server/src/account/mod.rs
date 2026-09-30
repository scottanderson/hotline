//! Named accounts: login, password, display name, and the [`myField_UserAccess`] (110)
//! privileges granted on successful login.
//!
//! [`AccountProvider`] is the storage interface; `hotline-server` only
//! defines it and provides [`InMemoryAccountProvider`] as a default. The
//! embedding application supplies the real backend to [`crate::Server::new`].
//!
//! [`myField_UserAccess`]: hotline_proto::FieldId::UserAccess

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
    pub password: Box<str>,
    /// Display name shown to other users.
    pub name: Box<str>,
    /// Privileges granted on successful login.
    #[serde(with = "crate::bitflags_serde")]
    pub access: Access,
    /// See [`AccessExtra`].
    #[serde(with = "crate::bitflags_serde")]
    pub access_extra: AccessExtra,
}

impl Account {
    /// `Admin`, with full access.
    pub fn admin(password: impl Into<Box<str>>) -> Self {
        Self {
            password: password.into(),
            name: "Admin".into(),
            access: Access::all(),
            access_extra: AccessExtra::all(),
        }
    }

    /// `Guest`, with default access and no password.
    pub fn guest() -> Self {
        Self {
            password: Box::default(),
            name: "Guest".into(),
            access: Access::default(),
            access_extra: AccessExtra::default(),
        }
    }
}
