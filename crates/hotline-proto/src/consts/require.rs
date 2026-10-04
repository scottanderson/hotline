/// Extension trait for any `bitflags` type: a privilege check that names
/// the missing flags via each type's own [`Self::actions`] table rather
/// than a raw flag-name dump.
pub trait RequireFlags: bitflags::Flags + Copy {
    /// `Err` describes every flag `required` has that `self` doesn't,
    /// via [`Self::actions`], joined by `", "`.
    fn require(self, required: Self) -> Result<(), String> {
        let missing = required.difference(self);
        if missing.is_empty() {
            return Ok(());
        }
        let actions = missing.actions();
        Err(format!("You are not allowed to {actions}."))
    }

    /// `Err` describes every flag in `any_of` when `self` has none of them.
    fn require_any(self, any_of: Self) -> Result<(), String> {
        if self.intersects(any_of) {
            return Ok(());
        }
        let actions = any_of.actions();
        Err(format!("You are not allowed to {actions}."))
    }

    /// Description of the actions that the privilege guards against, e.g.
    /// "participate in chat" for `SEND_CHAT`.
    fn actions(&self) -> String;
}
