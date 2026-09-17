//! Socket-level deadlines: a stalled HTTP body is not necessarily polled again.
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::Sleep,
};

pub struct WriteTimeout<T> {
    inner: T,
    duration: Duration,
    pending: Option<Pin<Box<Sleep>>>,
}

impl<T> WriteTimeout<T> {
    /// Wrap an IO transport, timing only writes/flushes that are actually pending.
    pub fn new(inner: T, duration: Duration) -> Self {
        Self {
            inner,
            duration,
            pending: None,
        }
    }

    fn deadline<U>(
        &mut self,
        cx: &mut Context<'_>,
        result: Poll<io::Result<U>>,
    ) -> Poll<io::Result<U>> {
        if result.is_ready() {
            self.pending = None;
            return result;
        }
        let timer = self
            .pending
            .get_or_insert_with(|| Box::pin(tokio::time::sleep(self.duration)));
        if timer.as_mut().poll(cx).is_ready() {
            Poll::Ready(Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "HTTP response write stalled",
            )))
        } else {
            Poll::Pending
        }
    }
}

impl<T: AsyncRead + Unpin> AsyncRead for WriteTimeout<T> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for WriteTimeout<T> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.deadline(cx, result)
    }
    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.deadline(cx, result)
    }
    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_flush(cx);
        self.deadline(cx, result)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let result = Pin::new(&mut self.inner).poll_shutdown(cx);
        self.deadline(cx, result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn pending_writes_timeout_but_idle_time_is_not_counted() {
        let (writer, mut reader) = tokio::io::duplex(1);
        let mut writer = WriteTimeout::new(writer, Duration::from_millis(20));
        tokio::time::sleep(Duration::from_millis(40)).await;
        writer
            .write_all(b"x")
            .await
            .expect("idle connection can write");
        assert_eq!(
            writer
                .write_all(b"y")
                .await
                .expect_err("stalled write")
                .kind(),
            io::ErrorKind::TimedOut
        );
        let mut byte = [0];
        reader.read_exact(&mut byte).await.expect("drain");
        writer
            .write_all(b"z")
            .await
            .expect("progress resets timeout");
    }
}
