//! The transfers that run, how far each is, and how to cancel one.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use std::time::Instant;

use hotline_client::HotlineClient;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::task::AbortHandle;

use super::{ClientSession, Request};
use crate::line::LineKind;
use crate::tasks::{TaskId, TaskRow};
use crate::transfer::{rate_text, size_text};

/// The lowest server version that holds a queued download before its
/// transfer starts.
const QUEUE_VERSION: u16 = 185;

/// The lowest server version that takes [`myTran_KillDownload`] (214).
///
/// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
const KILL_DOWNLOAD_VERSION: u16 = 182;

/// The fewest bytes that give a transfer rate.
const MIN_RATE_BYTES: u64 = 1024;

/// How far a transfer is.
#[derive(Debug, Default)]
pub struct Progress {
    /// The downloads ahead of this one in the server's queue.
    waiting: AtomicU32,
    bytes: AtomicU64,
    /// The bytes of the whole transfer, `0` if unknown.
    total: AtomicU64,
    items: AtomicU32,
    /// The items of a folder transfer, `0` for a file.
    item_count: AtomicU32,
    /// When the first byte moved, and the bytes before it.
    started: OnceLock<(Instant, u64)>,
    /// The server's reference of a download.
    reference: OnceLock<u32>,
    /// `true` after the transfer ends well.
    succeeded: AtomicBool,
}

impl Progress {
    /// Set the size: `total` bytes, of which the first `offset` are there.
    pub(super) fn set_size(&self, offset: u64, total: u64) {
        self.bytes.store(offset, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }

    /// Set the number of items of a folder transfer.
    pub(super) fn set_item_count(&self, count: u32) {
        self.item_count.store(count, Ordering::Relaxed);
    }

    /// Keep the server's reference of a download.
    pub(super) fn set_reference(&self, reference: u32) {
        let _ = self.reference.set(reference);
    }

    /// Count one more item of a folder transfer.
    pub(super) fn add_item(&self) {
        self.items.fetch_add(1, Ordering::Relaxed);
    }

    /// Note that the transfer ended well.
    pub(super) fn succeed(&self) {
        self.succeeded.store(true, Ordering::Relaxed);
    }

    /// Note that `waiting` downloads are ahead of this one in the server's queue.
    pub(super) fn set_waiting(&self, waiting: u32) {
        self.waiting.store(waiting, Ordering::Relaxed);
    }

    /// Count `n` bytes that moved. The first bytes end the wait in the queue.
    fn count(&self, n: usize) {
        if n == 0 {
            return;
        }
        self.started
            .get_or_init(|| (Instant::now(), self.bytes.load(Ordering::Relaxed)));
        self.waiting.store(0, Ordering::Relaxed);
        self.bytes.fetch_add(n as u64, Ordering::Relaxed);
    }

    /// The rate, the bytes, and the time left at `now`, such as
    /// `12K/sec 1.5M/2M 0:42`. `None` until a second passes and more than
    /// 1 KB moves.
    fn stats(&self, now: Instant) -> Option<String> {
        let &(start, start_bytes) = self.started.get()?;
        let bytes = self.bytes.load(Ordering::Relaxed);
        let total = self.total.load(Ordering::Relaxed);
        let seconds = now.saturating_duration_since(start).as_secs();
        let moved = bytes.saturating_sub(start_bytes);
        let rate = (seconds > 0 && moved > seconds && moved > MIN_RATE_BYTES)
            .then(|| moved.checked_div(seconds))
            .flatten()?;
        let left = total.saturating_sub(bytes).checked_div(rate)?;
        Some(format!(
            "{}/sec {}/{} {}",
            rate_text(rate),
            size_text(bytes),
            size_text(total),
            seconds_text(left)
        ))
    }

    /// The part that is done, from 0 to 1. `None` while the download waits
    /// in the server's queue, or when the size is unknown.
    fn ratio(&self) -> Option<f64> {
        if self.waiting.load(Ordering::Relaxed) > 0 {
            return None;
        }
        let total = self.total.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed).min(total);
        (total > 0).then(|| bytes as f64 / total as f64)
    }
}

/// `seconds` as `m:ss`, `h:mm:ss`, or `d:hh:mm:ss`.
fn seconds_text(seconds: u64) -> String {
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let (days, hours) = (hours / 24, hours % 24);
    match (days, hours) {
        (0, 0) => format!("{minutes}:{seconds:02}"),
        (0, hours) => format!("{hours}:{minutes:02}:{seconds:02}"),
        (days, hours) => format!("{days}:{hours:02}:{minutes:02}:{seconds:02}"),
    }
}

/// What a transfer moves, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Download,
    DownloadFolder,
    Upload,
    UploadFolder,
}

impl Kind {
    /// The start of the transfer's label.
    const fn does(self) -> &'static str {
        match self {
            Self::Download => "Downloading",
            Self::DownloadFolder => "Downloading folder",
            Self::Upload => "Uploading",
            Self::UploadFolder => "Uploading folder",
        }
    }

    const fn is_download(self) -> bool {
        matches!(self, Self::Download | Self::DownloadFolder)
    }
}

/// One transfer that runs on its own task.
pub struct RunningTransfer {
    id: u64,
    kind: Kind,
    name: Box<str>,
    /// When the user started the transfer.
    started: Instant,
    progress: Arc<Progress>,
    task: AbortHandle,
}

impl ClientSession {
    /// `true` if the server holds a queued download before its transfer starts.
    pub(super) fn server_queues(&self) -> bool {
        self.server_at_least(QUEUE_VERSION)
    }

    /// Keep the transfer of `name` that the user started at `started` and
    /// that runs on `task`.
    pub(super) fn track(
        &mut self,
        kind: Kind,
        name: &str,
        started: Instant,
        progress: Arc<Progress>,
        task: AbortHandle,
    ) {
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1);
        self.transfers.push(RunningTransfer {
            id: self.next_transfer_id,
            kind,
            name: name.into(),
            started,
            progress,
            task,
        });
    }

    /// Note that the server holds the download `reference` behind `waiting`
    /// others. Returns the download's name.
    pub(super) fn set_queue_place(&self, reference: u32, waiting: u32) -> Option<Box<str>> {
        let download = self
            .transfers
            .iter()
            .find(|t| t.kind.is_download() && t.progress.reference.get() == Some(&reference))?;
        download.progress.set_waiting(waiting);
        Some(download.name.clone())
    }

    /// Forget each transfer that ended. Returns each transfer that failed.
    pub fn end_transfers(&mut self) -> Vec<TaskId> {
        let mut failed = Vec::new();
        self.transfers.retain(|t| {
            let ended = t.task.is_finished();
            if ended && !t.progress.succeeded.load(Ordering::Relaxed) {
                failed.push(TaskId::Transfer(t.id));
            }
            !ended
        });
        failed
    }

    /// The row of each transfer, oldest first.
    #[cfg(test)]
    pub fn running_transfers(&self) -> Vec<TaskRow> {
        self.transfer_rows().map(|(_, row)| row).collect()
    }

    /// The row of each transfer, oldest first, with the time the user
    /// started it.
    pub fn transfer_rows(&self) -> impl Iterator<Item = (Instant, TaskRow)> + '_ {
        let now = Instant::now();
        self.transfers.iter().map(move |t| {
            let progress = &t.progress;
            let waiting = progress.waiting.load(Ordering::Relaxed);
            let mut label = format!("{} “{}”", t.kind.does(), t.name);
            if waiting > 0 {
                label = format!(
                    "All downloads slots are full. “{}” is number {waiting} in the server queue. Please wait...",
                    t.name
                );
            } else if let Some(stats) = progress.stats(now) {
                label.push_str(&format!(" ({stats})"));
                let count = progress.item_count.load(Ordering::Relaxed);
                if count > 0 {
                    let items = progress.items.load(Ordering::Relaxed).min(count);
                    label.push_str(&format!(" [{items} of {count}]"));
                }
            }
            let row = TaskRow {
                id: TaskId::Transfer(t.id),
                label,
                cancellable: true,
                ratio: progress.ratio(),
            };
            (t.started, row)
        })
    }

    /// Stop the transfer `id`. A download that the server still holds in its
    /// queue also leaves the queue, [`myTran_KillDownload`] (214). The `.hpf`
    /// file of a download stays for a resume. An upload closes its transfer
    /// connection.
    ///
    /// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
    pub fn cancel_transfer(&mut self, client: &HotlineClient, id: u64) -> Request<()> {
        let Some(pos) = self.transfers.iter().position(|t| t.id == id) else {
            return Request::ready(());
        };
        let transfer = self.transfers.remove(pos);
        transfer.task.abort();
        if !transfer.kind.is_download() {
            let text = format!("cancelled the upload of {}", transfer.name);
            self.push(text, LineKind::System);
            return Request::ready(());
        }
        let in_queue = transfer.progress.waiting.load(Ordering::Relaxed) > 0;
        let kill = in_queue && self.server_at_least(KILL_DOWNLOAD_VERSION);
        let reference = transfer.progress.reference.get().copied().filter(|_| kill);
        let client = client.clone();
        let failed = format!("failed to cancel {}", transfer.name);
        Request::report(
            async move {
                match reference {
                    Some(reference) => client.kill_download(reference).await,
                    None => Ok(()),
                }
            },
            failed,
            move |session: &mut Self, ()| {
                let text = format!("cancelled the download of {}", transfer.name);
                session.push(text, LineKind::System);
            },
        )
    }
}

/// The way of a stream that [`Counted`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Reads,
    Writes,
}

/// A stream that counts the bytes of one side in a [`Progress`]. The
/// first bytes also end the wait in the server's queue.
pub(super) struct Counted<T> {
    inner: T,
    progress: Arc<Progress>,
    side: Side,
}

impl<T> Counted<T> {
    /// Count the bytes that `inner` reads.
    pub(super) const fn reads(inner: T, progress: Arc<Progress>) -> Self {
        Self {
            inner,
            progress,
            side: Side::Reads,
        }
    }

    /// Count the bytes that `inner` writes.
    pub(super) const fn writes(inner: T, progress: Arc<Progress>) -> Self {
        Self {
            inner,
            progress,
            side: Side::Writes,
        }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for Counted<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let written = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let (Side::Writes, Poll::Ready(Ok(n))) = (self.side, &written) {
            self.progress.count(*n);
        }
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for Counted<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let read = Pin::new(&mut self.inner).poll_read(cx, buf);
        if self.side == Side::Reads {
            self.progress
                .count(buf.filled().len().saturating_sub(before));
        }
        read
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use hotline_proto::types::HlPath;
    use hotline_server::{InMemoryFileProvider, Providers, ServerConfig};

    use super::*;
    use crate::session::test_support;
    use crate::transfer::Start;

    /// Start the clock of `progress` two seconds ago.
    fn start_two_seconds_ago(progress: &Progress) {
        let start = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        progress.started.set((start, 0)).unwrap();
    }

    /// Track a transfer that never ends.
    fn track(session: &mut ClientSession, kind: Kind, progress: Arc<Progress>) -> AbortHandle {
        let task = tokio::spawn(std::future::pending::<()>()).abort_handle();
        session.track(kind, "f", Instant::now(), progress, task.clone());
        task
    }

    #[tokio::test]
    async fn cancel_transfer_takes_a_queued_download_out_of_the_queue() {
        let files = InMemoryFileProvider::default();
        files.seed(&HlPath::default(), "a.txt", *b"a");
        let providers = Providers {
            files: Arc::new(files),
            ..Providers::default()
        };
        let config = ServerConfig {
            max_downloads_per_client: 1,
            ..ServerConfig::default()
        };
        let addr = test_support::start_server_config(config, providers).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let dir = tempfile::tempdir().unwrap();
        let mut session = ClientSession::new("me", 0, client.encoding());
        session.download_dir = dir.path().to_path_buf();
        session.server_version = Some(190);
        let root = HlPath::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let _first = client.download_file(&root, "a.txt", 0).await.unwrap();
        let _ = session
            .download_file(&client, "a.txt", "b.txt", Start::Ask, &tx)
            .run(&mut session)
            .await;
        let label = || session.running_transfers()[0].label.clone();
        tokio::time::timeout(test_support::WAIT, async {
            while label() == "Downloading “a.txt”" {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let running = session.running_transfers();
        assert_eq!(running.len(), 1);
        assert_eq!(
            running[0].label,
            "All downloads slots are full. “a.txt” is number 1 in the server queue. Please wait..."
        );
        assert_eq!(running[0].ratio, None);

        session.cancel_transfer(&client, 1).run(&mut session).await;
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "cancelled the download of a.txt"
        );
        assert!(dir.path().join("b.txt.hpf").exists());
        let next = client.download_file(&root, "a.txt", 0).await.unwrap();
        assert_eq!(next.waiting_count, 1);
    }

    #[tokio::test]
    async fn cancel_transfer_stops_an_upload() {
        let addr = test_support::start_server(Providers::default()).await;
        let (client, _events) = test_support::join_admin(addr, "me").await;
        let mut session = ClientSession::new("me", 0, client.encoding());
        let task = track(&mut session, Kind::Upload, Arc::default());

        session.cancel_transfer(&client, 1).run(&mut session).await;
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "cancelled the upload of f"
        );
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        assert!(session.running_transfers().is_empty());
    }

    #[tokio::test]
    async fn end_transfers_gives_each_transfer_that_failed() {
        let mut session = ClientSession::new("me", 0, Default::default());
        let first = track(&mut session, Kind::Download, Arc::default());
        let _ = track(&mut session, Kind::Upload, Arc::default());
        first.abort();
        tokio::task::yield_now().await;
        assert_eq!(session.end_transfers(), [TaskId::Transfer(1)]);
        assert_eq!(session.running_transfers()[0].label, "Uploading “f”");
    }

    #[tokio::test]
    async fn end_transfers_keeps_quiet_about_a_transfer_that_succeeded() {
        let mut session = ClientSession::new("me", 0, Default::default());
        let progress = Arc::new(Progress::default());
        progress.succeed();
        let task = track(&mut session, Kind::Upload, progress);
        task.abort();
        tokio::task::yield_now().await;
        assert_eq!(session.end_transfers(), []);
        assert!(session.running_transfers().is_empty());
    }

    #[tokio::test]
    async fn running_transfers_gives_the_ratio_of_a_known_size() {
        let mut session = ClientSession::new("me", 0, Default::default());
        let progress = Arc::new(Progress::default());
        progress.set_size(25, 100);
        let _ = track(&mut session, Kind::Upload, progress);
        assert_eq!(session.running_transfers()[0].ratio, Some(0.25));
    }

    #[tokio::test]
    async fn running_transfers_counts_the_items_of_a_folder() {
        let mut session = ClientSession::new("me", 0, Default::default());
        let progress = Arc::new(Progress::default());
        progress.set_item_count(2);
        progress.set_size(0, 4096);
        start_two_seconds_ago(&progress);
        progress.count(2048);
        progress.add_item();
        let _ = track(&mut session, Kind::UploadFolder, progress);
        assert_eq!(
            session.running_transfers(),
            [TaskRow {
                id: TaskId::Transfer(1),
                label: "Uploading folder “f” (1K/sec 2K/4K 0:02) [1 of 2]".into(),
                cancellable: true,
                ratio: Some(0.5),
            }]
        );
    }

    #[test]
    fn ratio_waits_for_the_end_of_the_server_queue() {
        let progress = Progress::default();
        progress.set_size(0, 100);
        progress.set_waiting(4);
        assert_eq!(progress.ratio(), None);
        progress.count(10);
        assert_eq!(progress.ratio(), Some(0.1));
    }

    #[test]
    fn stats_give_the_rate_and_the_time_left() {
        let progress = Progress::default();
        progress.set_size(1024, 10 * 1024);
        progress.count(2048);
        let (start, _) = *progress.started.get().unwrap();
        let later = start + Duration::from_secs(2);
        assert_eq!(progress.stats(later).unwrap(), "1K/sec 3K/10K 0:07");
    }

    #[test]
    fn stats_wait_for_the_rate() {
        let progress = Progress::default();
        progress.set_size(0, 10 * 1024);
        progress.count(2048);
        assert_eq!(progress.stats(Instant::now()), None);
    }

    #[test]
    fn seconds_text_adds_hours_and_days_when_needed() {
        assert_eq!(seconds_text(42), "0:42");
        assert_eq!(seconds_text(3600 + 61), "1:01:01");
        assert_eq!(seconds_text(86_400 + 3600 + 61), "1:01:01:01");
    }
}
