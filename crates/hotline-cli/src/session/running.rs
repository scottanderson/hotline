//! The transfers that run, and how to cancel one.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use hotline_client::HotlineClient;
use tokio::io::AsyncWrite;
use tokio::task::AbortHandle;

use super::{ClientSession, Request};
use crate::line::LineKind;

/// The lowest server version that holds a queued download before its
/// transfer starts.
const QUEUE_VERSION: u16 = 185;

/// The lowest server version that takes [`myTran_KillDownload`] (214).
///
/// [`myTran_KillDownload`]: hotline_proto::Opcode::KillDownload
const KILL_DOWNLOAD_VERSION: u16 = 182;

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
    /// The server's reference of a download, `None` for an upload or an
    /// empty folder.
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

    /// Keep the transfer of `name` that runs on `task`.
    pub(super) fn track(
        &mut self,
        kind: Kind,
        name: &str,
        reference: Option<u32>,
        queued: Arc<AtomicBool>,
        task: AbortHandle,
    ) {
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1);
        self.transfers.push(RunningTransfer {
            id: self.next_transfer_id,
            kind,
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
            .transfers
            .iter()
            .find(|t| t.kind.is_download() && t.reference == Some(reference))?;
        download.queued.store(waiting > 0, Ordering::Relaxed);
        Some(download.name.clone())
    }

    /// Each transfer that still runs, as `(ID, label)`. With none, a line says so.
    pub fn running_transfers(&mut self) -> Vec<(u64, Box<str>)> {
        self.transfers.retain(|t| !t.task.is_finished());
        if self.transfers.is_empty() {
            self.push("no transfer runs", LineKind::System);
        }
        self.transfers
            .iter()
            .map(|t| {
                let mut label = format!("{} “{}”", t.kind.does(), t.name);
                if t.queued.load(Ordering::Relaxed) {
                    label.push_str(" (queued)");
                }
                (t.id, label.into())
            })
            .collect()
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
        let in_queue = transfer.queued.load(Ordering::Relaxed);
        let kill = in_queue && self.server_at_least(KILL_DOWNLOAD_VERSION);
        let reference = transfer.reference.filter(|_| kill);
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
        let running = session.running_transfers();
        assert_eq!(running, [(1, "Downloading “a.txt” (queued)".into())]);

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
        let task = tokio::spawn(std::future::pending::<()>()).abort_handle();
        session.track(Kind::Upload, "f", None, Arc::default(), task.clone());

        session.cancel_transfer(&client, 1).run(&mut session).await;
        assert_eq!(
            &*session.lines.last().unwrap().text,
            "cancelled the upload of f"
        );
        tokio::task::yield_now().await;
        assert!(task.is_finished());
        assert!(session.running_transfers().is_empty());
    }
}
