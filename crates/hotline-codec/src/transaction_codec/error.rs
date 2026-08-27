use hotline_proto::ProtoError;

/// Framing errors: either the transport failed (`io::Error`, required by
/// `Decoder`/`Encoder`'s `Error: From<io::Error>` bound) or the bytes
/// that arrived weren't a valid `Transaction`.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The underlying transport failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes read were not a valid transaction.
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),
}
