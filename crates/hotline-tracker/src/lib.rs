//! The `HTRK` tracker protocol: how a client asks a tracker for a server
//! list, and how a server periodically beacons itself over UDP. Kept
//! separate from `hotline-proto`'s transaction framing, a different wire
//! shape entirely, but reuses its `PacketWriter`/`PacketReader`/`VecWriter`.

use hotline_proto::text_encoding::Encoding;
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
    PacketWriter::new().bytes(MAGIC).u16(1).finish()
}

/// Decode the fixed 6-byte header a tracker reply starts with, checking
/// the magic and returning the version.
pub fn decode_reply_header(buf: &[u8]) -> Result<u16> {
    const HEADER_LEN: usize = 6;
    let prefix: [u8; HEADER_LEN] = buf
        .get(..HEADER_LEN)
        .map(TryInto::try_into)
        .and_then(std::result::Result::ok)
        .ok_or(TrackerError::UnexpectedEof {
            needed: HEADER_LEN,
            available: buf.len(),
        })?;

    let mut reader = PacketReader::new(prefix);
    let magic = reader.take::<4>();
    let version = reader.u16();
    reader.finish();

    if &magic != MAGIC {
        return Err(TrackerError::BadMagic);
    }
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

/// Read a Pascal-style (1-byte length prefix) string starting at `offset`,
/// returning it along with the offset just past it.
fn read_pstring(buf: &[u8], offset: usize) -> Result<(String, usize)> {
    let len = *buf.get(offset).ok_or(TrackerError::UnexpectedEof {
        needed: offset + 1,
        available: buf.len(),
    })? as usize;
    let start = offset + 1;
    let end = start + len;
    if buf.len() < end {
        return Err(TrackerError::UnexpectedEof {
            needed: end,
            available: buf.len(),
        });
    }
    Ok((String::from_utf8_lossy(&buf[start..end]).into_owned(), end))
}

impl TrackedServer {
    /// Decode one entry from a tracker server-list reply, returning the
    /// entry and bytes consumed.
    pub fn decode(buf: &[u8]) -> Result<(Self, usize)> {
        // ip(4) port(2) users(2) reserved(2) name_len(1) name desc_len(1) desc
        const PREFIX_LEN: usize = 10;
        let prefix: [u8; PREFIX_LEN] = buf
            .get(..PREFIX_LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(TrackerError::UnexpectedEof {
                needed: PREFIX_LEN,
                available: buf.len(),
            })?;
        let mut r = PacketReader::new(prefix);
        let ip = r.take::<4>();
        let port = r.u16();
        let user_count = r.u16();
        let _reserved = r.u16();
        r.finish();
        let (name, offset) = read_pstring(buf, PREFIX_LEN)?;
        let (description, offset) = read_pstring(buf, offset)?;

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
    /// Server's Hotline (TCP) listening port.
    pub port: u16,
    /// Number of connected users.
    pub user_count: u16,
    /// Random per-server ID the tracker uses to correlate updates.
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
    /// `name`/`description`/`login`/`password` is longer than 255 bytes.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(VecWriter::new()
            .u16(1)
            .u16(self.port)
            .u16(self.user_count)
            .u16(0)
            .u32(self.pass_id)
            .with_encoding(encoding)
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
    use hotline_proto::text_encoding;

    #[test]
    fn tracked_server_round_trips() {
        let encoding = text_encoding::DEFAULT;
        let buf = VecWriter::new()
            .bytes(&[127, 0, 0, 1])
            .u16(5500)
            .u16(3)
            .u16(0)
            .with_encoding(encoding)
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
        let encoding = text_encoding::DEFAULT;
        let beacon = ServerBeacon {
            port: 5500,
            user_count: 2,
            pass_id: 0xdead_beef,
            name: "Test Server".to_string(),
            description: "A description".to_string(),
            login: "".to_string(),
            password: "".to_string(),
        };
        let bytes = beacon.encode(encoding).unwrap();
        assert_eq!(u16::from_be_bytes([bytes[0], bytes[1]]), 1); // beacon version
        assert_eq!(u16::from_be_bytes([bytes[2], bytes[3]]), 5500);
    }

    #[test]
    fn server_beacon_encode_rejects_an_oversized_field() {
        let encoding = text_encoding::DEFAULT;
        let beacon = ServerBeacon {
            port: 0,
            user_count: 0,
            pass_id: 0,
            name: "x".repeat(256),
            description: String::new(),
            login: String::new(),
            password: String::new(),
        };
        assert!(matches!(
            beacon.encode(encoding),
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
