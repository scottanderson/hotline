use hotline_proto::TransactionHeader;

/// What [`super::TransactionCodec`] is waiting for next.
#[derive(Debug, Default)]
pub(super) enum DecodeState {
    /// Waiting for the 20-byte fixed prefix.
    #[default]
    Prefix,
    /// Prefix decoded; waiting for `header.data_len()` more bytes to
    /// finish it.
    Body(TransactionHeader),
}
