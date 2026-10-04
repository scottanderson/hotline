//! [`ItemTimes`]: when a file or folder was made and changed.

use std::time::SystemTime;

/// When a file or folder was made and last changed. `None` for a time
/// that a [`crate::FileProvider`] does not keep.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ItemTimes {
    /// When the item was made.
    pub created: Option<SystemTime>,
    /// When the item last changed.
    pub modified: Option<SystemTime>,
}
