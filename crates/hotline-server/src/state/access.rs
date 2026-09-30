use hotline_proto::consts::Access;

/// A live privilege change for every session logged in under `login`.
#[derive(Debug, Clone)]
pub struct AccessUpdate {
    /// Login the modified account is stored under.
    pub login: Box<str>,
    /// The account's new privileges.
    pub access: Access,
    /// The account's new display name, if non-empty.
    pub name: Option<Box<str>>,
}
