use hotline_proto::text_encoding::Encoding;
use hotline_proto::wire::VecWriter;

use crate::Result;

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
    use crate::TrackerError;
    use hotline_proto::text_encoding;

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
}
