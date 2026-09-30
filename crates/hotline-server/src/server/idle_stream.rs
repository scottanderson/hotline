use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::{Instant, Sleep};

/// A stream whose reads fail with [`io::ErrorKind::TimedOut`] once `timeout`
/// passes with no byte received.
pub(crate) struct IdleStream<S> {
    inner: S,
    timeout: Duration,
    deadline: Pin<Box<Sleep>>,
}

impl<S> IdleStream<S> {
    pub(crate) fn new(inner: S, timeout: Duration) -> Self {
        Self {
            inner,
            timeout,
            deadline: Box::pin(tokio::time::sleep(timeout)),
        }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for IdleStream<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = &mut *self;
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(result) => {
                if buf.filled().len() > before {
                    let next = Instant::now() + this.timeout;
                    this.deadline.as_mut().reset(next);
                }
                Poll::Ready(result)
            }
            Poll::Pending => match this.deadline.as_mut().poll(cx) {
                Poll::Ready(()) => Poll::Ready(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "no data received",
                ))),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for IdleStream<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
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

    const TIMEOUT: Duration = Duration::from_secs(120);

    #[tokio::test(start_paused = true)]
    async fn a_silent_peer_times_out() {
        let (_peer, ours) = tokio::io::duplex(64);
        let mut stream = IdleStream::new(ours, TIMEOUT);

        let mut byte = [0u8; 1];
        let error = stream.read_exact(&mut byte).await.unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn each_received_byte_restarts_the_timer() {
        let (mut peer, ours) = tokio::io::duplex(64);
        let mut stream = IdleStream::new(ours, TIMEOUT);
        let mut byte = [0u8; 1];

        for _ in 0..3 {
            tokio::time::sleep(TIMEOUT - Duration::from_secs(1)).await;
            peer.write_all(b"x").await.unwrap();
            stream.read_exact(&mut byte).await.unwrap();
        }
    }
}
