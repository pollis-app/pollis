//! The connector's contract: `https` is verified TLS (an untrusted chain or the
//! wrong name never yields a stream), `http` is plain TCP, anything else is
//! refused — and a real libsql remote handle actually dials through it.

use std::sync::Arc;

use pollis_libsql_connector::Connector;
use rcgen::{BasicConstraints, CertificateParams, IsCa, KeyPair};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use tokio_rustls::rustls::{self, RootCertStore, ServerConfig};
use tokio_rustls::TlsAcceptor;
use tower_service::Service;

/// A throwaway CA and a leaf it signed for `names`.
struct Pki {
    ca: CertificateDer<'static>,
    leaf: CertificateDer<'static>,
    leaf_key: PrivateKeyDer<'static>,
}

fn pki(names: &[&str]) -> Pki {
    let ca_key = KeyPair::generate().unwrap();
    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca = ca_params.self_signed(&ca_key).unwrap();

    let leaf_key = KeyPair::generate().unwrap();
    let leaf_params =
        CertificateParams::new(names.iter().map(|n| n.to_string()).collect::<Vec<_>>()).unwrap();
    let leaf = leaf_params.signed_by(&leaf_key, &ca, &ca_key).unwrap();

    Pki {
        ca: ca.der().clone(),
        leaf: leaf.der().clone(),
        leaf_key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    }
}

fn roots(ca: &CertificateDer<'static>) -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.add(ca.clone()).unwrap();
    roots
}

/// A one-connection TLS server: completes the handshake, reads one HTTP
/// request head, answers `500`, and returns the request head it saw (or the
/// handshake error).
async fn tls_server(pki: &Pki) -> (u16, tokio::task::JoinHandle<Result<String, String>>) {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![pki.leaf.clone()], pki.leaf_key.clone_key())
        .unwrap();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = TlsAcceptor::from(Arc::new(config));

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.map_err(|e| e.to_string())?;
        let alpn = tls.get_ref().1.alpn_protocol().map(|p| p.to_vec());
        assert_eq!(alpn.as_deref(), Some(&b"http/1.1"[..]), "client must offer http/1.1");
        let head = read_head(&mut tls).await;
        let _ = tls
            .write_all(b"HTTP/1.1 500 Internal Server Error\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
            .await;
        let _ = tls.shutdown().await;
        Ok(head)
    });
    (port, task)
}

async fn read_head<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if s.read(&mut byte).await.unwrap_or(0) == 0 {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

fn uri(s: &str) -> http::Uri {
    s.parse().unwrap()
}

#[tokio::test]
async fn https_with_a_trusted_chain_connects_over_tls() {
    let pki = pki(&["localhost"]);
    let (port, server) = tls_server(&pki).await;

    let mut connector = Connector::with_roots(roots(&pki.ca));
    let mut stream = connector
        .call(uri(&format!("https://localhost:{port}/")))
        .await
        .expect("trusted chain for the dialed name must connect");
    assert!(stream.is_tls());
    stream
        .write_all(b"GET /probe HTTP/1.1\r\nhost: localhost\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    assert!(response.starts_with(b"HTTP/1.1 500"));
    assert!(server.await.unwrap().unwrap().starts_with("GET /probe "));
}

#[tokio::test]
async fn https_rejects_a_chain_outside_the_trust_store() {
    let pki = pki(&["localhost"]);
    let (port, server) = tls_server(&pki).await;

    // The production constructor: Mozilla's roots, which never include a
    // throwaway CA.
    let mut connector = Connector::new();
    let err = connector
        .call(uri(&format!("https://localhost:{port}/")))
        .await
        .err()
        .expect("an untrusted chain must not yield a stream");
    assert!(err.to_string().contains("UnknownIssuer"), "{err}");
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn https_rejects_a_certificate_for_another_name() {
    let pki = pki(&["db.example.test"]);
    let (port, server) = tls_server(&pki).await;

    let mut connector = Connector::with_roots(roots(&pki.ca));
    let err = connector
        .call(uri(&format!("https://localhost:{port}/")))
        .await
        .err()
        .expect("a certificate for another host must not yield a stream");
    assert!(err.to_string().contains("not valid for name"), "{err}");
    assert!(server.await.unwrap().is_err());
}

#[tokio::test]
async fn http_is_plain_tcp() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut tcp, _) = listener.accept().await.unwrap();
        read_head(&mut tcp).await
    });

    let mut connector = Connector::new();
    let mut stream = connector
        .call(uri(&format!("http://127.0.0.1:{port}/")))
        .await
        .unwrap();
    assert!(!stream.is_tls());
    stream.write_all(b"GET /plain HTTP/1.1\r\n\r\n").await.unwrap();
    assert!(server.await.unwrap().starts_with("GET /plain "));
}

#[tokio::test]
async fn other_schemes_are_refused() {
    let mut connector = Connector::new();
    for u in ["ws://127.0.0.1:1/", "libsql://127.0.0.1:1/", "/no-scheme"] {
        let err = connector.call(uri(u)).await.err().expect(u);
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput, "{u}: {err}");
    }
}

/// libsql without `tls`, dialing `https` through [`Connector`]; the double answers 500.
#[tokio::test]
async fn libsql_remote_dials_through_the_connector() {
    let pki = pki(&["localhost"]);
    let (port, server) = tls_server(&pki).await;

    let db = libsql::Builder::new_remote(format!("https://localhost:{port}"), "token".into())
        .connector(Connector::with_roots(roots(&pki.ca)))
        .build()
        .await
        .unwrap();
    let conn = db.connect().unwrap();
    assert!(conn.execute("SELECT 1", ()).await.is_err());

    let head = server.await.unwrap().expect("libsql must complete the TLS handshake");
    assert!(head.starts_with("POST /v"), "hrana pipeline request expected, got {head:?}");
    assert!(head.to_ascii_lowercase().contains("authorization: bearer token"), "{head:?}");
}
