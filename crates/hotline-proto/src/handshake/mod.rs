//! The `TRTP` session-initialization handshake, sent once right after the
//! TCP connection is established and before any transaction traffic.
//!
//! The client's opening 12 bytes are the literal ASCII string
//! `"TRTPHOTL"` (protocol tag `TRTP` + sub-protocol tag `HOTL`, both
//! fixed) followed by a big-endian `u16` version (`1`) and `u16`
//! sub-version (`2`), not `"TRTP"` followed by a generic sub-protocol
//! field.

mod client;
mod server;

pub use client::ClientHandshake;
pub use server::ServerHandshake;

pub(crate) const MAGIC: [u8; 4] = *b"TRTP";

/// The fixed `"HOTL"` sub-protocol tag every client's handshake uses.
pub const SUB_PROTOCOL_HOTLINE: [u8; 4] = *b"HOTL";
