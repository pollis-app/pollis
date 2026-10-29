//! A tiny, dependency-light HTTP/1.1 health/version endpoint for orchestration.
//!
//! The relay is QUIC-only, so this serves an opt-in TCP port (`health_bind`; unset ⇒ off) with three routes:
//!
//! - `GET /health`  → `200 OK`, body `ok` — liveness.
//! - `GET /version` → `200 OK`, JSON
//!   `{"service":"pollis-relay","sha":"<GIT_SHA>","protocol":"<ALPN>","protocol_version":<N>}`
//!   — `sha` is the build identity and `protocol*` the wire identity; the reconciler keys on each (#703).
//! - `GET /peers`   → `200 OK`, JSON `{"peers":[{"cert_b64":"<base64 DER>"}]}` — parked peers' public leaf certs.
//!
//!   **Parked peers and nothing else** (§5.2/§11.7).
//!
//! Anything else is `404`; a non-`GET` method is `405`.
//!
//! A bind failure is logged but never takes down the relay.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::park::ParkedPeers;

/// The commit SHA baked in by the Docker build; `"unknown"` for local builds.
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// Bind the health endpoint on `bind` and spawn its serve loop, returning the task
/// handle and the bound address (useful when binding to port 0).
///
/// A bind failure is logged and returns `Ok(None)`.
///
/// `parked` is the live registry the relay itself splices into, so `/peers`
/// can never drift from what this node would actually accept an `Extend` for.
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

/// One bounded read of the request line, then route, respond and close.
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

/// Extract `(method, path)` from the first line of an HTTP request. Returns empty
/// strings on anything malformed — those fall through to a 404/405.
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

/// Map a method+path to a full HTTP/1.1 response string.
fn route(method: &str, path: &str, parked: &ParkedPeers) -> String {
    if method != "GET" {
        return http_response(405, "Method Not Allowed", "text/plain", "method not allowed");
    }
    // Ignore any query string (`/health?probe=1`).
    let path = path.split('?').next().unwrap_or(path);
    match path {
        "/health" => http_response(200, "OK", "text/plain", "ok"),
        "/version" => http_response(200, "OK", "application/json", &version_body()),
        "/peers" => http_response(200, "OK", "application/json", &peers_body(parked)),
        _ => http_response(404, "Not Found", "text/plain", "not found"),
    }
}

/// The `/peers` JSON body: `{"peers":[{"cert_b64":"<base64 DER>"}]}`, ordered by
/// fingerprint.
///
/// Hand-built from the leaves alone, so no field can quietly grow; base64 cannot break JSON.
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

/// The `/version` JSON body:
/// `{"service":"pollis-relay","sha":"<GIT_SHA>","protocol":"<ALPN>","protocol_version":<N>}`.
/// Hand-formatted: every field is a fixed shape, and `protocol*` come straight from [`crate::proto`].
fn version_body() -> String {
    // ALPN is an ASCII token, so this never lossily replaces.
    let protocol = std::str::from_utf8(crate::proto::ALPN).unwrap_or("");
    let protocol_version = crate::proto::PROTOCOL_VERSION;
    format!(
        "{{\"service\":\"pollis-relay\",\"sha\":\"{GIT_SHA}\",\"protocol\":\"{protocol}\",\"protocol_version\":{protocol_version}}}"
    )
}

/// Build a complete HTTP/1.1 response. `Connection: close` since each probe is a
/// one-shot request and we don't keep-alive.
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

    /// Send a raw request line to `addr`, return `(status_line, body)`.
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
        // Bind an ephemeral port (never a fixed/privileged one in tests).
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

        // GET /health → 200, body "ok".
        let (status, body) = request(addr, "/health").await;
        assert!(status.contains("200"), "health status: {status}");
        assert_eq!(body, "ok");

        // GET /version → 200, JSON with the SHA and the wire identity (#703).
        let (status, body) = request(addr, "/version").await;
        assert!(status.contains("200"), "version status: {status}");
        assert!(body.contains("\"service\":\"pollis-relay\""), "version body: {body}");
        assert!(body.contains(GIT_SHA), "version body missing sha: {body}");
        // The protocol identity must come straight from `proto`, so it stays in
        // lockstep with the ALPN the QUIC transport negotiates.
        let alpn = std::str::from_utf8(crate::proto::ALPN).unwrap();
        assert!(
            body.contains(&format!("\"protocol\":\"{alpn}\"")),
            "version body missing protocol: {body}"
        );
        assert!(
            body.contains(&format!("\"protocol_version\":{}", crate::proto::PROTOCOL_VERSION)),
            "version body missing protocol_version: {body}"
        );

        // GET /peers → 200, an empty list on a node nobody has parked at. Absent
        // is never "unknown" here: the relay always knows exactly who is parked.
        let (status, body) = request(addr, "/peers").await;
        assert!(status.contains("200"), "peers status: {status}");
        assert_eq!(body, "{\"peers\":[]}");

        // An unknown path → 404.
        let (status, _body) = request(addr, "/nope").await;
        assert!(status.contains("404"), "unknown-path status: {status}");

        // Signal shutdown; the serve loop must exit cleanly.
        let _ = tx.send(());
        handle.await.unwrap();
    }
}
