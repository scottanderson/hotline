use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::AsyncWrite;

use crate::file::memory::Store;
use crate::file::Node;
use crate::sync::lock;

/// Writes accumulate directly into the shared tree as they arrive
/// (rather than being buffered until shutdown), so a provider consumer
/// that never calls `shutdown()` still sees the bytes it wrote.
pub(crate) struct MemWriter {
    store: Arc<Store>,
    dir: Vec<String>,
    name: String,
}

impl MemWriter {
    /// Appends to the file `name` in `dir`, which must already exist.
    pub(crate) const fn appending(store: Arc<Store>, dir: Vec<String>, name: String) -> Self {
        Self { store, dir, name }
    }
}

impl AsyncWrite for MemWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut guard = lock(&self.store.root);
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

/// Appends to one resource fork of a shared [`Store`].
pub(crate) struct ForkWriter {
    store: Arc<Store>,
    key: (Vec<String>, String),
}

impl ForkWriter {
    /// Appends to the value at `key`, which it creates if missing.
    pub(crate) const fn appending(store: Arc<Store>, key: (Vec<String>, String)) -> Self {
        Self { store, key }
    }
}

impl AsyncWrite for ForkWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        lock(&self.store.resource_forks)
            .entry(self.key.clone())
            .or_default()
            .extend_from_slice(buf);
        Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}
