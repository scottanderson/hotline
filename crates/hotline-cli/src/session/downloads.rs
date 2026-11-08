//! The downloads that run, and how to cancel one.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use hotline_client::HotlineClient;
use tokio::io::AsyncWrite;
use tokio::task::AbortHandle;

use super::ClientSession;
use crate::line::LineKind;

/// The lowest server version that holds a queued download before its
/// transfer starts.
const QUEUE_VERSION: u16 = 185;

/// The lowest server version that takes [`myTran_KillDownload`] (214).
///
/// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
const KILL_DOWNLOAD_VERSION: u16 = 182;

/// One download that runs on its own task.
pub struct RunningDownload {
    id: u64,
    name: Box<str>,
    /// The server's reference, `None` for an empty folder.
    reference: Option<u32>,
    /// `true` while the server holds the download in its queue.
    queued: Arc<AtomicBool>,
    task: AbortHandle,
}

impl ClientSession {
    /// A flag that is `true` while a download waits in the server's queue
    /// behind `waiting_count` others.
    pub(super) fn queued_flag(&self, waiting_count: u32) -> Arc<AtomicBool> {
        let queued = waiting_count > 0 && self.server_at_least(QUEUE_VERSION);
        Arc::new(AtomicBool::new(queued))
    }

    /// Keep the download of `name` that runs on `task`.
    pub(super) fn track_download(
        &mut self,
        name: &str,
        reference: Option<u32>,
        queued: Arc<AtomicBool>,
        task: AbortHandle,
    ) {
        self.next_download_id += 1;
        self.downloads.push(RunningDownload {
            id: self.next_download_id,
            name: name.into(),
            reference,
            queued,
            task,
        });
    }

    /// Note that the server holds the download `reference` behind `waiting`
    /// others. Returns the download's name.
    pub(super) fn set_queue_place(&self, reference: u32, waiting: u32) -> Option<Box<str>> {
        let download = self
            .downloads
            .iter()
            .find(|d| d.reference == Some(reference))?;
        download.queued.store(waiting > 0, Ordering::Relaxed);
        Some(download.name.clone())
    }

    /// Each download that still runs, as `(ID, label)`. With none, a line says so.
    pub fn running_downloads(&mut self) -> Vec<(u64, Box<str>)> {
        self.downloads.retain(|d| !d.task.is_finished());
        if self.downloads.is_empty() {
            self.push("no download runs", LineKind::System);
        }
        self.downloads
            .iter()
            .map(|d| {
                let label = if d.queued.load(Ordering::Relaxed) {
                    format!("{} (queued)", d.name)
                } else {
                    d.name.to_string()
                };
                (d.id, label.into())
            })
            .collect()
    }

    /// Stop the download `id`. One that the server still holds in its queue
    /// also leaves the queue, [`myTran_KillDownload`] (214). The `.hpf`
    /// file stays for a resume.
    ///
    /// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
    pub async fn kill_download(&mut self, client: &HotlineClient, id: u64) {
        let Some(pos) = self.downloads.iter().position(|d| d.id == id) else {
            return;
        };
        let download = self.downloads.remove(pos);
        download.task.abort();
        let in_queue = download.queued.load(Ordering::Relaxed);
        let kill = in_queue && self.server_at_least(KILL_DOWNLOAD_VERSION);
        if let Some(reference) = download.reference.filter(|_| kill) {
            if let Err(e) = client.kill_download(reference).await {
                let text = format!("failed to cancel {}: {e}", download.name);
                return self.push(text, LineKind::Error);
            }
        }
        self.push(
            format!("cancelled the download of {}", download.name),
            LineKind::System,
        );
    }
}

/// A writer that clears its download's queued flag on the first write.
pub(super) struct FirstWrite<W> {
    inner: W,
    queued: Arc<AtomicBool>,
}

impl<W> FirstWrite<W> {
    pub(super) const fn new(inner: W, queued: Arc<AtomicBool>) -> Self {
        Self { inner, queued }
    }
}

impl<W: AsyncWrite + Unpin> AsyncWrite for FirstWrite<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.queued.store(false, Ordering::Relaxed);
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use hotline_proto::types::HlPath;
    use hotline_server::{InMemoryFileProvider, Providers, ServerConfig};

    use super::*;
    use crate::session::test_support;
    use crate::transfer::Start;

    #[tokio::test]
    async fn kill_download_takes_a_queued_download_out_of_the_queue() {
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
            .await;
        let running = session.running_downloads();
        assert_eq!(running, [(1, "a.txt (queued)".into())]);

        session.kill_download(&client, 1).await;
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "cancelled the download of a.txt"
        );
        assert!(dir.path().join("b.txt.hpf").exists());
        let next = client.download_file(&root, "a.txt", 0).await.unwrap();
        assert_eq!(next.waiting_count, 1);
    }
}
