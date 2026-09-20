use hotline_proto::text_encoding::Encoding;
use hotline_proto::wire::VecWriter;

use crate::Result;

/// Message type for a beacon that registers or refreshes a server.
/// openline's `ProcessRegister` ignores type 2 (remove).
const TYPE_ADD: u16 = 1;

/// A server's periodic UDP beacon to a tracker.
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
    /// Tracker password, empty if the tracker takes unauthenticated
    /// registrations.
    pub password: String,
}

impl ServerBeacon {
    /// Encode the UDP payload sent to the tracker. Fails if any of
    /// `name`/`description`/`password` is longer than 255 bytes.
    pub fn encode(&self, encoding: &'static Encoding) -> Result<Vec<u8>> {
        Ok(VecWriter::new()
            .u16(TYPE_ADD)
            .u16(self.port)
            .u16(self.user_count)
            .u16(0) // flags
            .u32(self.pass_id)
            .with_encoding(encoding)
            .pstring(&self.name)?
            .pstring(&self.description)?
            .pstring(&self.password)?
            .finish())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TrackerError;
    use hotline_proto::text_encoding;

    /// openline's `ProcessRegister` reads
    /// `type | port | userCount | flags | passID` then three pstrings,
    /// in that order and with nothing between them.
    #[test]
    fn server_beacon_encodes_fields_in_order() {
        let beacon = ServerBeacon {
            port: 5500,
            user_count: 2,
            pass_id: 0xdead_beef,
            name: "Test".to_string(),
            description: "Desc".to_string(),
            password: "pw".to_string(),
        };

        let bytes = beacon.encode(text_encoding::DEFAULT).unwrap();

        let mut expected = vec![
            0x00, 0x01, // type: add
            0x15, 0x7c, // port 5500
            0x00, 0x02, // user count
            0x00, 0x00, // flags
            0xde, 0xad, 0xbe, 0xef, // pass ID
        ];
        expected.extend([4, b'T', b'e', b's', b't']);
        expected.extend([4, b'D', b'e', b's', b'c']);
        expected.extend([2, b'p', b'w']);
        assert_eq!(bytes, expected);
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
            password: String::new(),
        };
        assert!(matches!(
            beacon.encode(encoding),
            Err(TrackerError::StringTooLong(_))
        ));
    }
}
