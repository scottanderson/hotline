//! [`ClientHandshake`]: the 12-byte packet the client sends first.

use crate::error::{ProtoError, Result};
use crate::handshake::{MAGIC, SUB_PROTOCOL_HOTLINE};
use crate::wire::{PacketReader, PacketWriter};

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
        let protocol = r.take();
        let sub_protocol = r.take();
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
}
