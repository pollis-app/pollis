//! Opaque byte-pipe stream types; the caller runs its own TLS over the top.

use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// Anything that is both `AsyncRead` and `AsyncWrite` (blanket impl).
pub trait DuplexStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> DuplexStream for T {}

/// Type-erased [`DuplexStream`], so the shim pipes a circuit and a direct dial alike.
pub struct BoxedStream(Pin<Box<dyn DuplexStream>>);

impl BoxedStream {
    pub fn new<S: DuplexStream + 'static>(inner: S) -> Self {
        BoxedStream(Box::pin(inner))
    }
}

impl AsyncRead for BoxedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_read(cx, buf)
    }
}

impl AsyncWrite for BoxedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.0.as_mut().poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        self.0.as_mut().poll_shutdown(cx)
    }
}

/// A relay hop's bidirectional byte pipe: one quinn stream in each direction.
///
/// The owned `Connection`/`Endpoint` are never read; they keep the QUIC
/// session alive, since dropping the endpoint tears the connection down.
pub struct RelayStream {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    _conn: Option<quinn::Connection>,
    _endpoint: Option<quinn::Endpoint>,
}

impl RelayStream {
    pub(crate) fn new(
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        conn: Option<quinn::Connection>,
        endpoint: Option<quinn::Endpoint>,
    ) -> Self {
        RelayStream {
            send,
            recv,
            _conn: conn,
            _endpoint: endpoint,
        }
    }
}

impl AsyncRead for RelayStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        // Fully-qualified so quinn's inherent methods don't shadow the trait.
        AsyncRead::poll_read(Pin::new(&mut self.recv), cx, buf)
    }
}

impl AsyncWrite for RelayStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.send), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.send), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.send), cx)
    }
}
