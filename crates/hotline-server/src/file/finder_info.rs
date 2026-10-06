//! [`FinderInfo`]: a file's Mac type, creator, and Finder flags.

/// A file's Mac type, creator, and Finder flags (`fdFlags`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FinderInfo {
    /// The Mac file type.
    pub file_type: [u8; 4],
    /// The Mac creator.
    pub creator: [u8; 4],
    /// The Finder flags.
    pub flags: u16,
}

impl FinderInfo {
    /// The Finder flag that tells the Finder it has seen the file.
    pub const INITED: u16 = 0x0100;

    /// The Finder flag that marks an alias.
    pub const ALIAS: u16 = 0x8000;
}
