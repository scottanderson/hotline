//! Client-side error type.

use thiserror::Error;

/// Convenience alias for results returned by `hotline-client`.
pub type Result<T> = std::result::Result<T, ClientError>;

/// Everything that can go wrong establishing or using a client session.
#[derive(Debug, Error)]
pub enum ClientError {
    /// Transport-level failure (connect, read, write).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The wire format itself was malformed.
    #[error("protocol error: {0}")]
    Proto(#[from] hotline_proto::ProtoError),

    /// The server replied with a nonzero error code. Carries the
    /// `ERROR_TEXT` field if the server sent one.
    #[error("server error: {0}")]
    Server(String),

    /// The background read loop exited (connection closed) before a
    /// pending request could be answered.
    #[error("connection closed while waiting for a reply")]
    ConnectionClosed,
}

impl From<hotline_codec::CodecError> for ClientError {
    fn from(err: hotline_codec::CodecError) -> Self {
        match err {
            hotline_codec::CodecError::Io(e) => Self::Io(e),
            other => Self::Proto(other.into()),
        }
    }
}
