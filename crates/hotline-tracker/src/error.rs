use hotline_proto::wire::PStringTooLong;
use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, TrackerError>;

/// Errors decoding or encoding tracker traffic.
#[derive(Debug, Error)]
pub enum TrackerError {
    /// Buffer too short for the structure being decoded.
    #[error("unexpected end of buffer: needed {needed}, had {available}")]
    UnexpectedEof {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },
    /// The `HTRK` magic number did not match.
    #[error("bad magic, expected HTRK")]
    BadMagic,
    /// A name/description/password was too long to encode.
    #[error(transparent)]
    StringTooLong(#[from] PStringTooLong),
}
