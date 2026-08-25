//! [`TransactionCodec`]: a [`tokio_util::codec::Decoder`]/[`tokio_util::codec::Encoder`] for
//! [`hotline_proto::Transaction`], used via `tokio_util::codec::Framed`
//! instead of a hand-rolled read-then-parse loop over a raw socket.
//!
//! `Decoder::decode` is called with whatever bytes have arrived so far
//! and returns `Ok(None)` to ask for more, so `Framed` handles TCP
//! fragmentation without relying on a single read returning a complete
//! frame. Callers get a `Stream<Item = Result<Transaction, _>>` +
//! `Sink<Transaction>`, used as `.next().await` / `.send(tx).await`.

mod transaction_codec;

pub use transaction_codec::{CodecError, TransactionCodec};
