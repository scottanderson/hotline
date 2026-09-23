use hotline_proto::types::HlPath;

/// One file or folder under a folder being transferred, from
/// [`crate::FileProvider::walk`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderItem {
    /// Path from the walked folder to this item, walked folder not included.
    pub path: HlPath,
    /// Whether this item is a folder.
    pub is_folder: bool,
    /// A file's size in bytes. Always `0` for a folder.
    pub size: u64,
}
