use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use crate::flood::{FloodDecision, FloodEvent, FloodPolicy, MAX_CHAT_LINE};
use crate::sync::lock;

/// Length of each counting window.
const WINDOW: Duration = Duration::from_secs(60);
/// Transactions a window allows.
const MAX_TRANSACTIONS: usize = 120;
/// Private messages, broadcasts, and chat invites a window allows.
const MAX_MESSAGES: usize = 20;
/// Relayed chat bytes a window allows.
const MAX_CHAT_BYTES: usize = 10240;
/// Least a chat line counts for, which also caps lines per window.
const MIN_CHAT_LINE: usize = 90;
/// Name or icon changes a window allows.
const MAX_NAME_CHANGES: usize = 25;
/// Repeats of the previous text that ban.
const MAX_REPEATS: u32 = 5;
/// Connections in a row from one address past which a burst bans.
const MAX_CONNECTIONS_IN_A_ROW: u32 = 10;
/// How soon those connections must follow the first.
const CONNECTION_BURST: Duration = Duration::from_secs(30);

/// A count that restarts once [`WINDOW`] passes since its first event.
#[derive(Debug)]
struct Window {
    start: Instant,
    count: usize,
}

impl Window {
    const fn new(now: Instant) -> Self {
        Self {
            start: now,
            count: 0,
        }
    }

    /// `true` if the count is already past `max`; otherwise adds `amount`.
    fn over(&mut self, now: Instant, max: usize, amount: usize) -> bool {
        if now.duration_since(self.start) > WINDOW {
            *self = Self::new(now);
        }
        if self.count > max {
            return true;
        }
        self.count += amount;
        false
    }

    /// Adds `amount` without a check.
    const fn add(&mut self, amount: usize) {
        self.count += amount;
    }
}

/// One session's counters.
#[derive(Debug)]
struct UserFlood {
    transactions: Window,
    messages: Window,
    chat_bytes: Window,
    name_changes: Window,
    last_text: Option<u64>,
    repeats: u32,
}

impl UserFlood {
    const fn new(now: Instant) -> Self {
        Self {
            transactions: Window::new(now),
            messages: Window::new(now),
            chat_bytes: Window::new(now),
            name_changes: Window::new(now),
            last_text: None,
            repeats: 0,
        }
    }

    fn over(&mut self, now: Instant, event: FloodEvent<'_>) -> bool {
        match event {
            FloodEvent::Transaction => self.transactions.over(now, MAX_TRANSACTIONS, 1),
            FloodEvent::Message => self.messages.over(now, MAX_MESSAGES, 1),
            FloodEvent::Chat => self.chat_bytes.over(now, MAX_CHAT_BYTES, 0),
            FloodEvent::ChatBytes(len) => {
                self.chat_bytes.add(len.clamp(MIN_CHAT_LINE, MAX_CHAT_LINE));
                false
            }
            FloodEvent::NameChange => self.name_changes.over(now, MAX_NAME_CHANGES, 1),
            FloodEvent::Text(text) => self.repeated(text),
        }
    }

    /// `true` on the [`MAX_REPEATS`]th repeat in a row of the same text.
    fn repeated(&mut self, text: &[u8]) -> bool {
        let mut hasher = DefaultHasher::new();
        text.hash(&mut hasher);
        let hash = hasher.finish();
        if self.last_text == Some(hash) {
            self.repeats += 1;
            return self.repeats == MAX_REPEATS;
        }
        self.last_text = Some(hash);
        self.repeats = 0;
        false
    }
}

/// The most recent address to connect, and its run of connections.
#[derive(Debug)]
struct ConnectionRun {
    ip: IpAddr,
    start: Instant,
    count: u32,
}

/// A [`FloodPolicy`] with openline's fixed limits, kept in memory only.
#[derive(Debug, Default)]
pub struct InMemoryFloodPolicy {
    users: Mutex<HashMap<u16, UserFlood>>,
    last_connection: Mutex<Option<ConnectionRun>>,
}

#[async_trait]
impl FloodPolicy for InMemoryFloodPolicy {
    async fn check(&self, user_id: u16, event: FloodEvent<'_>) -> FloodDecision {
        let now = Instant::now();
        let mut users = lock(&self.users);
        let user = users.entry(user_id).or_insert_with(|| UserFlood::new(now));
        FloodDecision::ban_if(user.over(now, event))
    }

    async fn check_connection(&self, ip: IpAddr) -> FloodDecision {
        let now = Instant::now();
        let mut last = lock(&self.last_connection);
        match last.as_mut() {
            Some(run) if run.ip == ip => {
                run.count += 1;
                FloodDecision::ban_if(
                    run.count > MAX_CONNECTIONS_IN_A_ROW
                        && now.duration_since(run.start) < CONNECTION_BURST,
                )
            }
            _ => {
                *last = Some(ConnectionRun {
                    ip,
                    start: now,
                    count: 0,
                });
                FloodDecision::Allow
            }
        }
    }

    async fn reset(&self, user_id: u16) {
        lock(&self.users).remove(&user_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: u16 = 1;
    const BOB: u16 = 2;

    /// Checks `event` `times` times; the index of the first ban, if any.
    async fn first_ban(
        policy: &InMemoryFloodPolicy,
        event: FloodEvent<'_>,
        times: usize,
    ) -> Option<usize> {
        for i in 1..=times {
            if policy.check(ALICE, event).await == FloodDecision::Ban {
                return Some(i);
            }
        }
        None
    }

    /// Sends `times` chat lines of `len` bytes; the first ban's index, if any.
    async fn first_chat_ban(
        policy: &InMemoryFloodPolicy,
        len: usize,
        times: usize,
    ) -> Option<usize> {
        for i in 1..=times {
            if policy.check(ALICE, FloodEvent::Chat).await == FloodDecision::Ban {
                return Some(i);
            }
            policy.check(ALICE, FloodEvent::ChatBytes(len)).await;
        }
        None
    }

    #[tokio::test]
    async fn the_122nd_transaction_in_a_window_bans() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(
            first_ban(&policy, FloodEvent::Transaction, 200).await,
            Some(122)
        );
    }

    #[tokio::test]
    async fn the_22nd_message_in_a_window_bans() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(first_ban(&policy, FloodEvent::Message, 50).await, Some(22));
    }

    #[tokio::test]
    async fn the_27th_name_change_in_a_window_bans() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(
            first_ban(&policy, FloodEvent::NameChange, 50).await,
            Some(27)
        );
    }

    #[tokio::test]
    async fn short_chat_lines_count_as_90_bytes() {
        let policy = InMemoryFloodPolicy::default();
        // 10240 / 90 = 113.8: the 115th short line finds the total past 10240.
        assert_eq!(first_chat_ban(&policy, 1, 200).await, Some(115));
    }

    #[tokio::test]
    async fn a_long_chat_line_counts_at_most_8192_bytes() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(first_chat_ban(&policy, 100_000, 5).await, Some(3));
    }

    #[tokio::test]
    async fn the_sixth_identical_text_in_a_row_bans() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(
            first_ban(&policy, FloodEvent::Text(b"spam"), 10).await,
            Some(6)
        );
    }

    #[tokio::test]
    async fn a_different_text_restarts_the_repeat_count() {
        let policy = InMemoryFloodPolicy::default();
        for _ in 0..5 {
            policy.check(ALICE, FloodEvent::Text(b"spam")).await;
        }
        policy.check(ALICE, FloodEvent::Text(b"other")).await;

        assert_eq!(
            first_ban(&policy, FloodEvent::Text(b"spam"), 10).await,
            Some(6)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_window_restarts_after_60_seconds() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(first_ban(&policy, FloodEvent::Message, 21).await, None);

        tokio::time::advance(WINDOW + Duration::from_secs(1)).await;

        assert_eq!(first_ban(&policy, FloodEvent::Message, 21).await, None);
    }

    #[tokio::test]
    async fn users_are_counted_apart() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(first_ban(&policy, FloodEvent::Message, 21).await, None);

        assert_eq!(
            policy.check(BOB, FloodEvent::Message).await,
            FloodDecision::Allow
        );
    }

    #[tokio::test]
    async fn reset_forgets_a_user() {
        let policy = InMemoryFloodPolicy::default();
        assert_eq!(first_ban(&policy, FloodEvent::Message, 21).await, None);

        policy.reset(ALICE).await;

        assert_eq!(first_ban(&policy, FloodEvent::Message, 21).await, None);
    }

    const HOST: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));
    const OTHER: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));

    #[tokio::test]
    async fn the_12th_connection_in_a_row_within_30_seconds_bans() {
        let policy = InMemoryFloodPolicy::default();
        for _ in 0..11 {
            assert_eq!(policy.check_connection(HOST).await, FloodDecision::Allow);
        }
        assert_eq!(policy.check_connection(HOST).await, FloodDecision::Ban);
    }

    #[tokio::test]
    async fn another_address_breaks_a_connection_run() {
        let policy = InMemoryFloodPolicy::default();
        for _ in 0..11 {
            policy.check_connection(HOST).await;
        }
        policy.check_connection(OTHER).await;

        assert_eq!(policy.check_connection(HOST).await, FloodDecision::Allow);
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_connection_run_does_not_ban() {
        let policy = InMemoryFloodPolicy::default();
        for _ in 0..11 {
            policy.check_connection(HOST).await;
        }

        tokio::time::advance(CONNECTION_BURST).await;

        assert_eq!(policy.check_connection(HOST).await, FloodDecision::Allow);
    }
}
