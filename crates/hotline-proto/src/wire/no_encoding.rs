//! [`NoEncoding`]: [`crate::wire::PacketBuilder`]'s pre-encoding typestate.

/// [`crate::wire::PacketBuilder`]'s `E` state before
/// [`crate::wire::PacketBuilder::with_encoding`] has run: `pstring` isn't
/// available yet.
#[derive(Debug, Default)]
pub struct NoEncoding;
