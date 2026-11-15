use hotline_proto::text_encoding::Encoding;
use hotline_proto::wire::ArrayReader;

use crate::{REPLY_HEADER_LEN, Result, TrackedServer, TrackerError, decode_reply_header};

/// Message type a tracker stamps on each chunk of its server list.
const TYPE_SERVER_LIST: u16 = 1;

/// `msg_type(2) | data_size(2) | total_servers(2) | chunk_servers(2)`,
/// the header on each chunk of a reply.
pub const CHUNK_HEADER_LEN: usize = 8;

/// `msg_type(2) | data_size(2)`, the chunk bytes `data_size` does not count.
const DATA_SIZE_END: usize = 4;

/// The header on one chunk of a tracker's server-list reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ChunkHeader {
    /// Bytes after this field, so the chunk spans `DATA_SIZE_END + data_size`.
    data_size: u16,
    /// Servers across every chunk of this reply.
    total_servers: u16,
    /// Servers in this chunk alone.
    chunk_servers: u16,
}

impl ChunkHeader {
    fn decode(buf: &[u8]) -> Result<Self> {
        let prefix: [u8; CHUNK_HEADER_LEN] = buf
            .get(..CHUNK_HEADER_LEN)
            .map(TryInto::try_into)
            .and_then(std::result::Result::ok)
            .ok_or(TrackerError::UnexpectedEof {
                needed: CHUNK_HEADER_LEN,
                available: buf.len(),
            })?;

        let mut r = ArrayReader::new(prefix);
        let msg_type = r.u16();
        let data_size = r.u16();
        let total_servers = r.u16();
        let chunk_servers = r.u16();
        r.finish();

        if msg_type != TYPE_SERVER_LIST {
            return Err(TrackerError::UnexpectedMessageType(msg_type));
        }
        // `data_size` spans `total_servers` and `chunk_servers` as well.
        if usize::from(data_size) < CHUNK_HEADER_LEN - DATA_SIZE_END {
            return Err(TrackerError::MalformedChunk {
                declared: data_size,
                servers: chunk_servers,
            });
        }
        Ok(Self {
            data_size,
            total_servers,
            chunk_servers,
        })
    }
}

/// A tracker's decoded reply to [`crate::encode_client_request_v1`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerList {
    /// How many servers the tracker says it holds, which can exceed
    /// `servers.len()` when the reply was cut short.
    pub total: u16,
    /// Every server decoded, across all of the reply's chunks.
    pub servers: Vec<TrackedServer>,
}

/// Decode a whole tracker reply: the `HTRK` header, then one or more
/// chunks of server entries.
///
/// A tracker splits its list into chunks of at most 8192 bytes
/// (openline's `SendServerList`).
pub fn decode_server_list(buf: &[u8], encoding: &'static Encoding) -> Result<ServerList> {
    decode_reply_header(buf)?;

    let mut offset = REPLY_HEADER_LEN;
    let mut total = 0;
    let mut servers = Vec::new();

    while offset < buf.len() {
        let header = ChunkHeader::decode(buf.get(offset..).unwrap_or_default())?;
        total = header.total_servers;

        let end = offset
            .saturating_add(DATA_SIZE_END)
            .saturating_add(usize::from(header.data_size));
        let mut entries = buf
            .get(offset.saturating_add(CHUNK_HEADER_LEN)..end)
            .ok_or(TrackerError::UnexpectedEof {
                needed: end,
                available: buf.len(),
            })?;

        for _ in 0..header.chunk_servers {
            let (server, consumed) = TrackedServer::decode(entries, encoding).map_err(|_| {
                TrackerError::MalformedChunk {
                    declared: header.data_size,
                    servers: header.chunk_servers,
                }
            })?;
            servers.push(server);
            entries = entries.get(consumed..).unwrap_or_default();
        }
        offset = end;
    }

    Ok(ServerList { total, servers })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::text_encoding;
    use hotline_proto::wire::VecWriter;

    /// One chunk's worth of entry bytes, as a tracker would write them.
    fn entries(servers: &[(&str, &str)]) -> Vec<u8> {
        let encoding = text_encoding::DEFAULT;
        let mut out = Vec::new();
        for (name, description) in servers {
            out.extend(
                VecWriter::new()
                    .bytes(&[10, 0, 0, 1])
                    .u16(5500)
                    .u16(7)
                    .u16(0)
                    .with_encoding(encoding)
                    .pstring(name)
                    .unwrap()
                    .pstring(description)
                    .unwrap()
                    .finish(),
            );
        }
        out
    }

    fn chunk(total: u16, servers: &[(&str, &str)]) -> Vec<u8> {
        let body = entries(servers);
        let data_size = u16::try_from(4 + body.len()).unwrap();
        VecWriter::new()
            .u16(TYPE_SERVER_LIST)
            .u16(data_size)
            .u16(total)
            .u16(u16::try_from(servers.len()).unwrap())
            .bytes(&body)
            .finish()
    }

    #[test]
    fn a_single_chunk_reply_decodes() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(chunk(2, &[("One", "first"), ("Two", "second")]));

        let list = decode_server_list(&buf, encoding).unwrap();

        assert_eq!(list.total, 2);
        assert_eq!(list.servers.len(), 2);
        assert_eq!(&*list.servers[0].name, "One");
        assert_eq!(&*list.servers[1].description, "second");
    }

    /// A tracker splits a long list, so the decoder reads past the first
    /// chunk.
    #[test]
    fn a_split_reply_decodes_every_chunk() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(chunk(3, &[("One", "first")]));
        buf.extend(chunk(3, &[("Two", "second"), ("Three", "third")]));

        let list = decode_server_list(&buf, encoding).unwrap();

        assert_eq!(list.total, 3);
        let names: Vec<_> = list.servers.iter().map(|s| &*s.name).collect();
        assert_eq!(names, ["One", "Two", "Three"]);
    }

    #[test]
    fn an_empty_list_decodes_to_no_servers() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(chunk(0, &[]));

        let list = decode_server_list(&buf, encoding).unwrap();

        assert_eq!(list.total, 0);
        assert!(list.servers.is_empty());
    }

    #[test]
    fn a_truncated_entry_is_rejected() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(chunk(1, &[("One", "first")]));
        buf.truncate(buf.len() - 3);

        assert!(matches!(
            decode_server_list(&buf, encoding),
            Err(TrackerError::UnexpectedEof { .. })
        ));
    }

    /// `data_size` frames the chunk, so padding after the last entry
    /// must not shift where the next chunk is read from.
    #[test]
    fn padding_after_a_chunks_entries_is_skipped() {
        let encoding = text_encoding::DEFAULT;
        let body = entries(&[("One", "first")]);
        let mut padded = VecWriter::new()
            .u16(TYPE_SERVER_LIST)
            .u16(u16::try_from(4 + body.len() + 3).unwrap())
            .u16(2)
            .u16(1)
            .bytes(&body)
            .finish();
        padded.extend([0, 0, 0]);

        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(padded);
        buf.extend(chunk(2, &[("Two", "second")]));

        let list = decode_server_list(&buf, encoding).unwrap();

        let names: Vec<_> = list.servers.iter().map(|s| &*s.name).collect();
        assert_eq!(names, ["One", "Two"]);
    }

    /// mhxd's own chunk splitting compares a hash bucket index with a
    /// server count, so a chunk's size and its entry count can disagree.
    #[test]
    fn entries_overrunning_their_chunk_are_rejected() {
        let encoding = text_encoding::DEFAULT;
        let body = entries(&[("One", "first")]);
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(
            VecWriter::new()
                .u16(TYPE_SERVER_LIST)
                .u16(u16::try_from(4 + body.len()).unwrap())
                .u16(2)
                .u16(2) // one more entry than the chunk holds
                .bytes(&body)
                .finish(),
        );

        assert!(matches!(
            decode_server_list(&buf, encoding),
            Err(TrackerError::MalformedChunk { servers: 2, .. })
        ));
    }

    /// `data_size` counts from just past itself, so it must cover at
    /// least `total_servers` and `chunk_servers`.
    #[test]
    fn a_chunk_declaring_less_than_its_own_header_is_rejected() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(
            VecWriter::new()
                .u16(TYPE_SERVER_LIST)
                .u16(0)
                .u16(1)
                .u16(1)
                .finish(),
        );

        assert!(matches!(
            decode_server_list(&buf, encoding),
            Err(TrackerError::MalformedChunk { .. })
        ));
    }

    /// Every byte past the magic comes from an unauthenticated peer, so
    /// no input may panic. The chunk type is held valid so the sweep
    /// reaches the entry walk.
    #[test]
    fn arbitrary_chunk_headers_error_but_never_panic() {
        let encoding = text_encoding::DEFAULT;
        let mut seed = 0x1234_5678_9abc_def0_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed as u8
        };

        for _ in 0..20_000 {
            let mut buf = b"HTRK\x00\x01".to_vec();
            for _ in 0..=(next() % 3) {
                buf.extend([0, 1]); // a chunk this decoder accepts
                buf.extend([next() % 2, next()]); // data_size
                buf.extend([next() % 2, next()]); // total_servers
                buf.extend([0, next() % 4]); // chunk_servers
                buf.extend((0..next() % 32).map(|_| next()));
            }
            let _ = decode_server_list(&buf, encoding);
        }
    }

    #[test]
    fn an_unknown_message_type_is_rejected() {
        let encoding = text_encoding::DEFAULT;
        let mut buf = b"HTRK\x00\x01".to_vec();
        buf.extend(VecWriter::new().u16(9).u16(4).u16(0).u16(0).finish());

        assert!(matches!(
            decode_server_list(&buf, encoding),
            Err(TrackerError::UnexpectedMessageType(9))
        ));
    }
}
