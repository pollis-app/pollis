//! A hand-rolled HTTP/1.1 probe endpoint on an opt-in TCP port, since the
//! QUIC-only relay can't be probed over TCP.
//!
//! - `GET /health`  → `ok`.
//! - `GET /version` → build `sha` plus wire `protocol`/`protocol_version`. Both
//!   are reported because the reconciler keys directory membership on the wire
//!   identity and stale-node cycling on the build (#703).
//! - `GET /peers`   → leaf certs of peers parked here (#813), already public via
//!   the signed directory. Parked peers and nothing else: §5.2/§11.7 forbid a
//!   relay reporting who its users are.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::park::ParkedPeers;

/// Build SHA baked in by the Docker build; `"unknown"` for local builds.
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// Bind and spawn the serve loop, returning the task and bound address.
///
/// A bind failure is logged and returns `Ok(None)`: the probe is auxiliary and
/// must not stop forwarding.
pub async fn spawn<F>(
    bind: SocketAddr,
    shutdown: F,
    parked: Arc<ParkedPeers>,
) -> anyhow::Result<Option<(JoinHandle<()>, SocketAddr)>>
where
    F: Future<Output = ()> + Send + 'static,
{
    match TcpListener::bind(bind).await {
        Ok(listener) => {
            let addr = listener.local_addr().unwrap_or(bind);
            let handle = tokio::spawn(serve(listener, shutdown, parked));
            Ok(Some((handle, addr)))
        }
        Err(e) => {
            tracing::error!("health endpoint bind {bind} failed: {e} — continuing without it");
            Ok(None)
        }
    }
}

/// Serve on an already-bound listener until `shutdown` resolves.
pub async fn serve<F>(listener: TcpListener, shutdown: F, parked: Arc<ParkedPeers>)
where
    F: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                break;
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _peer)) => {
                        tokio::spawn(handle_conn(stream, parked.clone()));
                    }
                    Err(e) => {
                        tracing::debug!("health: accept failed: {e}");
                    }
                }
            }
        }
    }
}

/// One bounded read is enough: only the request line matters.
async fn handle_conn(mut stream: TcpStream, parked: Arc<ParkedPeers>) {
    let mut buf = [0u8; 1024];
    let n = match stream.read(&mut buf).await {
        Ok(0) => {
            return;
        }
        Ok(n) => n,
        Err(_) => {
            return;
        }
    };

    let (method, path) = parse_request_line(&buf[..n]);
    let response = route(method, path, &parked);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.flush().await;
}

/// `(method, path)` from the request line; empty strings when malformed.
fn parse_request_line(req: &[u8]) -> (&str, &str) {
    let line_end = req
        .iter()
        .position(|&b| b == b'\r' || b == b'\n')
        .unwrap_or(req.len());
    let line = std::str::from_utf8(&req[..line_end]).unwrap_or("");
    let mut parts = line.split(' ');
    let method = parts.next().unwrap_or("");
    let path = parts.next().unwrap_or("");
    (method, path)
}

fn route(method: &str, path: &str, parked: &ParkedPeers) -> String {
    if method != "GET" {
        return http_response(405, "Method Not Allowed", "text/plain", "method not allowed");
    }
    let path = path.split('?').next().unwrap_or(path);
    match path {
        "/health" => http_response(200, "OK", "text/plain", "ok"),
        "/version" => http_response(200, "OK", "application/json", &version_body()),
        "/peers" => http_response(200, "OK", "application/json", &peers_body(parked)),
        _ => http_response(404, "Not Found", "text/plain", "not found"),
    }
}

/// `{"peers":[{"cert_b64":"<base64 DER>"}]}`, ordered by fingerprint.
///
/// Hand-formatted from the leaves alone, not from a struct that could quietly
/// grow a field.
fn peers_body(parked: &ParkedPeers) -> String {
    use base64::Engine as _;

    let b64 = base64::engine::general_purpose::STANDARD;
    let entries: Vec<String> = parked
        .peer_leaves()
        .iter()
        .map(|leaf| format!("{{\"cert_b64\":\"{}\"}}", b64.encode(leaf)))
        .collect();
    format!("{{\"peers\":[{}]}}", entries.join(","))
}

/// Hand-formatted: every field is a fixed, JSON-safe shape.
fn version_body() -> String {
    let protocol = std::str::from_utf8(crate::proto::ALPN).unwrap_or("");
    let protocol_version = crate::proto::PROTOCOL_VERSION;
    format!(
        "{{\"service\":\"pollis-relay\",\"sha\":\"{GIT_SHA}\",\"protocol\":\"{protocol}\",\"protocol_version\":{protocol_version}}}"
    )
}

fn http_response(status: u16, reason: &str, content_type: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {len}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        len = body.len(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    async fn request(addr: SocketAddr, target: &str) -> (String, String) {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let req = format!("GET {target} HTTP/1.1\r\nHost: localhost\r\n\r\n");
        stream.write_all(req.as_bytes()).await.unwrap();
        stream.flush().await.unwrap();
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).await.unwrap();
        let text = String::from_utf8_lossy(&raw).into_owned();
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((text.as_str(), ""));
        let status = head.lines().next().unwrap_or("").to_string();
        (status, body.to_string())
    }

    #[tokio::test]
    async fn serves_health_version_and_404_then_shuts_down() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = oneshot::channel::<()>();
        let parked = ParkedPeers::new();
        let handle = tokio::spawn(serve(
            listener,
            async move {
                let _ = rx.await;
            },
            parked,
        ));

        let (status, body) = request(addr, "/health").await;
        assert!(status.contains("200"), "health status: {status}");
        assert_eq!(body, "ok");

        let (status, body) = request(addr, "/version").await;
        assert!(status.contains("200"), "version status: {status}");
        assert!(body.contains("\"service\":\"pollis-relay\""), "version body: {body}");
        assert!(body.contains(GIT_SHA), "version body missing sha: {body}");
        let alpn = std::str::from_utf8(crate::proto::ALPN).unwrap();
        assert!(
            body.contains(&format!("\"protocol\":\"{alpn}\"")),
            "version body missing protocol: {body}"
        );
        assert!(
            body.contains(&format!("\"protocol_version\":{}", crate::proto::PROTOCOL_VERSION)),
            "version body missing protocol_version: {body}"
        );

        let (status, body) = request(addr, "/peers").await;
        assert!(status.contains("200"), "peers status: {status}");
        assert_eq!(body, "{\"peers\":[]}");

        let (status, _body) = request(addr, "/nope").await;
        assert!(status.contains("404"), "unknown-path status: {status}");

        let _ = tx.send(());
        handle.await.unwrap();
    }
}
