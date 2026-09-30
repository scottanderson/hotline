use hotline_proto::text_encoding::{self, Encoding};
use hotline_proto::wire::PacketReader;

use crate::{Result, TrackerError};

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
    pub name: Box<str>,
    /// Server description.
    pub description: Box<str>,
}

/// Read a Pascal-style (1-byte length prefix) string starting at `offset`,
/// returning it along with the offset just past it.
fn read_pstring(buf: &[u8], offset: usize, encoding: &'static Encoding) -> Result<(String, usize)> {
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
    Ok((text_encoding::decode(encoding, &buf[start..end]), end))
}

impl TrackedServer {
    /// Decode one entry from a tracker server-list reply, returning the
    /// entry and bytes consumed.
    pub fn decode(buf: &[u8], encoding: &'static Encoding) -> Result<(Self, usize)> {
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
        let (name, offset) = read_pstring(buf, PREFIX_LEN, encoding)?;
        let (description, offset) = read_pstring(buf, offset, encoding)?;

        Ok((
            Self {
                ip,
                port,
                user_count,
                name: name.into(),
                description: description.into(),
            },
            offset,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::wire::VecWriter;

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

        let (server, consumed) = TrackedServer::decode(&buf, encoding).unwrap();
        assert_eq!(consumed, buf.len());
        assert_eq!(server.ip, [127, 0, 0, 1]);
        assert_eq!(server.port, 5500);
        assert_eq!(&*server.name, "Test Server");
        assert_eq!(&*server.description, "A description");
    }
}
