//! The `HTRK` tracker protocol: how a client asks a tracker for a server
//! list, and how a server periodically beacons itself over UDP. Kept
//! separate from `hotline-proto`'s transaction framing, a different wire
//! shape entirely, but reuses its `PacketWriter`/`PacketReader`/`VecWriter`.

mod error;
mod server_beacon;
mod tracked_server;

use hotline_proto::wire::{PacketReader, PacketWriter};

pub use error::{Result, TrackerError};
pub use server_beacon::ServerBeacon;
pub use tracked_server::TrackedServer;

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
    fn reply_header_rejects_bad_magic() {
        let bytes = b"NOPE\x00\x01";
        assert!(matches!(
            decode_reply_header(bytes),
            Err(TrackerError::BadMagic)
        ));
    }
}
