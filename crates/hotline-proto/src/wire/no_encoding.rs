//! [`NoEncoding`]: [`crate::wire::Writer`]'s pre-encoding typestate.

/// [`crate::wire::Writer`]'s `E` state before
/// [`crate::wire::Writer::with_encoding`] has run: `pstring` isn't
/// available yet.
#[derive(Debug, Default)]
pub struct NoEncoding;
