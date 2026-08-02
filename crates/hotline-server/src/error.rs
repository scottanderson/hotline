//! Server-side error type.

use thiserror::Error;

/// Convenience alias for results returned by `hotline-server`.
pub type Result<T> = std::result::Result<T, ServerError>;

/// Everything that can go wrong running a listener or session.
#[derive(Debug, Error)]
pub enum ServerError {
    /// Transport-level failure (bind, accept, read, write).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The wire format itself was malformed.
    #[error("protocol error: {0}")]
    Proto(#[from] hotline_proto::ProtoError),

    /// The client failed the handshake (bad magic, wrong version, ...).
    #[error("handshake rejected: {0}")]
    HandshakeRejected(&'static str),
}

impl From<hotline_codec::CodecError> for ServerError {
    fn from(err: hotline_codec::CodecError) -> Self {
        match err {
            hotline_codec::CodecError::Io(e) => Self::Io(e),
            hotline_codec::CodecError::Proto(e) => Self::Proto(e),
        }
    }
}
