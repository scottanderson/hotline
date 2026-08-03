//! Error type shared by every decoder in this crate.

use thiserror::Error;

use crate::wire::PStringTooLong;

/// Convenience alias for results returned by `hotline-proto` decoders.
pub type Result<T> = std::result::Result<T, ProtoError>;

/// Everything that can go wrong turning bytes into protocol structures.
#[derive(Debug, Error)]
pub enum ProtoError {
    /// Fewer bytes were available than the structure being decoded requires.
    #[error("unexpected end of buffer: needed {needed} bytes, had {available}")]
    UnexpectedEof {
        /// Bytes required to finish decoding.
        needed: usize,
        /// Bytes actually available.
        available: usize,
    },

    /// A fixed magic number (e.g. `TRTP`) did not match. Every magic
    /// value this crate checks is 4 bytes, so both sides are fixed
    /// arrays rather than `Vec<u8>`/slices.
    #[error("bad magic: expected {expected:?}, got {got:?}")]
    BadMagic {
        /// The magic bytes we expected.
        expected: &'static [u8; 4],
        /// The magic bytes we actually read.
        got: [u8; 4],
    },

    /// The server (or client) rejected the handshake / login.
    #[error("protocol rejected the connection with error code {0}")]
    Rejected(u32),

    /// A string was too long to encode into a 1-byte-length-prefixed
    /// field (e.g. an `HlPath` component).
    #[error(transparent)]
    StringTooLong(#[from] PStringTooLong),

    /// A field's data, or a transaction's field count, exceeded what a
    /// `u16` length prefix can represent (65535). Reachable with
    /// attacker-controlled input (e.g. relaying two oversized fields
    /// combined into one).
    #[error("{what} is {len}, too long for a u16 length prefix (max 65535)")]
    TooLong {
        /// What was too long (e.g. `"field data"`, `"field count"`).
        what: &'static str,
        /// The actual length.
        len: usize,
    },

    /// Catch-all for structurally invalid data (bad enum discriminant, etc).
    #[error("malformed data: {0}")]
    Malformed(String),
}
