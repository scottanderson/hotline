//! Download admission past `max_downloads`/`max_downloads_per_client`,
//! like openline's `mDownloadWaitingList`. Uploads are not capped.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::server::{DEFAULT_MAX_DOWNLOADS, DEFAULT_MAX_DOWNLOADS_PER_CLIENT};
use crate::state::TransferRef;
use crate::sync::lock;

/// A download waiting for a free slot.
struct Waiting {
    reference: TransferRef,
    user_id: u16,
    /// [`myField_WaitingCount`] (116) last pushed, to skip unchanged updates.
    ///
    /// [`myField_WaitingCount`]: hotline_proto::FieldId::WaitingCount
    last_reported: u32,
}

/// `max_downloads` and `max_downloads_per_client`, see [`crate::ServerConfig`].
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub(crate) total: usize,
    pub(crate) per_client: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            total: DEFAULT_MAX_DOWNLOADS,
            per_client: DEFAULT_MAX_DOWNLOADS_PER_CLIENT,
        }
    }
}

#[derive(Default)]
struct Inner {
    limits: Limits,
    active: HashMap<TransferRef, u16>,
    waiting: VecDeque<Waiting>,
    notify: HashMap<TransferRef, Arc<Notify>>,
    cancel: HashMap<TransferRef, CancellationToken>,
}

impl Inner {
    /// Downloads ahead of a new one from `user_id`, `0` if a slot is free.
    fn ahead_of(&self, user_id: u16) -> u32 {
        let client_count = self.active.values().filter(|&&u| u == user_id).count();
        ahead(self.active.len(), self.limits.total).max(ahead(client_count, self.limits.per_client))
    }

    /// Wake `reference`'s waiter, even one that has not started waiting yet.
    fn wake(&mut self, reference: TransferRef) {
        if let Some(notify) = self.notify.remove(&reference) {
            notify.notify_one();
        }
    }
}

/// The downloads ahead of a new one when `count` run and `max` may run:
/// `0` under `max`, `count - max + 1` at or past it.
const fn ahead(count: usize, max: usize) -> u32 {
    (count + 1).saturating_sub(max) as u32
}

/// One promotion or position update to push as [`myTran_DownloadInfo`] (211).
///
/// [`myTran_DownloadInfo`]: hotline_proto::Opcode::DownloadInfo
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct QueueUpdate {
    pub(crate) user_id: u16,
    pub(crate) reference: TransferRef,
    /// `0` means the download was just promoted to an active slot.
    pub(crate) waiting_count: u32,
}

#[derive(Default)]
pub(crate) struct DownloadQueue {
    inner: Mutex<Inner>,
}

impl DownloadQueue {
    /// Replace the limits. Takes effect at the next admission or release.
    pub(crate) fn set_limits(&self, limits: Limits) {
        lock(&self.inner).limits = limits;
    }

    /// Admit a newly registered download. Returns the
    /// [`myField_WaitingCount`] (116) for its own reply, `0` if it starts in
    /// an active slot.
    ///
    /// [`myField_WaitingCount`]: hotline_proto::FieldId::WaitingCount
    pub(crate) fn admit(&self, reference: TransferRef, user_id: u16) -> u32 {
        let mut inner = lock(&self.inner);
        inner.cancel.insert(reference, CancellationToken::new());
        let waiting_count = inner.ahead_of(user_id);
        if waiting_count == 0 {
            inner.active.insert(reference, user_id);
            return 0;
        }
        let reported = waiting_count + inner.waiting.len() as u32;
        inner.notify.insert(reference, Arc::new(Notify::new()));
        inner.waiting.push_back(Waiting {
            reference,
            user_id,
            last_reported: reported,
        });
        reported
    }

    /// Wait until `reference` holds an active slot. `false` if it left the
    /// queue without one.
    pub(crate) async fn wait_for_slot(&self, reference: TransferRef) -> bool {
        loop {
            let notify = {
                let inner = lock(&self.inner);
                if inner.active.contains_key(&reference) {
                    return true;
                }
                match inner.notify.get(&reference) {
                    Some(notify) => notify.clone(),
                    None => return false,
                }
            };
            notify.notified().await;
        }
    }

    /// Drop `reference`, active or queued, and promote waiting downloads
    /// that now fit, like openline's `ProcessDownloadsWaiting`.
    pub(crate) fn release(&self, reference: TransferRef) -> Vec<QueueUpdate> {
        let mut inner = lock(&self.inner);
        inner.active.remove(&reference);
        inner.waiting.retain(|w| w.reference != reference);
        inner.cancel.remove(&reference);
        inner.wake(reference);
        recompute(&mut inner)
    }

    /// Whether `reference` is waiting for a slot.
    pub(crate) fn is_waiting(&self, reference: TransferRef) -> bool {
        let inner = lock(&self.inner);
        inner.waiting.iter().any(|w| w.reference == reference)
    }

    /// `reference`'s cancel token, `None` once it has left the queue.
    pub(crate) fn cancel_token(&self, reference: TransferRef) -> Option<CancellationToken> {
        lock(&self.inner).cancel.get(&reference).cloned()
    }

    /// Cancel `user_id`'s download `reference`, then release it like
    /// [`Self::release`]. `None` if it isn't `user_id`'s download.
    pub(crate) fn kill(&self, reference: TransferRef, user_id: u16) -> Option<Vec<QueueUpdate>> {
        {
            let inner = lock(&self.inner);
            let owner = inner.active.get(&reference).copied().or_else(|| {
                inner
                    .waiting
                    .iter()
                    .find(|w| w.reference == reference)
                    .map(|w| w.user_id)
            });
            if owner != Some(user_id) {
                return None;
            }
            if let Some(cancel) = inner.cancel.get(&reference) {
                cancel.cancel();
            }
        }
        Some(self.release(reference))
    }

    /// Cancel and drop every active and waiting entry for a disconnected
    /// user, then promote waiting downloads that now fit.
    pub(crate) fn remove_user(&self, user_id: u16) -> Vec<QueueUpdate> {
        let mut inner = lock(&self.inner);
        let removed_refs: Vec<TransferRef> = inner
            .active
            .iter()
            .filter(|&(_, &u)| u == user_id)
            .map(|(&r, _)| r)
            .chain(
                inner
                    .waiting
                    .iter()
                    .filter(|w| w.user_id == user_id)
                    .map(|w| w.reference),
            )
            .collect();
        inner.active.retain(|_, &mut u| u != user_id);
        inner.waiting.retain(|w| w.user_id != user_id);
        for reference in removed_refs {
            if let Some(cancel) = inner.cancel.remove(&reference) {
                cancel.cancel();
            }
            inner.wake(reference);
        }
        recompute(&mut inner)
    }
}

/// Recompute each queued waiting count front to back, promoting each that
/// reaches `0`.
fn recompute(inner: &mut Inner) -> Vec<QueueUpdate> {
    let mut updates = Vec::new();
    let mut i = 0;
    while let Some(user_id) = inner.waiting.get(i).map(|w| w.user_id) {
        let mut waiting_count = inner.ahead_of(user_id);
        if waiting_count > 0 {
            waiting_count += i as u32;
        }
        if waiting_count == 0 {
            let Some(w) = inner.waiting.remove(i) else {
                break;
            };
            inner.active.insert(w.reference, w.user_id);
            inner.wake(w.reference);
            updates.push(QueueUpdate {
                user_id: w.user_id,
                reference: w.reference,
                waiting_count: 0,
            });
        } else if let Some(entry) = inner.waiting.get_mut(i) {
            if entry.last_reported != waiting_count {
                entry.last_reported = waiting_count;
                updates.push(QueueUpdate {
                    user_id,
                    reference: entry.reference,
                    waiting_count,
                });
            }
            i += 1;
        }
    }
    updates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ahead_is_zero_under_max_and_counts_from_max() {
        assert_eq!(
            [0, 1, 2, 3, 4].map(|count| ahead(count, 2)),
            [0, 0, 1, 2, 3]
        );
    }

    fn update(user_id: u16, reference: u32, waiting_count: u32) -> QueueUpdate {
        QueueUpdate {
            user_id,
            reference: TransferRef(reference),
            waiting_count,
        }
    }

    fn with_limits(total: usize, per_client: usize) -> DownloadQueue {
        let queue = DownloadQueue::default();
        queue.set_limits(Limits { total, per_client });
        queue
    }

    #[test]
    fn admits_under_the_cap_immediately() {
        let q = with_limits(2, 2);
        assert_eq!(q.admit(TransferRef(1), 100), 0);
        assert_eq!(q.admit(TransferRef(2), 101), 0);
    }

    #[test]
    fn queues_past_the_global_cap() {
        let q = with_limits(1, 10);
        assert_eq!(q.admit(TransferRef(1), 100), 0);
        assert_eq!(q.admit(TransferRef(2), 101), 1);
    }

    #[test]
    fn queues_past_the_per_client_cap() {
        let q = with_limits(10, 1);
        assert_eq!(q.admit(TransferRef(1), 100), 0);
        assert_eq!(q.admit(TransferRef(2), 100), 1);
    }

    #[test]
    fn releasing_an_active_slot_promotes_the_front_of_the_queue() {
        let q = with_limits(1, 10);
        assert_eq!(q.admit(TransferRef(1), 100), 0);
        assert_eq!(q.admit(TransferRef(2), 101), 1);
        assert_eq!(q.admit(TransferRef(3), 102), 2);

        let updates = q.release(TransferRef(1));
        assert_eq!(updates, vec![update(101, 2, 0), update(102, 3, 1)]);
    }

    #[test]
    fn releasing_a_waiting_download_drops_it_from_the_queue() {
        let q = with_limits(1, 10);
        assert_eq!(q.admit(TransferRef(1), 100), 0);
        assert_eq!(q.admit(TransferRef(2), 101), 1);

        q.release(TransferRef(2));

        assert_eq!(q.release(TransferRef(1)), vec![]);
        assert_eq!(q.admit(TransferRef(3), 102), 0);
    }

    #[tokio::test]
    async fn wait_for_slot_returns_immediately_once_active() {
        let q = with_limits(1, 10);
        q.admit(TransferRef(1), 100);
        assert!(q.wait_for_slot(TransferRef(1)).await);
    }

    #[tokio::test]
    async fn wait_for_slot_resolves_once_promoted() {
        let q = Arc::new(with_limits(1, 10));
        q.admit(TransferRef(1), 100);
        assert_eq!(q.admit(TransferRef(2), 101), 1);

        let waiter = tokio::spawn({
            let q = q.clone();
            async move { q.wait_for_slot(TransferRef(2)).await }
        });
        tokio::task::yield_now().await;
        q.release(TransferRef(1));
        assert!(waiter.await.unwrap());
    }

    #[tokio::test]
    async fn a_wakeup_before_the_wait_starts_is_kept() {
        let mut inner = Inner::default();
        let notify = Arc::new(Notify::new());
        inner.notify.insert(TransferRef(2), notify.clone());

        inner.wake(TransferRef(2));

        tokio::time::timeout(std::time::Duration::from_secs(1), notify.notified())
            .await
            .expect("wake should store a permit");
    }

    #[tokio::test]
    async fn disconnecting_a_waiting_user_ends_their_wait_without_a_slot() {
        let q = Arc::new(with_limits(1, 10));
        q.admit(TransferRef(1), 100);
        assert_eq!(q.admit(TransferRef(2), 101), 1);

        let waiter = tokio::spawn({
            let q = q.clone();
            async move { q.wait_for_slot(TransferRef(2)).await }
        });
        tokio::task::yield_now().await;
        q.remove_user(101);

        let granted = tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .expect("the waiter should wake")
            .unwrap();
        assert!(!granted);
    }

    #[test]
    fn disconnecting_a_waiting_user_drops_their_entry_and_reindexes_the_rest() {
        let q = with_limits(1, 10);
        q.admit(TransferRef(1), 100);
        assert_eq!(q.admit(TransferRef(2), 101), 1);
        assert_eq!(q.admit(TransferRef(3), 102), 2);

        let updates = q.remove_user(101);
        assert_eq!(updates, vec![update(102, 3, 1)]);
    }

    #[test]
    fn disconnecting_an_active_user_frees_their_slot_for_the_queue() {
        let q = with_limits(1, 10);
        q.admit(TransferRef(1), 100);
        assert_eq!(q.admit(TransferRef(2), 101), 1);

        let updates = q.remove_user(100);
        assert_eq!(updates, vec![update(101, 2, 0)]);
    }

    #[test]
    fn disconnecting_a_user_cancels_their_running_download() {
        let q = with_limits(1, 10);
        q.admit(TransferRef(1), 100);
        let cancel = q.cancel_token(TransferRef(1)).unwrap();

        q.remove_user(100);

        assert!(cancel.is_cancelled());
    }

    #[test]
    fn killing_a_running_download_cancels_it_and_frees_its_slot() {
        let queue = with_limits(1, 10);
        queue.admit(TransferRef(1), 5);
        queue.admit(TransferRef(2), 6);
        let cancel = queue.cancel_token(TransferRef(1)).unwrap();

        let updates = queue
            .kill(TransferRef(1), 5)
            .expect("user 5 owns download 1");

        assert!(cancel.is_cancelled());
        assert!(queue.cancel_token(TransferRef(1)).is_none());
        assert_eq!(updates, vec![update(6, 2, 0)]);
    }

    #[test]
    fn killing_a_queued_download_drops_it() {
        let queue = with_limits(1, 10);
        queue.admit(TransferRef(1), 5);
        queue.admit(TransferRef(2), 6);

        assert!(queue.kill(TransferRef(2), 6).is_some());
        assert!(queue.cancel_token(TransferRef(2)).is_none());
    }

    /// openline's `KillDownloadByRefNum` leaves another client's download alone.
    #[test]
    fn another_users_download_is_not_killed() {
        let queue = with_limits(1, 10);
        queue.admit(TransferRef(1), 5);
        let cancel = queue.cancel_token(TransferRef(1)).unwrap();

        assert!(queue.kill(TransferRef(1), 6).is_none());
        assert!(!cancel.is_cancelled());
    }
}
