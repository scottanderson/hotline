//! [`ServerHandshake`]: the 8-byte packet the server replies with.

use crate::error::{ProtoError, Result};
use crate::handshake::MAGIC;
use crate::wire::{PacketReader, PacketWriter};

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
        let magic = r.take();
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
