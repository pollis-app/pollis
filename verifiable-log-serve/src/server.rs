//! Dev/demo HTTP server over a generated artifact directory. Production is a
//! static host; this only adds per-path `Content-Type` and cache policy, plus
//! one dynamic, unauthenticated, CORS-open endpoint `GET /verify/group/<id>`
//! that re-derives a verdict anyone could compute from the public artifacts.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::error::{Result, ServeError};
use crate::group::verify_group;

pub(crate) const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
pub(crate) const SHORT_CACHE: &str = "no-cache";

/// Prefix of the dynamic per-group verification endpoint.
pub(crate) const VERIFY_GROUP_PREFIX: &str = "/verify/group/";

/// Worker threads handling requests. The verify endpoint makes blocking HTTP
/// calls back to this same server (to fetch the static artifacts it verifies),
/// so more than one worker is required or that self-request would deadlock.
const WORKERS: usize = 4;

/// A running dev server on `127.0.0.1`. Dropping it (or calling
/// [`DevServer::shutdown`]) stops the worker pool.
pub struct DevServer {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl DevServer {
    /// Bind `127.0.0.1:<port>` (`0` = ephemeral) and start serving `root`.
    /// Returns once bound, so [`DevServer::port`] is immediately usable.
    pub fn spawn(root: PathBuf, port: u16) -> Result<DevServer> {
        let server = Server::http(("127.0.0.1", port))
            .map_err(|e| ServeError::Http(format!("bind failed: {e}")))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| ServeError::Http("server bound to a non-IP address".into()))?;

        // Trust the served log's own keys so a local dev log verifies without the
        // production pin. Safe in every profile: only a process serving a log
        // reaches this, and the auditor CLI serves nothing.
        for rel in [
            "v1/public_key.json",
            "v1/commit-log-2/public_key.json",
            "v1/account-keys/public_key.json",
        ] {
            if let Ok(txt) = std::fs::read_to_string(root.join(rel)) {
                if let Ok(doc) = serde_json::from_str::<crate::bundle::PublicKeyDoc>(&txt) {
                    crate::pinned::trust_hex_for_dev(&doc.public_key);
                    for k in &doc.keys {
                        crate::pinned::trust_hex_for_dev(&k.public_key);
                    }
                }
            }
        }

        let server = Arc::new(server);
        let base_url = Arc::new(format!("http://{addr}"));
        let stop = Arc::new(AtomicBool::new(false));

        let handles = (0..WORKERS)
            .map(|_| {
                let server = server.clone();
                let root = root.clone();
                let base_url = base_url.clone();
                let stop = stop.clone();
                std::thread::spawn(move || serve_loop(&server, &root, &base_url, &stop))
            })
            .collect();

        Ok(DevServer { addr, stop, handles })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Base URL, e.g. `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Block until the process is killed. Used by the CLI `serve` command.
    pub fn block_forever(self) {
        while self.handles.iter().any(|h| !h.is_finished()) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn serve_loop(server: &Server, root: &Path, base_url: &str, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_request(request, root, base_url),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

fn handle_request(request: Request, root: &Path, base_url: &str) {
    let method = request.method().clone();

    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();

    if method == Method::Options {
        cors_preflight(request);
        return;
    }

    if let Some(encoded) = path.strip_prefix(VERIFY_GROUP_PREFIX) {
        if method != Method::Get {
            respond_status(request, 405, "method not allowed");
            return;
        }
        handle_verify_group(request, base_url, encoded);
        return;
    }

    if method != Method::Get && method != Method::Head {
        respond_status(request, 405, "method not allowed");
        return;
    }

    let rel = path.trim_start_matches('/');

    // Reject empty paths and any traversal attempt outright.
    if rel.is_empty() || rel.split('/').any(|seg| seg == "..") {
        respond_status(request, 404, "not found");
        return;
    }

    let file = root.join(rel);
    let bytes = match std::fs::read(&file) {
        Ok(b) => b,
        Err(_) => {
            respond_status(request, 404, "not found");
            return;
        }
    };

    let cache = cache_control_for(&path);
    let response = Response::from_data(bytes)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", cache));
    let _ = request.respond(response);
}

/// Runs the same verifier the CLI calls, so the two can never disagree.
fn handle_verify_group(request: Request, base_url: &str, encoded_id: &str) {
    let id = percent_decode(encoded_id);
    if id.is_empty() {
        respond_json(request, 400, r#"{"error":"missing group id"}"#.to_string());
        return;
    }

    match verify_group(base_url, &id) {
        Ok(report) => match serde_json::to_string(&report) {
            Ok(body) => respond_json(request, 200, body),
            Err(e) => respond_json(
                request,
                500,
                format!("{{\"error\":\"failed to serialize report: {}\"}}", escape(&e.to_string())),
            ),
        },
        // A transport/parse failure of the underlying artifacts — distinct from
        // a group that merely fails verification (that returns Ok + 200).
        Err(e) => respond_json(
            request,
            502,
            format!("{{\"error\":\"{}\"}}", escape(&e.to_string())),
        ),
    }
}

pub(crate) fn cors_preflight(request: Request) {
    let response = Response::empty(204)
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "*"))
        .with_header(header("Access-Control-Max-Age", "86400"));
    let _ = request.respond(response);
}

pub(crate) fn cache_control_for(path: &str) -> &'static str {
    if path.ends_with("/latest.json") || path.ends_with("/index.json") {
        SHORT_CACHE
    } else {
        IMMUTABLE_CACHE
    }
}

/// JSON body, CORS-open, `no-cache` (the verdict is computed, not immutable).
pub(crate) fn respond_json(request: Request, status: u16, body: String) {
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Cache-Control", SHORT_CACHE));
    let _ = request.respond(response);
}

pub(crate) fn respond_status(request: Request, status: u16, message: &str) {
    let body = format!("{{\"error\":\"{message}\"}}");
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"));
    let _ = request.respond(response);
}

/// Decode `%XX` escapes in one path segment. Bad escapes pass through literally;
/// `+` stays literal (it means space only in a query string).
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

pub(crate) fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

/// Infallible for the static header names and values used here.
pub(crate) fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .unwrap_or_else(|_| Header::from_bytes(&b"X-Invalid"[..], &b"1"[..]).unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_escapes_and_passthrough() {
        assert_eq!(percent_decode("conv-a"), "conv-a");
        assert_eq!(percent_decode("group%20one"), "group one");
        assert_eq!(percent_decode("a%2Fb"), "a/b");
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
