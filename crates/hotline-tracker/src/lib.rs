//! The `HTRK` tracker protocol: how a client asks a tracker for a server
//! list, and how a server periodically beacons itself over UDP. Kept
//! separate from `hotline-proto`'s transaction framing, a different wire
//! shape entirely, but reuses its `ArrayWriter`/`ArrayReader`/`VecWriter`.

#![cfg_attr(test, allow(clippy::arithmetic_side_effects))]

mod error;
mod server_beacon;
mod server_list;
mod tracked_server;

use hotline_proto::wire::{ArrayReader, ArrayWriter};

pub use error::{Result, TrackerError};
pub use server_beacon::ServerBeacon;
pub use server_list::{CHUNK_HEADER_LEN, ServerList, decode_server_list};
pub use tracked_server::TrackedServer;

const MAGIC: &[u8; 4] = b"HTRK";

/// Port a tracker answers client queries on.
pub const TCP_PORT: u16 = 5498;

/// Port a tracker listens for server beacons on.
pub const UDP_PORT: u16 = 5499;

/// `magic(4) | version(2)`, the fixed prefix on a tracker reply.
pub const REPLY_HEADER_LEN: usize = 6;

/// Add `default_port` to a bare `host`.
pub fn with_default_port(address: &str, default_port: u16) -> String {
    if address.contains(':') {
        address.to_string()
    } else {
        format!("{address}:{default_port}")
    }
}

/// The client's initial request to a tracker (version 1, "old protocol":
/// just the magic + version, no login).
pub fn encode_client_request_v1() -> [u8; 6] {
    ArrayWriter::new().bytes(MAGIC).u16(1).finish()
}

/// Decode the fixed 6-byte header a tracker reply starts with, checking
/// the magic and returning the version.
pub fn decode_reply_header(buf: &[u8]) -> Result<u16> {
    let prefix: [u8; REPLY_HEADER_LEN] = buf
        .get(..REPLY_HEADER_LEN)
        .map(TryInto::try_into)
        .and_then(std::result::Result::ok)
        .ok_or(TrackerError::UnexpectedEof {
            needed: REPLY_HEADER_LEN,
            available: buf.len(),
        })?;

    let mut reader = ArrayReader::new(prefix);
    let magic = reader.take();
    let version = reader.u16();
    reader.finish();

    if &magic != MAGIC {
        return Err(TrackerError::BadMagic);
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_bare_host_takes_the_default_port() {
        assert_eq!(
            with_default_port("tracker.example", 5498),
            "tracker.example:5498"
        );
        assert_eq!(with_default_port("10.0.0.1:1", 5498), "10.0.0.1:1");
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

#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
