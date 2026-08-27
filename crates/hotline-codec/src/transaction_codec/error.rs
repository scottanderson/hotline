use hotline_proto::ProtoError;

/// A transport failure or an invalid [`hotline_proto::Transaction`].
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The underlying transport failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The bytes read were not a valid transaction.
    #[error("protocol error: {0}")]
    Proto(#[from] ProtoError),
    /// A transaction's declared size exceeded this codec's cap.
    #[error("transaction size {len} exceeds the max of {max}")]
    TooLarge {
        /// The declared size that was rejected.
        len: u32,
        /// The cap it broke.
        max: u32,
    },
    /// A transaction declared a zero `len` or a zero `len2`.
    #[error("transaction declares a zero size")]
    ZeroLength,
    /// Too many split transactions are pending reassembly at once: one
    /// connection may hold at most `MAX_PENDING_BYTES` in reassembly.
    #[error("too many pending partial transactions (max {max})")]
    TooManyPendingPartials {
        /// The cap it broke.
        max: usize,
    },
}

impl From<CodecError> for ProtoError {
    fn from(err: CodecError) -> Self {
        match err {
            CodecError::Proto(e) => e,
            other => ProtoError::Malformed(other.to_string()),
        }
    }
}
