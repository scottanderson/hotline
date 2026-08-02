//! The `TRTP` session-initialization handshake, sent once right after the
//! TCP connection is established and before any transaction traffic.
//!
//! The client's opening 12 bytes are the literal ASCII string
//! `"TRTPHOTL"` (protocol tag `TRTP` + sub-protocol tag `HOTL`, both
//! fixed) followed by a big-endian `u16` version (`1`) and `u16`
//! sub-version (`2`), not `"TRTP"` followed by a generic sub-protocol
//! field.

use crate::error::{ProtoError, Result};
use crate::wire::{FixedEncode, PacketWriter};

const MAGIC: &[u8; 4] = b"TRTP";

/// The fixed `"HOTL"` sub-protocol tag every client's hello uses.
pub const SUB_PROTOCOL_HOTLINE: [u8; 4] = *b"HOTL";

/// The 12-byte packet the client sends first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientHello {
    /// Sub-protocol tag. Always [`SUB_PROTOCOL_HOTLINE`] for a normal
    /// Hotline connection.
    pub sub_protocol: [u8; 4],
    /// Protocol major version. Pinned at `1` for wire compatibility.
    pub version: u16,
    /// Protocol minor/sub version. Pinned at `2`.
    pub sub_version: u16,
}

impl Default for ClientHello {
    fn default() -> Self {
        Self {
            sub_protocol: SUB_PROTOCOL_HOTLINE,
            version: 1,
            sub_version: 2,
        }
    }
}

impl ClientHello {
    /// Encode to the wire format: `"TRTP" | sub_protocol | version | sub_version`.
    pub fn encode(&self) -> [u8; 12] {
        PacketWriter::<12>::new()
            .bytes(MAGIC)
            .bytes(&self.sub_protocol)
            .u16(self.version)
            .u16(self.sub_version)
            .finish()
    }

    /// Decode a 12-byte client hello.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 12 {
            return Err(ProtoError::UnexpectedEof {
                needed: 12,
                available: buf.len(),
            });
        }
        if &buf[0..4] != MAGIC {
            return Err(ProtoError::BadMagic {
                expected: b"TRTP",
                got: [buf[0], buf[1], buf[2], buf[3]],
            });
        }
        Ok(Self {
            sub_protocol: buf[4..8].try_into().unwrap(),
            version: u16::from_be_bytes([buf[8], buf[9]]),
            sub_version: u16::from_be_bytes([buf[10], buf[11]]),
        })
    }
}

/// The 8-byte packet the server replies with: `"TRTP"` plus a 4-byte
/// error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerHello {
    /// `0` means the connection is accepted; any other value is an error
    /// code and the connection should be closed.
    pub error_code: u32,
}

impl ServerHello {
    /// A successful handshake reply.
    pub const OK: ServerHello = ServerHello { error_code: 0 };

    /// Encode to the wire format: `"TRTP" | error_code`.
    pub fn encode(&self) -> [u8; 8] {
        PacketWriter::<8>::new()
            .bytes(MAGIC)
            .u32(self.error_code)
            .finish()
    }

    /// Decode an 8-byte server hello. Returns `Err(ProtoError::Rejected)`
    /// if the server reported a nonzero error code.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < 8 {
            return Err(ProtoError::UnexpectedEof {
                needed: 8,
                available: buf.len(),
            });
        }
        if &buf[0..4] != MAGIC {
            return Err(ProtoError::BadMagic {
                expected: b"TRTP",
                got: [buf[0], buf[1], buf[2], buf[3]],
            });
        }
        let error_code = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if error_code != 0 {
            return Err(ProtoError::Rejected(error_code));
        }
        Ok(Self { error_code })
    }
}

impl FixedEncode<12> for ClientHello {
    fn encode(&self) -> [u8; 12] {
        ClientHello::encode(self)
    }
}

impl FixedEncode<8> for ServerHello {
    fn encode(&self) -> [u8; 8] {
        ServerHello::encode(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_hello_encodes_expected_bytes() {
        let hello = ClientHello::default();
        assert_eq!(&hello.encode(), b"TRTPHOTL\x00\x01\x00\x02");
    }

    #[test]
    fn client_hello_round_trips() {
        let hello = ClientHello::default();
        let bytes = hello.encode();
        assert_eq!(ClientHello::decode(&bytes).unwrap(), hello);
    }

    #[test]
    fn server_hello_ok_round_trips() {
        let bytes = ServerHello::OK.encode();
        assert_eq!(ServerHello::decode(&bytes).unwrap(), ServerHello::OK);
    }

    #[test]
    fn server_hello_rejection_is_an_error() {
        let bytes = ServerHello { error_code: 1 }.encode();
        assert!(matches!(
            ServerHello::decode(&bytes),
            Err(ProtoError::Rejected(1))
        ));
    }
}
