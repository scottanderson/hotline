//! [`PStringTooLong`]: the error [`crate::wire::Writer::pstring`] returns.

/// A string was too long to fit a 1-byte Pascal-string length prefix
/// (max 255 bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("string is {len} bytes, too long for a 1-byte length prefix (max 255)")]
pub struct PStringTooLong {
    /// The string's actual length in bytes.
    pub len: usize,
}
