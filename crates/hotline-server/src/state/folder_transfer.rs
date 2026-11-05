use crate::file::FolderItem;
use crate::state::TransferTarget;
use hotline_proto::text_encoding::Encoding;

/// A pending `HTXF` folder transfer's identity, from [`crate::FileProvider`],
/// stepped through one [`FolderItem`] at a time.
#[derive(Debug, Clone)]
pub struct FolderTransfer {
    /// The folder's login and path.
    pub target: TransferTarget,
    /// Download: every item to send, in order. Always empty for an upload.
    pub items: Vec<FolderItem>,
    /// Upload: the item count the client declared. Download: `items.len()`.
    pub item_count: u32,
    /// The control connection's text encoding, for each item's path.
    pub encoding: &'static Encoding,
}
