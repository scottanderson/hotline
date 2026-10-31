use std::collections::hash_map::Entry;
use std::collections::HashMap;
use std::net::IpAddr;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use crate::sync::lock;

/// Counts open connections, in total and for each address.
#[derive(Debug)]
pub(crate) struct ConnectionLimits {
    max_total: Option<NonZeroUsize>,
    max_per_address: Option<NonZeroUsize>,
    open: Mutex<OpenConnections>,
}

#[derive(Debug, Default)]
struct OpenConnections {
    total: usize,
    by_address: HashMap<IpAddr, usize>,
}

/// `true` once `count` reaches `max`.
const fn at_limit(count: usize, max: Option<NonZeroUsize>) -> bool {
    match max {
        Some(max) => count >= max.get(),
        None => false,
    }
}

impl ConnectionLimits {
    /// `None` means no limit.
    pub(crate) fn new(
        max_total: Option<NonZeroUsize>,
        max_per_address: Option<NonZeroUsize>,
    ) -> Arc<Self> {
        Arc::new(Self {
            max_total,
            max_per_address,
            open: Mutex::default(),
        })
    }

    /// A place for one more connection from `ip`, or `None` at either limit.
    pub(crate) fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionPermit> {
        let mut open = lock(&self.open);
        let from_ip = open.by_address.get(&ip).copied().unwrap_or(0);
        if at_limit(open.total, self.max_total) || at_limit(from_ip, self.max_per_address) {
            return None;
        }
        open.total += 1;
        *open.by_address.entry(ip).or_default() += 1;
        Some(ConnectionPermit {
            limits: Arc::clone(self),
            ip,
        })
    }
}

/// Holds one connection's place until dropped.
#[derive(Debug)]
pub(crate) struct ConnectionPermit {
    limits: Arc<ConnectionLimits>,
    ip: IpAddr,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let mut open = lock(&self.limits.open);
        open.total -= 1;
        if let Entry::Occupied(mut entry) = open.by_address.entry(self.ip) {
            *entry.get_mut() -= 1;
            if *entry.get() == 0 {
                entry.remove();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));
    const BOB: IpAddr = IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 2));

    fn limits(max_total: usize, max_per_address: usize) -> Arc<ConnectionLimits> {
        ConnectionLimits::new(
            NonZeroUsize::new(max_total),
            NonZeroUsize::new(max_per_address),
        )
    }

    #[test]
    fn an_address_at_its_limit_is_refused() {
        let limits = limits(0, 2);
        let _first = limits.try_acquire(ALICE).unwrap();
        let _second = limits.try_acquire(ALICE).unwrap();

        assert!(limits.try_acquire(ALICE).is_none());
        assert!(limits.try_acquire(BOB).is_some());
    }

    #[test]
    fn the_total_limit_covers_every_address() {
        let limits = limits(2, 0);
        let _first = limits.try_acquire(ALICE).unwrap();
        let _second = limits.try_acquire(BOB).unwrap();

        assert!(limits.try_acquire(BOB).is_none());
    }

    #[test]
    fn dropping_a_permit_frees_its_place() {
        let limits = limits(1, 1);
        let first = limits.try_acquire(ALICE).unwrap();
        drop(first);

        assert!(limits.open.lock().unwrap().by_address.is_empty());
        assert!(limits.try_acquire(ALICE).is_some());
    }

    #[test]
    fn no_limit_allows_any_number() {
        let limits = ConnectionLimits::new(None, None);
        let permits: Vec<_> = (0..100).map(|_| limits.try_acquire(ALICE)).collect();

        assert!(permits.iter().all(Option::is_some));
    }
}
