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
    /// Appends to the file `name` in `dir`, which must already exist.
    pub(crate) const fn appending(root: Arc<Mutex<Dir>>, dir: Vec<String>, name: String) -> Self {
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
        if let Some(Node::File(data)) = guard
            .get_dir_mut(&self.dir)
            .and_then(|d| d.entries.get_mut(&self.name))
        {
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
