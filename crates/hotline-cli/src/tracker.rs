//! `HTRK` tracker query: list the servers a tracker knows about.

use std::fmt::Write as _;
use std::net::Ipv4Addr;
use std::time::Duration;

use anyhow::{Context, bail};
use hotline_proto::text_encoding::Encoding;
use hotline_tracker::{
    CHUNK_HEADER_LEN, REPLY_HEADER_LEN, ServerList, TCP_PORT, TrackerError, decode_server_list,
    encode_client_request_v1, with_default_port,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Give up on a tracker that stops sending mid-list.
const REPLY_TIMEOUT: Duration = Duration::from_secs(15);

/// Refuse a reply larger than this rather than buffering it forever.
const MAX_REPLY_LEN: usize = 4 * 1024 * 1024;

/// Ask `address` for its server list.
///
/// A tracker doesn't close after sending, so this reads until the list
/// is complete.
pub async fn query(address: &str, encoding: &'static Encoding) -> anyhow::Result<ServerList> {
    query_until(address, encoding, REPLY_TIMEOUT).await
}

/// [`query`], with the stall deadline spelled out.
async fn query_until(
    address: &str,
    encoding: &'static Encoding,
    reply_timeout: Duration,
) -> anyhow::Result<ServerList> {
    let address = with_default_port(address, TCP_PORT);
    let mut stream = TcpStream::connect(&address)
        .await
        .with_context(|| format!("failed to connect to tracker {address}"))?;
    stream
        .write_all(&encode_client_request_v1())
        .await
        .context("failed to send the tracker request")?;

    let now = tokio::time::Instant::now();
    let deadline = now.checked_add(reply_timeout).unwrap_or(now);
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut partial = None;

    loop {
        // The `HTRK` header on its own decodes as a valid empty list.
        if buf.len() >= REPLY_HEADER_LEN + CHUNK_HEADER_LEN {
            match decode_server_list(&buf, encoding) {
                Ok(list) if usize::from(list.total) <= list.servers.len() => return Ok(list),
                Ok(list) => partial = Some(list),
                Err(TrackerError::UnexpectedEof { .. }) => {}
                Err(e) => return Err(e).context("tracker sent a reply this client can't read"),
            }
        }

        if buf.len() > MAX_REPLY_LEN {
            bail!("tracker reply exceeded {MAX_REPLY_LEN} bytes");
        }

        let read = match tokio::time::timeout_at(deadline, stream.read(&mut chunk)).await {
            Ok(read) => read.context("failed to read from the tracker")?,
            // A tracker can claim more servers than it sends.
            Err(_) => return partial.context("timed out waiting for the tracker's server list"),
        };
        if read == 0 {
            // EOF: hand back whatever decoded, or report the truncation.
            return decode_server_list(&buf, encoding)
                .or_else(|e| partial.ok_or(e))
                .context("tracker closed the connection mid-list");
        }
        buf.extend_from_slice(chunk.get(..read).unwrap_or_default());
    }
}

/// One server per line: address, users, name, then description.
pub fn format_list(list: &ServerList) -> String {
    let mut out = format!("{} server(s)\n", list.total);
    for server in &list.servers {
        let ip = Ipv4Addr::from(server.ip);
        let _ = write!(
            out,
            "{:<21} {:>4} users  {}",
            format!("{ip}:{}", server.port),
            server.user_count,
            server.name,
        );
        if !server.description.is_empty() {
            let _ = write!(out, "  {}", server.description);
        }
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hotline_proto::text_encoding;
    use hotline_tracker::TrackedServer;
    use tokio::net::TcpListener;

    /// Serve `reply` in the given writes, then stay open like a real
    /// tracker does, and return the address to query.
    async fn fake_tracker(writes: Vec<Vec<u8>>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 6];
            stream.read_exact(&mut request).await.unwrap();
            for write in writes {
                stream.write_all(&write).await.unwrap();
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        addr
    }

    fn chunk(total: u16, names: &[&str]) -> Vec<u8> {
        let mut body = Vec::new();
        for name in names {
            body.extend([10, 0, 0, 1]);
            body.extend(5500u16.to_be_bytes());
            body.extend(0u16.to_be_bytes());
            body.extend(0u16.to_be_bytes());
            body.push(u8::try_from(name.len()).unwrap());
            body.extend(name.as_bytes());
            body.push(0); // empty description
        }
        let mut out = 1u16.to_be_bytes().to_vec();
        out.extend(u16::try_from(4 + body.len()).unwrap().to_be_bytes());
        out.extend(total.to_be_bytes());
        out.extend(u16::try_from(names.len()).unwrap().to_be_bytes());
        out.extend(body);
        out
    }

    /// The `HTRK` header alone decodes as a valid empty list, so a
    /// reader that accepts it returns before any server arrives.
    #[tokio::test]
    async fn a_header_only_read_does_not_end_the_query() {
        let addr = fake_tracker(vec![b"HTRK\x00\x01".to_vec(), chunk(2, &["Alpha", "Beta"])]).await;

        let list = query(&addr, text_encoding::DEFAULT).await.unwrap();

        assert_eq!(list.total, 2);
        let names: Vec<_> = list.servers.iter().map(|s| &*s.name).collect();
        assert_eq!(names, ["Alpha", "Beta"]);
    }

    /// A tracker splits a long list across chunks and doesn't close
    /// afterwards, so the query must keep reading until `total` is met.
    #[tokio::test]
    async fn a_split_reply_is_read_to_completion() {
        let addr = fake_tracker(vec![
            b"HTRK\x00\x01".to_vec(),
            chunk(3, &["Alpha"]),
            chunk(3, &["Beta", "Gamma"]),
        ])
        .await;

        let list = query(&addr, text_encoding::DEFAULT).await.unwrap();

        assert_eq!(list.servers.len(), 3);
    }

    #[tokio::test]
    async fn a_genuinely_empty_list_returns_rather_than_hanging() {
        let addr = fake_tracker(vec![b"HTRK\x00\x01".to_vec(), chunk(0, &[])]).await;

        let list = query(&addr, text_encoding::DEFAULT).await.unwrap();

        assert_eq!(list.total, 0);
        assert!(list.servers.is_empty());
    }

    /// A tracker can claim more servers than it sends. The servers that
    /// arrive are the answer.
    #[tokio::test]
    async fn a_short_list_is_returned_once_the_tracker_stalls() {
        let addr = fake_tracker(vec![b"HTRK\x00\x01".to_vec(), chunk(3, &["Alpha"])]).await;

        let list = query_until(&addr, text_encoding::DEFAULT, Duration::from_millis(300))
            .await
            .unwrap();

        assert_eq!(list.total, 3);
        assert_eq!(list.servers.len(), 1);
    }

    /// Nothing decoded and nothing coming is still a failure.
    #[tokio::test]
    async fn a_silent_tracker_is_an_error() {
        let addr = fake_tracker(Vec::new()).await;

        let error = query_until(&addr, text_encoding::DEFAULT, Duration::from_millis(300))
            .await
            .unwrap_err();

        assert!(error.to_string().contains("timed out"), "{error}");
    }

    #[test]
    fn a_listing_shows_address_users_and_name() {
        let list = ServerList {
            total: 1,
            servers: vec![TrackedServer {
                ip: [10, 0, 0, 1],
                port: 5500,
                user_count: 7,
                name: "Test".into(),
                description: "desc".into(),
            }],
        };

        let text = format_list(&list);

        assert!(text.starts_with("1 server(s)\n"), "{text}");
        assert!(text.contains("10.0.0.1:5500"), "{text}");
        assert!(text.contains("7 users"), "{text}");
        assert!(text.contains("Test  desc"), "{text}");
    }

    /// The count line shows the tracker's `total`, which can exceed what
    /// arrived.
    #[test]
    fn a_short_list_still_reports_the_trackers_own_total() {
        let list = ServerList {
            total: 9,
            servers: Vec::new(),
        };
        assert_eq!(format_list(&list), "9 server(s)\n");
    }
}
