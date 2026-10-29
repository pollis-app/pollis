//! The connector every remote libsql handle in this workspace dials through.
//!
//! libsql's `tls` feature pins rustls 0.22 / webpki 0.102, which carry
//! RUSTSEC advisories (#1303), so libsql is built without it and gets this
//! rustls-0.23 connector instead. Trust anchors are `webpki-roots`, like
//! reqwest's `rustls-tls`.
//!
//! libsql without `tls` or a connector panics on the first remote connection,
//! so every `Builder::new_remote` must call `.connector(Connector::new())`.

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

    /// Verify servers against `roots` instead (tests with a throwaway CA).
    pub fn with_roots(roots: RootCertStore) -> Self {
        // Explicit provider: `builder()` panics when several rustls crypto
        // backends are compiled in.
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .expect("ring's provider supports rustls's default protocol versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        // libsql's hrana client speaks HTTP/1.1 only.
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

pub enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl Stream {
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
