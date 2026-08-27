//! Shared, cross-session server state: the connected-user table, chat
//! rooms, classic news, and the one per-recipient channel sessions use to
//! reach each other.

mod access;
mod console;
mod entry;
mod file_transfer;
mod recipient;
mod room;
mod shared;
mod transfer;
mod transfer_ref;
mod transfer_target;

use std::borrow::Cow;

use hotline_proto::consts::{Access, RequireFlags};
use hotline_proto::types::HlPath;

pub use access::AccessUpdate;
pub use console::ConsoleUpdate;
pub use entry::{UserEntry, IDLE_THRESHOLD};
pub use file_transfer::FileTransfer;
pub use recipient::RecipientMessage;
pub use room::ChatRoom;
pub use shared::SharedState;
pub use transfer::{PendingTransfer, TransferDirection, TransferKind};
pub use transfer_ref::TransferRef;
pub use transfer_target::TransferTarget;

/// `true` if any component of `dir` names an "Upload" or "Drop Box"
/// folder (case-insensitive), the one place an account without
/// `UPLOAD_ANYWHERE` may still upload to.
fn is_dropbox_path(dir: &HlPath) -> bool {
    dir.components.iter().any(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("upload") || c.contains("drop box")
    })
}

/// `true` if `access` lacks `VIEW_DROP_BOXES` and `dir`/`name`'s path
/// contains "drop box" (case-insensitive). `name` is `None` for a bare
/// directory listing.
fn dropbox_blocked(dir: &HlPath, name: Option<&str>, access: Access) -> bool {
    if access.contains(Access::VIEW_DROP_BOXES) {
        return false;
    }
    dir.components
        .iter()
        .map(String::as_str)
        .chain(name)
        .any(|c| c.to_ascii_lowercase().contains("drop box"))
}

/// `Err` if `dropbox_blocked` for `dir`/`name`, worded via
/// [`RequireFlags::require`] so it names `VIEW_DROP_BOXES` like any other
/// missing-privilege error.
fn dropbox_access(
    dir: &HlPath,
    name: Option<&str>,
    access: Access,
) -> Result<(), Cow<'static, str>> {
    if dropbox_blocked(dir, name, access) {
        access.require(Access::VIEW_DROP_BOXES)?;
    }
    Ok(())
}
