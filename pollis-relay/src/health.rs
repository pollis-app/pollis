//! A tiny, dependency-light HTTP/1.1 health/version endpoint for orchestration.
//!
//! The relay proper is QUIC-only (UDP), which load-balancers and container
//! orchestrators can't probe with a plain TCP/HTTP liveness check. This module
//! stands up a minimal HTTP/1.1 responder on a **separate, opt-in TCP port**
//! (config `health_bind` / `--health-bind` / `POLLIS_RELAY_HEALTH_BIND`; unset ⇒
//! not started) that answers exactly two routes:
//!
//! - `GET /health`  → `200 OK`, body `ok` — liveness.
//! - `GET /version` → `200 OK`, JSON `{"service":"pollis-relay","sha":"<GIT_SHA>"}`
//!   — mirrors the DS `/version` tripwire so a deploy can confirm the *running*
//!   image is the one it just built (not merely that a workflow fired).
//!
//! Anything else is `404`; a non-`GET` method is `405`. It is hand-rolled on a
//! `tokio::net::TcpListener` (read the request line, match the path, write a fixed
//! response) rather than pulling in hyper/axum — the relay already has tokio and
//! the surface is two static routes.
//!
//! It runs as its own task so it never blocks the QUIC accept loop, and honors the
//! same shutdown signal as the relay (stops on SIGTERM/SIGINT). A **bind failure
//! is logged but does not take down the relay** — forwarding is the relay's job;
//! health is auxiliary, so losing it must not cost delivery.

use std::future::Future;
use std::net::SocketAddr;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// The git SHA this binary was built from, baked in at compile time by the Docker
/// build (`ARG GIT_SHA` → `ENV GIT_SHA` → `option_env!`). `"unknown"` for local
/// `cargo` builds. Reported at `/version` — mirrors `pollis-delivery`'s GIT_SHA so
/// a deploy can verify the new image is actually live, per the same tripwire.
pub const GIT_SHA: &str = match option_env!("GIT_SHA") {
    Some(s) => s,
    None => "unknown",
};

/// Bind the health endpoint on `bind` and spawn its serve loop, returning the task
/// handle and the bound address (useful when binding to port 0).
///
/// A **bind failure is logged and returns `Ok(None)`** — the relay keeps running
/// without a health endpoint rather than failing to boot, because forwarding is
/// the load-bearing job and this probe is auxiliary. (An operator who wants
/// fail-fast can treat a missing `/health` as a failed deploy from the outside.)
pub async fn spawn<F>(bind: SocketAddr, shutdown: F) -> anyhow::Result<Option<(JoinHandle<()>, SocketAddr)>>
where
    F: Future<Output = ()> + Send + 'static,
{
    match TcpListener::bind(bind).await {
        Ok(listener) => {
            let addr = listener.local_addr().unwrap_or(bind);
            let handle = tokio::spawn(serve(listener, shutdown));
            Ok(Some((handle, addr)))
        }
        Err(e) => {
            tracing::error!("health endpoint bind {bind} failed: {e} — continuing without it");
            Ok(None)
        }
    }
}

/// Serve health/version on an already-bound listener until `shutdown` resolves.
/// Each connection is handled on its own task; the accept loop exits promptly on
/// shutdown (biased select) so a rolling redeploy isn't held open by the probe.
pub async fn serve<F>(listener: TcpListener, shutdown: F)
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
                        tokio::spawn(handle_conn(stream));
                    }
                    Err(e) => {
                        tracing::debug!("health: accept failed: {e}");
                    }
                }
            }
        }
    }
}

/// Read one request, route it, write the response, and close. We only need the
/// request line for a liveness probe, so a single bounded read is sufficient — no
/// need to buffer the whole request or keep the connection alive.
async fn handle_conn(mut stream: TcpStream) {
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
    let response = route(method, path);
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
fn route(method: &str, path: &str) -> String {
    if method != "GET" {
        return http_response(405, "Method Not Allowed", "text/plain", "method not allowed");
    }
    // Ignore any query string (`/health?probe=1`).
    let path = path.split('?').next().unwrap_or(path);
    match path {
        "/health" => http_response(200, "OK", "text/plain", "ok"),
        "/version" => http_response(200, "OK", "application/json", &version_body()),
        _ => http_response(404, "Not Found", "text/plain", "not found"),
    }
}

/// The `/version` JSON body: `{"service":"pollis-relay","sha":"<GIT_SHA>"}`. Hand-
/// formatted (no serde_json) — the shape is fixed and the SHA is `[0-9a-f]`/`unknown`.
fn version_body() -> String {
    format!("{{\"service\":\"pollis-relay\",\"sha\":\"{GIT_SHA}\"}}")
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
        let handle = tokio::spawn(serve(listener, async move {
            let _ = rx.await;
        }));

        // GET /health → 200, body "ok".
        let (status, body) = request(addr, "/health").await;
        assert!(status.contains("200"), "health status: {status}");
        assert_eq!(body, "ok");

        // GET /version → 200, JSON carrying the SHA (baked or "unknown").
        let (status, body) = request(addr, "/version").await;
        assert!(status.contains("200"), "version status: {status}");
        assert!(body.contains("\"service\":\"pollis-relay\""), "version body: {body}");
        assert!(body.contains(GIT_SHA), "version body missing sha: {body}");

        // An unknown path → 404.
        let (status, _body) = request(addr, "/nope").await;
        assert!(status.contains("404"), "unknown-path status: {status}");

        // Signal shutdown; the serve loop must exit cleanly.
        let _ = tx.send(());
        handle.await.unwrap();
    }
}
