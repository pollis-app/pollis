//! A tiny dev/demo HTTP server over a generated artifact directory.
//!
//! This exists for local testing and demos only. The real deployment of the
//! *read API* is "drop the generated `/v1` directory on a static host" — there
//! is no query service, no database, no app logic to run. This server maps a
//! request path to a file under the root and serves it with the right
//! `Content-Type` and cache policy:
//!
//! * immutable artifacts (everything except the two that move) →
//!   `Cache-Control: public, max-age=31536000, immutable`
//! * `sth/latest.json` and `index.json` (which move as the log grows) →
//!   `Cache-Control: no-cache`
//!
//! On top of the static read API it also exposes ONE dynamic endpoint —
//! `GET /verify/group/<id>` — which runs the shared [`crate::group::verify_group`]
//! against this server's own `/v1` base and returns a [`crate::group::GroupReport`]
//! as JSON. It carries `Access-Control-Allow-Origin: *` (and answers `OPTIONS`
//! preflight) so the static marketing site can call it cross-origin, and it is
//! served `no-cache` because it is computed per request, not immutable.
//!
//! The static read API is credential-free by design; the verify endpoint takes
//! no auth either — it only re-derives a verdict anyone could compute from the
//! public artifacts.

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

/// A running dev server bound to a `127.0.0.1` port, served from a pool of
/// background threads. Dropping it (or calling [`DevServer::shutdown`]) stops
/// the pool.
pub struct DevServer {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl DevServer {
    /// Bind `127.0.0.1:<port>` (pass `0` for an ephemeral port) and start
    /// serving `root` from a pool of background threads. Returns once the
    /// socket is bound, so [`DevServer::port`] is immediately usable.
    pub fn spawn(root: PathBuf, port: u16) -> Result<DevServer> {
        let server = Server::http(("127.0.0.1", port))
            .map_err(|e| ServeError::Http(format!("bind failed: {e}")))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| ServeError::Http("server bound to a non-IP address".into()))?;

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

    /// The bound port (resolved even when `0` was requested).
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Base URL a client should hit, e.g. `http://127.0.0.1:54321`.
    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Block the calling thread, keeping the server alive until the process is
    /// killed. Used by the CLI `serve` command.
    pub fn block_forever(self) {
        // The serve loops only return on `stop`, which we never set here.
        while self.handles.iter().any(|h| !h.is_finished()) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    /// Stop the background threads and wait for them to exit.
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

/// Accept requests until `stop` is set, polling so shutdown is prompt.
fn serve_loop(server: &Server, root: &Path, base_url: &str, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_request(request, root, base_url),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

/// Route one request. `OPTIONS` is answered as a CORS preflight; the dynamic
/// verify endpoint is handled specially; everything else is a read-only static
/// file lookup under `root`.
fn handle_request(request: Request, root: &Path, base_url: &str) {
    let method = request.method().clone();

    // Strip any query string and keep the path.
    let url = request.url().to_string();
    let path = url.split('?').next().unwrap_or("").to_string();

    // CORS preflight for the browser calling the verify endpoint cross-origin.
    if method == Method::Options {
        cors_preflight(request);
        return;
    }

    // Dynamic per-group verification endpoint.
    if let Some(encoded) = path.strip_prefix(VERIFY_GROUP_PREFIX) {
        if method != Method::Get {
            respond_status(request, 405, "method not allowed");
            return;
        }
        handle_verify_group(request, base_url, encoded);
        return;
    }

    // Static read API: GET/HEAD only.
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

/// Run the shared per-group verifier against our own `/v1` base and return the
/// [`crate::group::GroupReport`] as JSON. This is the SAME function the CLI
/// calls, so the two can never disagree.
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

/// Answer a CORS preflight (`OPTIONS`) so the static site can call the verify
/// endpoint cross-origin.
pub(crate) fn cors_preflight(request: Request) {
    let response = Response::empty(204)
        .with_header(header("Access-Control-Allow-Origin", "*"))
        .with_header(header("Access-Control-Allow-Methods", "GET, OPTIONS"))
        .with_header(header("Access-Control-Allow-Headers", "*"))
        .with_header(header("Access-Control-Max-Age", "86400"));
    let _ = request.respond(response);
}

/// The cache policy for a request path: short for the two documents that move,
/// long-immutable for everything else.
pub(crate) fn cache_control_for(path: &str) -> &'static str {
    if path.ends_with("/latest.json") || path.ends_with("/index.json") {
        SHORT_CACHE
    } else {
        IMMUTABLE_CACHE
    }
}

/// Respond with a JSON body, the cross-origin header (so the static site can
/// read it), and `no-cache` (the verdict is computed, not immutable).
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

/// Decode `%XX` percent-escapes in a single URL path segment. Unrecognised or
/// truncated escapes are passed through literally; invalid UTF-8 is replaced
/// rather than panicking. (`+` is left as-is — it is a literal in a path, only
/// a space in a query string.)
pub(crate) fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            match (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                (Some(h), Some(l)) => {
                    out.push(h * 16 + l);
                    i += 3;
                    continue;
                }
                _ => {}
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

/// Minimal JSON string escaping for the small error messages we embed by hand.
pub(crate) fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

/// Build a header, falling back silently if the static inputs ever fail to
/// parse (they don't — both are valid header bytes).
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
        // Truncated / invalid escapes pass through literally, never panic.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }
}
