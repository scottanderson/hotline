use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// A stream that adds every byte it reads or writes to `count`.
pub(crate) struct CountingStream<S> {
    inner: S,
    count: Arc<AtomicU64>,
}

impl<S> CountingStream<S> {
    pub(crate) const fn new(inner: S, count: Arc<AtomicU64>) -> Self {
        Self { inner, count }
    }

    fn add(&self, n: usize) {
        self.count.fetch_add(n as u64, Ordering::Relaxed);
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for CountingStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        self.add(buf.filled().len().saturating_sub(before));
        result
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for CountingStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = result {
            self.add(n);
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn counts_bytes_both_ways() {
        let (mut peer, ours) = tokio::io::duplex(64);
        let count = Arc::new(AtomicU64::new(0));
        let mut stream = CountingStream::new(ours, count.clone());

        stream.write_all(b"hello").await.unwrap();
        peer.write_all(b"abc").await.unwrap();
        let mut buf = [0u8; 3];
        stream.read_exact(&mut buf).await.unwrap();

        assert_eq!(count.load(Ordering::Relaxed), 8);
    }
}
