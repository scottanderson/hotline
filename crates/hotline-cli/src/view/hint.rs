use hotline_proto::Access;

/// A key hint: the key, what it does, and the privileges that turn it on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub key: &'static str,
    pub does: &'static str,
    /// Any one of these privileges turns the key on. Empty for a key that
    /// needs none.
    pub enabled_by: Access,
}

impl Hint {
    pub const fn new(key: &'static str, does: &'static str) -> Self {
        Self {
            key,
            does,
            enabled_by: Access::empty(),
        }
    }

    /// This hint, turned on by any one of the privileges in `access`.
    pub const fn enabled_by(self, access: Access) -> Self {
        Self {
            enabled_by: access,
            ..self
        }
    }
}
