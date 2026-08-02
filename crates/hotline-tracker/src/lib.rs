//! The `HTRK` tracker protocol: how a client asks a tracker for a server
//! list, and how a server periodically beacons itself to a tracker over
//! UDP. Kept separate from `hotline-proto`'s transaction framing since
//! it's a different wire shape entirely (no transaction headers), but
//! reuses `hotline-proto::wire`'s `PacketWriter`/`PacketReader`/`VecWriter`
//! for the same reason every chunk in this crate benefits from them: no
//! hand-tracked byte offsets to get wrong.

use hotline_proto::wire::{PStringTooLong, PacketReader, PacketWriter, VecWriter};
use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, TrackerError>;

/// Errors decoding or encoding tracker traffic.
#[derive(Debug, Error)]
pub enum TrackerError {
    /// Buffer too short for the structure being decoded.
    #[error("unexpected end of buffer: needed {needed}, had {available}")]
    UnexpectedEof {
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },
    /// The `HTRK` magic number did not match.
    #[error("bad magic, expected HTRK")]
    BadMagic,
    /// A name/description/login/password was too long to encode.
    #[error(transparent)]
    StringTooLong(#[from] PStringTooLong),
}

const MAGIC: &[u8; 4] = b"HTRK";

/// The client's initial request to a tracker (version 1, "old protocol":
/// just the magic + version, no login).
pub fn encode_client_request_v1() -> [u8; 6] {
    PacketWriter::<6>::new().bytes(MAGIC).u16(1).finish()
}

/// Decode the fixed 8-byte header a tracker reply always starts with:
/// magic + version + message_type + message_size follow, but callers
/// typically only need magic/version checked before reading the rest
/// with a length-aware parser; this returns `(version)`.
pub fn decode_reply_header(buf: &[u8]) -> Result<u16> {
    let prefix: [u8; 6] =
        buf.get(..6)
            .and_then(|s| s.try_into().ok())
            .ok_or(TrackerError::UnexpectedEof {
                needed: 6,
                available: buf.len(),
            })?;

    let (reader, magic) = PacketReader::new(prefix).take::<4>();
    if &magic != MAGIC {
        return Err(TrackerError::BadMagic);
    }
    let (_, version) = reader.u16();
    Ok(version)
}

/// One entry in a tracker's server list reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackedServer {
    /// Server's IPv4 address.
    pub ip: [u8; 4],
    /// Server's Hotline (base) port.
    pub port: u16,
    /// Number of users currently connected.
    pub user_count: u16,
    /// Server name.
    pub name: String,
    /// Server description.
    pub description: String,
}

impl TrackedServer {
    /// Decode one entry from a tracker server-list reply, returning the
    /// entry and bytes consumed.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        // ip(4) port(2) users(2) reserved(2) name_len(1) name desc_len(1) desc
        if buf.len() < 12 {
            return Err(TrackerError::UnexpectedEof {
                needed: 12,
                available: buf.len(),
            });
        }
        let ip = [buf[0], buf[1], buf[2], buf[3]];
        let port = u16::from_be_bytes([buf[4], buf[5]]);
        let user_count = u16::from_be_bytes([buf[6], buf[7]]);
        let name_len = buf[10] as usize;
        let mut offset = 11;
        if buf.len() < offset + name_len + 1 {
            return Err(TrackerError::UnexpectedEof {
                needed: offset + name_len + 1,
                available: buf.len(),
            });
        }
        let name = String::from_utf8_lossy(&buf[offset..offset + name_len]).into_owned();
        offset += name_len;
        let desc_len = buf[offset] as usize;
        offset += 1;
        if buf.len() < offset + desc_len {
            return Err(TrackerError::UnexpectedEof {
                needed: offset + desc_len,
                available: buf.len(),
            });
        }
        let description = String::from_utf8_lossy(&buf[offset..offset + desc_len]).into_owned();
        offset += desc_len;

        Ok((
            Self {
                ip,
                port,
                user_count,
                name,
                description,
            },
            offset,
        ))
    }
}

/// A server's periodic UDP beacon to a tracker (new-protocol form, with
/// login/password).
#[derive(Debug, Clone)]
pub struct ServerBeacon {
    /// Server's UDP listening port (for the beacon itself).
    pub udp_port: u16,
    /// Number of connected users.
    pub user_count: u16,
    /// Random per-server "pass ID" the guide mentions, used by the
    /// tracker to correlate updates.
    pub pass_id: u32,
    /// Server name.
    pub name: String,
    /// Server description.
    pub description: String,
    /// Tracker account login.
    pub login: String,
    /// Tracker account password.
    pub password: String,
}

impl ServerBeacon {
    /// Encode the UDP payload sent to the tracker. Fails if any of
    /// `name`/`description`/`login`/`password` is longer than 255 bytes,
    /// since each is written as a Pascal-style (1-byte length prefix)
    /// string.
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(VecWriter::new()
            .u16(1)
            .u16(self.udp_port)
            .u16(self.user_count)
            .u16(0)
            .u32(self.pass_id)
            .pstring(&self.name)?
            .pstring(&self.description)?
            .pstring(&self.login)?
            .pstring(&self.password)?
            .finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracked_server_round_trips() {
        let buf = VecWriter::new()
            .bytes(&[127, 0, 0, 1])
            .u16(5500)
            .u16(3)
            .u16(0)
            .pstring("Test Server")
            .unwrap()
            .pstring("A description")
            .unwrap()
            .finish();

        let (server, consumed) = TrackedServer::decode(&buf).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(server.ip, [127, 0, 0, 1]);
        assert_eq!(server.port, 5500);
        assert_eq!(server.name, "Test Server");
        assert_eq!(server.description, "A description");
    }

    #[test]
    fn server_beacon_round_trips_through_encode() {
        let beacon = ServerBeacon {
            udp_port: 5499,
            user_count: 2,
            pass_id: 0xdead_beef,
            name: "Test Server".to_string(),
            description: "A description".to_string(),
            login: "".to_string(),
            password: "".to_string(),
        };
        let bytes = beacon.encode().unwrap();
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 1); // beacon version
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 5499);
    }

    #[test]
    fn server_beacon_encode_rejects_an_oversized_field() {
        let beacon = ServerBeacon {
            udp_port: 0,
            user_count: 0,
            pass_id: 0,
            name: "x".repeat(256),
            description: String::new(),
            login: String::new(),
            password: String::new(),
        };
        assert!(matches!(
            beacon.encode(),
            Err(TrackerError::StringTooLong(_))
        ));
    }

    #[test]
    fn client_request_v1_encodes_magic_and_version() {
        assert_eq!(&encode_client_request_v1(), b"HTRK\x00\x01");
    }

    #[test]
    fn reply_header_round_trips() {
        let bytes = encode_client_request_v1();
        assert_eq!(decode_reply_header(&bytes).unwrap(), 1);
    }

    #[test]
    fn reply_header_rejects_bad_magic() {
        let bytes = b"NOPE\x00\x01";
        assert!(matches!(
            decode_reply_header(bytes),
            Err(TrackerError::BadMagic)
        ));
    }
}
