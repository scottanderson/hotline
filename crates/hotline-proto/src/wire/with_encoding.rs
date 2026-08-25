//! [`WithEncoding`]: [`crate::wire::PacketBuilder`]'s post-encoding typestate.

use crate::text_encoding;

/// [`crate::wire::PacketBuilder`]'s `E` state once
/// [`crate::wire::PacketBuilder::with_encoding`] has run: unlocks `pstring`.
#[derive(Debug, Clone, Copy)]
pub struct WithEncoding(pub(crate) &'static text_encoding::Encoding);
