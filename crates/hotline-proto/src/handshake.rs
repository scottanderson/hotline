//! The `TRTP` session-initialization handshake, sent once right after the
//! TCP connection is established and before any transaction traffic.
//!
//! The client's opening 12 bytes are the literal ASCII string
//! `"TRTPHOTL"` (protocol tag `TRTP` + sub-protocol tag `HOTL`, both
//! fixed) followed by a big-endian `u16` version (`1`) and `u16`
//! sub-version (`2`), not `"TRTP"` followed by a generic sub-protocol
//! field.

use crate::error::{ProtoError, Result};
use crate::wire::{PacketReader, PacketWriter};

const MAGIC: [u8; 4] = *b"TRTP";

/// The fixed `"HOTL"` sub-protocol tag every client's handshake uses.
pub const SUB_PROTOCOL_HOTLINE: [u8; 4] = *b"HOTL";

/// The 12-byte packet the client sends first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHandshake {
    /// Sub-protocol tag. Always [`SUB_PROTOCOL_HOTLINE`] for a normal
    /// Hotline connection.
    pub sub_protocol: [u8; 4],
    /// Protocol major version. Pinned at `1` for wire compatibility.
    pub version: u16,
    /// Protocol minor/sub version. Pinned at `2`.
    pub sub_version: u16,
}

impl Default for ClientHandshake {
    fn default() -> Self {
        Self {
            sub_protocol: SUB_PROTOCOL_HOTLINE,
            version: 1,
            sub_version: 2,
        }
    }
}

impl ClientHandshake {
    /// Wire length in bytes: `magic(4) | sub_protocol(4) | version(2) |
    /// sub_version(2)`.
    pub const LEN: usize = 12;

    /// Encode to the wire format: `"TRTP" | sub_protocol | version | sub_version`.
    pub fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .bytes(&MAGIC)
            .bytes(&self.sub_protocol)
            .u16(self.version)
            .u16(self.sub_version)
            .finish()
    }

    /// Decode a 12-byte client handshake.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let buf: [u8; Self::LEN] = buf
            .get(..Self::LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            })?;
        let mut r = PacketReader::new(buf);
        let protocol = r.take::<4>();
        let sub_protocol = r.take::<4>();
        let version = r.u16();
        let sub_version = r.u16();
        r.finish();
        if protocol != MAGIC {
            return Err(ProtoError::BadMagic {
                expected: MAGIC,
                got: protocol,
            });
        }
        Ok(Self {
            sub_protocol,
            version,
            sub_version,
        })
    }
}

/// The 8-byte packet the server replies with: `"TRTP"` plus a 4-byte
/// error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHandshake {
    /// `0` means the connection is accepted; any other value is an error
    /// code and the connection should be closed.
    pub error_code: u32,
}

impl ServerHandshake {
    /// Wire length in bytes: `magic(4) | error_code(4)`.
    pub const LEN: usize = 8;

    /// A successful handshake reply.
    pub const OK: ServerHandshake = ServerHandshake { error_code: 0 };

    /// Encode to the wire format: `"TRTP" | error_code`.
    pub fn encode(&self) -> [u8; Self::LEN] {
        PacketWriter::new()
            .bytes(&MAGIC)
            .u32(self.error_code)
            .finish()
    }

    /// Decode an 8-byte server handshake. Returns `Err(ProtoError::Rejected)`
    /// if the server reported a nonzero error code.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let buf: [u8; Self::LEN] = buf
            .get(..Self::LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(ProtoError::UnexpectedEof {
                needed: Self::LEN,
                available: buf.len(),
            })?;
        let mut r = PacketReader::new(buf);
        let magic = r.take::<4>();
        let error_code = r.u32();
        r.finish();
        if magic != MAGIC {
            return Err(ProtoError::BadMagic {
                expected: MAGIC,
                got: magic,
            });
        }
        if error_code != 0 {
            return Err(ProtoError::Rejected(error_code));
        }
        Ok(Self { error_code })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_handshake_encodes_expected_bytes() {
        let handshake = ClientHandshake::default();
        assert_eq!(&handshake.encode(), b"TRTPHOTL\x00\x01\x00\x02");
    }

    #[test]
    fn client_handshake_round_trips() {
        let handshake = ClientHandshake::default();
        let bytes = handshake.encode();
        assert_eq!(ClientHandshake::decode(&bytes).unwrap(), handshake);
    }

    #[test]
    fn server_handshake_ok_round_trips() {
        let bytes = ServerHandshake::OK.encode();
        assert_eq!(
            ServerHandshake::decode(&bytes).unwrap(),
            ServerHandshake::OK
        );
    }

    #[test]
    fn server_handshake_rejection_is_an_error() {
        let bytes = ServerHandshake { error_code: 1 }.encode();
        assert!(matches!(
            ServerHandshake::decode(&bytes),
            Err(ProtoError::Rejected(1))
        ));
    }
}
