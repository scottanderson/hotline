use hotline_proto::TransactionHeader;

/// A split transaction's parts accumulated so far, keyed by
/// `(is_reply, trans)` in [`super::TransactionCodec`]'s `partials`.
#[derive(Debug)]
pub(super) struct Partial {
    /// The most recently received part's header.
    pub(super) header: TransactionHeader,
    /// Every part's body bytes, in arrival order.
    pub(super) body: Vec<u8>,
}
