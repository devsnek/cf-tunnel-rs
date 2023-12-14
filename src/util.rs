use std::{marker::Unpin, pin::Pin};
use tokio::io::{AsyncRead, AsyncWrite};

#[pin_project::pin_project]
pub struct Join<W, R> {
    #[pin]
    writer: W,
    #[pin]
    reader: R,
}

impl<W, R> Join<W, R>
where
    W: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    pub fn new(writer: W, reader: R) -> Self {
        Self { writer, reader }
    }

    pub fn split(self) -> (W, R) {
        (self.writer, self.reader)
    }
}

impl<W, R> AsyncWrite for Join<W, R>
where
    W: AsyncWrite + Unpin,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        self.project().writer.poll_write(cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().writer.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().writer.poll_shutdown(cx)
    }
}

impl<W, R> AsyncRead for Join<W, R>
where
    R: AsyncRead + Unpin,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context,
        buf: &mut tokio::io::ReadBuf,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        self.project().reader.poll_read(cx, buf)
    }
}
