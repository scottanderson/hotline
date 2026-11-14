use std::borrow::Cow;

use hotline_proto::Access;

/// A key hint: the key, what it does, and the privileges that turn it on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint<'a> {
    pub key: Cow<'a, str>,
    pub does: &'static str,
    /// Any one of these privileges turns the key on. Empty for a key that
    /// needs none.
    pub enabled_by: Access,
}

impl<'a> Hint<'a> {
    pub fn new(key: impl Into<Cow<'a, str>>, does: &'static str) -> Self {
        Self {
            key: key.into(),
            does,
            enabled_by: Access::empty(),
        }
    }

    /// This hint, turned on by any one of the privileges in `access`.
    pub fn enabled_by(self, access: Access) -> Self {
        Self {
            enabled_by: access,
            ..self
        }
    }
}
