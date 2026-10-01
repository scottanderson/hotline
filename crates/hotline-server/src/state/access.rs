use hotline_proto::consts::Access;

/// A live privilege change for every session logged in under `login`.
#[derive(Debug, Clone)]
pub struct AccessUpdate {
    /// Login the modified account was stored under before this edit.
    pub login: Box<str>,
    /// The account's new login, if this edit renamed it.
    pub renamed_to: Option<Box<str>>,
    /// The account's new privileges.
    pub access: Access,
    /// The account's new display name, if non-empty.
    pub name: Option<Box<str>>,
}
