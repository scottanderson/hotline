//! Shared, cross-session server state: the one per-recipient channel
//! sessions use to reach each other, the transfers, and the calls to the
//! providers.

mod access;
mod console;
mod download_queue;
mod entry;
mod file_transfer;
mod folder_transfer;
mod progress;
mod recipient;
mod replaced_account;
mod shared;
mod transfer;
mod transfer_ref;
mod transfer_target;

use std::borrow::Cow;

use hotline_proto::consts::Access;
use hotline_proto::types::HlPath;

pub use access::AccessUpdate;
pub use console::ConsoleUpdate;
pub use entry::{IDLE_THRESHOLD, UserEntry};
pub use file_transfer::FileTransfer;
pub use folder_transfer::FolderTransfer;
pub(crate) use progress::ProgressGuard;
pub use progress::{TransferProgress, TransferReport, TransferSection};
pub(crate) use recipient::{Connection, QUEUE_LEN};
pub use recipient::{Inbox, RecipientMessage};
pub(crate) use replaced_account::{NotReplaced, ReplacedAccount};
pub use shared::SharedState;
pub use transfer::{PendingTransfer, TransferDirection, TransferKind};
pub use transfer_ref::TransferRef;
pub use transfer_target::TransferTarget;

/// `true` if `dir`'s own name contains "upload" or "drop box"
/// (case-insensitive), the one place an account without
/// `UPLOAD_ANYWHERE` may still upload to.
fn is_upload_folder(dir: &HlPath) -> bool {
    dir.components.last().is_some_and(|c| {
        let c = c.to_ascii_lowercase();
        c.contains("upload") || c.contains("drop box")
    })
}

/// `Err` if `dir` is not an upload folder and `access` lacks
/// `UPLOAD_ANYWHERE`, for an upload of the `kind` `name`.
fn upload_destination_access(
    dir: &HlPath,
    access: Access,
    kind: &str,
    name: &str,
) -> Result<(), String> {
    if !is_upload_folder(dir) && !access.contains(Access::UPLOAD_ANYWHERE) {
        Err(format!(
            "Cannot accept upload of the {kind} “{name}” because you are only allowed to upload to the “Uploads” folder."
        ))?;
    }
    Ok(())
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

/// `Err` with the text of a missing `VIEW_DROP_BOXES` if `dropbox_blocked`
/// for `dir`/`name`.
fn dropbox_access(
    dir: &HlPath,
    name: Option<&str>,
    access: Access,
) -> Result<(), Cow<'static, str>> {
    if dropbox_blocked(dir, name, access) {
        Err("You are not allowed to view drop boxes.")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_destination_folders_own_name_counts_as_an_upload_folder() {
        assert!(is_upload_folder(&HlPath::new(["Uploads"])));
        assert!(is_upload_folder(&HlPath::new(["Files", "Drop Box"])));
        assert!(!is_upload_folder(&HlPath::new(["Uploads", "Music"])));
        assert!(!is_upload_folder(&HlPath::default()));
    }
}
