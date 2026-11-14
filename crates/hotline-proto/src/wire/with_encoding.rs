//! [`WithEncoding`]: [`crate::wire::Writer`]'s post-encoding typestate.

use crate::text_encoding;

/// [`crate::wire::Writer`]'s `E` state once
/// [`crate::wire::Writer::with_encoding`] has run: unlocks `pstring`.
#[derive(Debug, Clone, Copy)]
pub struct WithEncoding(pub(crate) &'static text_encoding::Encoding);
