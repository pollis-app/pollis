//! A tiny dev/demo HTTP server over a generated artifact directory.
//!
//! This exists for local testing and demos only. The real deployment is "drop
//! the generated `/v1` directory on a static host" — there is no query service,
//! no database, no app logic to run. This server just maps a request path to a
//! file under the root and serves it with the right `Content-Type` and the
//! cache policy the artifacts are designed for:
//!
//! * immutable artifacts (everything except the two that move) →
//!   `Cache-Control: public, max-age=31536000, immutable`
//! * `sth/latest.json` and `index.json` (which move as the log grows) →
//!   `Cache-Control: no-cache`
//!
//! The server is read-only and serves the public, unauthenticated read API by
//! design — there are no credentials anywhere on this path.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::error::{Result, ServeError};

const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
const SHORT_CACHE: &str = "no-cache";

/// A running dev server bound to a `127.0.0.1` port, served from a background
/// thread. Dropping it (or calling [`DevServer::shutdown`]) stops the thread.
pub struct DevServer {
    addr: std::net::SocketAddr,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl DevServer {
    /// Bind `127.0.0.1:<port>` (pass `0` for an ephemeral port) and start
    /// serving `root` from a background thread. Returns once the socket is
    /// bound, so [`DevServer::port`] is immediately usable.
    pub fn spawn(root: PathBuf, port: u16) -> Result<DevServer> {
        let server = Server::http(("127.0.0.1", port))
            .map_err(|e| ServeError::Http(format!("bind failed: {e}")))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| ServeError::Http("server bound to a non-IP address".into()))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let handle = std::thread::spawn(move || serve_loop(server, &root, &stop_thread));

        Ok(DevServer {
            addr,
            stop,
            handle: Some(handle),
        })
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
        if let Some(handle) = &self.handle {
            // The serve loop only returns on `stop`, which we never set here.
            while !handle.is_finished() {
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    /// Stop the background thread and wait for it to exit.
    pub fn shutdown(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for DevServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Accept requests until `stop` is set, polling so shutdown is prompt.
fn serve_loop(server: Server, root: &Path, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_request(request, root),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

/// Map one request to a file under `root` and respond. Read-only: only `GET`
/// (and `HEAD`) are answered; anything else is `405`.
fn handle_request(request: Request, root: &Path) {
    let method = request.method().clone();
    if method != Method::Get && method != Method::Head {
        respond_status(request, 405, "method not allowed");
        return;
    }

    // Strip any query string and the leading slash.
    let url = request.url().to_string();
    let path_part = url.split('?').next().unwrap_or("");
    let rel = path_part.trim_start_matches('/');

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

    let cache = cache_control_for(path_part);
    let response = Response::from_data(bytes)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", cache));
    let _ = request.respond(response);
}

/// The cache policy for a request path: short for the two documents that move,
/// long-immutable for everything else.
fn cache_control_for(path: &str) -> &'static str {
    if path.ends_with("/latest.json") || path.ends_with("/index.json") {
        SHORT_CACHE
    } else {
        IMMUTABLE_CACHE
    }
}

fn respond_status(request: Request, status: u16, message: &str) {
    let body = format!("{{\"error\":\"{message}\"}}");
    let response = Response::from_string(body)
        .with_status_code(status)
        .with_header(header("Content-Type", "application/json"));
    let _ = request.respond(response);
}

/// Build a header, falling back silently if the static inputs ever fail to
/// parse (they don't — both are valid header bytes).
fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes())
        .unwrap_or_else(|_| Header::from_bytes(&b"X-Invalid"[..], &b"1"[..]).unwrap())
}
