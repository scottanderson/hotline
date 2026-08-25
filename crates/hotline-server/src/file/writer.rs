use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

use crate::file::{Dir, Node};

/// Writes accumulate directly into the shared tree as they arrive
/// (rather than being buffered until shutdown), so a provider consumer
/// that never calls `shutdown()` still sees the bytes it wrote.
pub(crate) struct MemWriter {
    root: Arc<Mutex<Dir>>,
    dir: Vec<String>,
    name: String,
}

impl MemWriter {
    pub(crate) fn new(root: Arc<Mutex<Dir>>, dir: Vec<String>, name: String) -> Self {
        {
            let mut guard = root.lock().unwrap();
            let d = guard.get_dir_mut(&dir);
            d.entries.insert(name.clone(), Node::File(Vec::new()));
        }
        Self { root, dir, name }
    }

    /// Like [`Self::new`], but appends to the file's existing bytes
    /// instead of truncating them.
    pub(crate) fn append(root: Arc<Mutex<Dir>>, dir: Vec<String>, name: String) -> Self {
        Self { root, dir, name }
    }
}

impl AsyncWrite for MemWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.root.lock().unwrap();
        let d = guard.get_dir_mut(&self.dir);
        if let Some(Node::File(data)) = d.entries.get_mut(&self.name) {
            data.extend_from_slice(buf);
        }
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
