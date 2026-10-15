//! The connector every remote libsql handle in this workspace dials through.
//!
//! libsql's built-in `tls` feature wraps hyper-rustls 0.25, which pins rustls
//! 0.22 and the rustls-webpki 0.102 line that RUSTSEC-2026-0049/0098/0099 land
//! on — and libsql 0.10 still does (#1303). There is no hyper-0.14-compatible
//! hyper-rustls on rustls 0.23, so the workspace builds libsql WITHOUT `tls` and
//! passes [`Connector`] to `Builder::connector` instead: a plain tokio
//! `TcpStream`, wrapped in rustls 0.23 when the URL is `https`.
//!
//! Behaviour matches the connector it replaces (`https_or_http`, HTTP/1.1 only,
//! `TCP_NODELAY`), with one deliberate difference: the trust anchors are
//! Mozilla's root set compiled in (`webpki-roots`) rather than the host's CA
//! bundle — the same anchors reqwest's `rustls-tls` already uses for the DS's
//! other outbound calls.
//!
//! libsql without `tls` and without a connector PANICS on the first remote
//! connection ("you must provide your own http connector"), so every
//! `Builder::new_remote` in the workspace must call `.connector(Connector::new())`.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use hyper::client::connect::{Connected, Connection};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{self, ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

/// A `tower::Service<http::Uri>` that opens an `http` or `https` connection.
#[derive(Clone)]
pub struct Connector {
    tls: TlsConnector,
}

impl Connector {
    /// Verify servers against Mozilla's root set (`webpki-roots`).
    pub fn new() -> Self {
        let roots = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
        };
        Self::with_roots(roots)
    }

    /// Verify servers against `roots` instead. For tests, which stand up a TLS
    /// server under a throwaway CA; production uses [`Connector::new`].
    pub fn with_roots(roots: RootCertStore) -> Self {
        // An explicit provider rather than `ClientConfig::builder()`: the
        // process-wide default is ambiguous (and `builder()` panics) whenever
        // more than one rustls crypto backend is compiled into the binary.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring's provider supports rustls's default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        // libsql's hrana client speaks HTTP/1.1 only (the replaced connector
        // was built with `enable_http1()` and nothing else).
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Self {
            tls: TlsConnector::from(Arc::new(config)),
        }
    }
}

impl Default for Connector {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Connector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector").finish_non_exhaustive()
    }
}

impl tower_service::Service<http::Uri> for Connector {
    type Response = Stream;
    type Error = io::Error;
    type Future = Pin<Box<dyn Future<Output = io::Result<Stream>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: http::Uri) -> Self::Future {
        let tls = self.tls.clone();
        Box::pin(async move { connect(&tls, &uri).await })
    }
}

async fn connect(tls: &TlsConnector, uri: &http::Uri) -> io::Result<Stream> {
    let is_tls = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        other => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported scheme {other:?} in {uri}"),
            ));
        }
    };
    let host = uri
        .host()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, format!("no host in {uri}")))?;
    let port = uri.port_u16().unwrap_or(if is_tls { 443 } else { 80 });
    // `http::Uri` keeps IPv6 literals bracketed; neither the resolver nor
    // rustls's `ServerName` wants the brackets.
    let host = host.trim_start_matches('[').trim_end_matches(']');

    let tcp = TcpStream::connect((host, port)).await?;
    tcp.set_nodelay(true)?;
    if !is_tls {
        return Ok(Stream::Plain(tcp));
    }

    let server_name = ServerName::try_from(host.to_string())
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let tls_stream = tls.connect(server_name, tcp).await?;
    Ok(Stream::Tls(Box::new(tls_stream)))
}

/// The connection [`Connector`] hands libsql: plain TCP for `http`, rustls over
/// TCP for `https`.
pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
    /// Whether this connection is TLS-wrapped.
    pub fn is_tls(&self) -> bool {
        matches!(self, Stream::Tls(_))
    }
}

impl Connection for Stream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_write_vectored(cx, bufs),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_write_vectored(cx, bufs),
        }
    }

    fn is_write_vectored(&self) -> bool {
        match self {
            Stream::Plain(s) => s.is_write_vectored(),
            Stream::Tls(s) => s.is_write_vectored(),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_flush(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Stream::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Stream::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}
