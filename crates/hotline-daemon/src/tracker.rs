//! Periodic `HTRK` registration with one or more trackers.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use hotline_proto::text_encoding;
use hotline_server::SharedState;
use hotline_tracker::{ServerBeacon, UDP_PORT, with_default_port};
use tokio::net::UdpSocket;

/// How often to re-register, like openline's `mTrackerTimer`.
pub const BEACON_INTERVAL: Duration = Duration::from_secs(300);

/// One tracker to register with, and the shared secret it expects.
#[derive(Debug, Clone)]
pub struct TrackerTarget {
    /// `host` or `host:port`; the port defaults to [`UDP_PORT`].
    pub address: Box<str>,
    /// Tracker password, empty when the tracker takes unauthenticated
    /// registrations.
    pub password: Box<str>,
}

impl TrackerTarget {
    /// Parse a `[password@]host[:port]` argument.
    pub fn parse(value: &str) -> Self {
        let (password, address) = value.rsplit_once('@').unwrap_or(("", value));
        let address = with_default_port(address, UDP_PORT);
        Self {
            address: address.into(),
            password: password.into(),
        }
    }
}

/// Fail if `name` is empty or any target's beacon does not encode.
pub fn check(targets: &[TrackerTarget], name: &str, description: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        bail!("a tracker registration needs a non-empty --name");
    }
    for target in targets {
        beacon(target, name, description, 0, 0, 0)
            .encode(text_encoding::DEFAULT)
            .with_context(|| format!("tracker beacon for {} does not fit", target.address))?;
    }
    Ok(())
}

/// The beacon `hls` sends to one target.
fn beacon(
    target: &TrackerTarget,
    name: &str,
    description: &str,
    port: u16,
    user_count: u16,
    pass_id: u32,
) -> ServerBeacon {
    ServerBeacon {
        port,
        user_count,
        pass_id,
        name: name.into(),
        description: description.into(),
        password: target.password.clone(),
    }
}

/// Beacon `state`'s user count to every target, forever, resolving each
/// address again every cycle.
pub async fn run_beacon(
    state: Arc<SharedState>,
    targets: Vec<TrackerTarget>,
    port: u16,
    name: String,
    description: String,
    pass_id: u32,
) -> anyhow::Result<()> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("failed to open the tracker beacon socket")?;

    loop {
        let user_count = u16::try_from(state.connection_count().await).unwrap_or(u16::MAX);

        for target in &targets {
            let payload = beacon(target, &name, &description, port, user_count, pass_id)
                .encode(text_encoding::DEFAULT);
            let payload = match payload {
                Ok(payload) => payload,
                // `check` already ruled this out at startup.
                Err(error) => {
                    tracing::warn!(target = %target.address, %error, "skipping tracker beacon");
                    continue;
                }
            };
            match socket.send_to(&payload, &*target.address).await {
                Ok(_) => tracing::debug!(target = %target.address, user_count, "beaconed"),
                Err(error) => {
                    tracing::warn!(target = %target.address, %error, "tracker beacon failed");
                }
            }
        }

        tokio::time::sleep(BEACON_INTERVAL).await;
    }
}

/// Read the tracker ID saved at `path`, saving a new random one if it is
/// missing or `0`.
pub async fn load_pass_id(path: &Path) -> anyhow::Result<u32> {
    let saved = match tokio::fs::read_to_string(path).await {
        Ok(text) => text
            .trim()
            .parse()
            .with_context(|| format!("{} does not hold a tracker ID", path.display()))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    if saved != 0 {
        return Ok(saved);
    }
    let pass_id = random_pass_id()?;
    tokio::fs::write(path, format!("{pass_id}\n"))
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(pass_id)
}

/// A random nonzero tracker ID from the OS random source.
fn random_pass_id() -> anyhow::Result<u32> {
    loop {
        let pass_id = getrandom::u32().context("failed to read random bytes")?;
        if pass_id != 0 {
            return Ok(pass_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_takes_the_default_tracker_port() {
        let target = TrackerTarget::parse("tracker.example");
        assert_eq!(&*target.address, "tracker.example:5499");
        assert_eq!(&*target.password, "");
    }

    #[test]
    fn an_explicit_port_is_kept() {
        assert_eq!(
            &*TrackerTarget::parse("10.0.0.1:1234").address,
            "10.0.0.1:1234"
        );
    }

    #[test]
    fn the_tracker_id_is_the_pass_id() {
        let target = TrackerTarget::parse("tracker.example");
        let bytes = beacon(&target, "Test", "", 5500, 0, 0x7a69)
            .encode(text_encoding::DEFAULT)
            .unwrap();
        assert_eq!(bytes[8..12], 0x7a69u32.to_be_bytes());
    }

    #[test]
    fn an_empty_name_is_refused() {
        let targets = [TrackerTarget::parse("tracker.example")];
        let error = check(&targets, "", "").unwrap_err();
        assert!(error.to_string().contains("non-empty --name"), "{error}");
    }

    #[test]
    fn an_oversized_field_is_refused() {
        let targets = [TrackerTarget::parse("tracker.example")];
        assert!(check(&targets, &"x".repeat(256), "").is_err());
        assert!(check(&targets, "ok", &"x".repeat(256)).is_err());
        assert!(check(&targets, "ok", "").is_ok());
    }

    #[test]
    fn an_oversized_password_names_its_own_target() {
        let targets = [TrackerTarget::parse(&format!(
            "{}@tracker.example",
            "x".repeat(256)
        ))];
        let error = check(&targets, "ok", "").unwrap_err();
        assert!(error.to_string().contains("tracker.example"), "{error}");
    }

    /// mhxd's own `password@host` form; the split is from the right, so
    /// an `@` inside the password still parses.
    #[test]
    fn a_password_is_split_off_the_front() {
        let target = TrackerTarget::parse("s3cr3t@tracker.example:5499");
        assert_eq!(&*target.address, "tracker.example:5499");
        assert_eq!(&*target.password, "s3cr3t");
    }

    #[tokio::test]
    async fn a_tracker_id_is_created_once_and_then_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker-id");

        let first = load_pass_id(&path).await.unwrap();
        let second = load_pass_id(&path).await.unwrap();

        assert_ne!(first, 0);
        assert_eq!(first, second);
    }

    /// openline replaces a saved `0` with a random ID.
    #[tokio::test]
    async fn a_saved_zero_tracker_id_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker-id");
        tokio::fs::write(&path, "0\n").await.unwrap();

        let pass_id = load_pass_id(&path).await.unwrap();

        assert_ne!(pass_id, 0);
        assert_eq!(load_pass_id(&path).await.unwrap(), pass_id);
    }

    #[tokio::test]
    async fn an_unreadable_tracker_id_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tracker-id");
        tokio::fs::write(&path, "not a number").await.unwrap();

        assert!(load_pass_id(&path).await.is_err());
    }
}
