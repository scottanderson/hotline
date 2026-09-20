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
    /// A chunk's entries did not fit the size it declared.
    #[error("chunk declares {declared} bytes of entries, its {servers} entries did not fit")]
    MalformedChunk {
        /// The chunk's own `data_size`.
        declared: u16,
        /// The entry count the chunk's header gave.
        servers: u16,
    },
    /// A reply chunk carried a message type this crate doesn't know.
    #[error("unexpected tracker message type {0}")]
    UnexpectedMessageType(u16),
    /// A name/description/password was too long to encode.
    #[error(transparent)]
    StringTooLong(#[from] PStringTooLong),
}
