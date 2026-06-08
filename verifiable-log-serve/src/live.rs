//! A **live, lazily-refreshed** HTTP server over the verifiable log, reading the
//! commit log directly from Turso/libSQL instead of a pre-generated directory.
//!
//! Where [`crate::server::DevServer`] serves a frozen `/v1` snapshot off disk,
//! this server holds the same surface **in memory** and rebuilds it on demand:
//!
//! * It keeps the current signed [`Bundle`], the generated `/v1` artifact map
//!   ([`layout::generate_artifacts`]), and the [`Instant`] it was last built.
//! * Every request first **ensures freshness**: if the cache is older than the
//!   TTL it is rebuilt (one DB pull → [`build_bundle`] → regenerate artifacts →
//!   atomic swap); otherwise it is served untouched, with **no DB hit**.
//! * Rebuilds are **single-flight**: a mutex with a double-checked TTL test means
//!   that however many requests pile up during a stale window, exactly one of
//!   them pulls the DB and the rest serve the freshly-swapped cache. The net
//!   property is at most one DB pull per TTL regardless of request volume.
//! * A rebuild that fails (DB down, a fork/regression the builder rejects) keeps
//!   serving the last-good cache and logs the error — it never crashes.
//!
//! Public surface is identical to the static mode: the immutable `/v1/...`
//! artifacts (with the same cache policy) plus the dynamic
//! `GET /verify/group/<id>` endpoint. The per-group verdict is computed by
//! [`verify_group_in_bundle`] against the in-memory bundle directly — the server
//! never HTTP-fetches itself — so it shares the exact verdict core the CLI uses.
//!
//! Only this serve runtime reads the clock (TTL instants, the STH timestamp);
//! the [`verifiable_log`] core and the [`verifiable_log_builder`] bundle logic
//! stay clock-free.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ed25519_dalek::SigningKey;
use tiny_http::{Method, Request, Response, Server};
use verifiable_log_builder::{build_bundle, source};

use crate::bundle::Bundle;
use crate::error::{Result, ServeError};
use crate::group::verify_group_in_bundle;
use crate::layout;
use crate::server::{
    cache_control_for, cors_preflight, escape, header, percent_decode, respond_json,
    respond_status, VERIFY_GROUP_PREFIX,
};

/// Number of request worker threads. Unlike the static server, the live verify
/// endpoint does **not** call back into itself over HTTP, so a single rebuild can
/// never deadlock a worker; multiple workers just keep cached reads concurrent.
const WORKERS: usize = 4;

/// The in-memory cache swapped in on each rebuild. Immutable once built; a
/// rebuild produces a fresh `Cache` and atomically replaces the `Arc`.
struct Cache {
    /// The signed bundle, used to answer `/verify/group/<id>` directly.
    bundle: Bundle,
    /// `relative path -> JSON bytes` for the `/v1` read API (see
    /// [`layout::generate_artifacts`]).
    artifacts: BTreeMap<String, Vec<u8>>,
    /// When this cache was built — the TTL is measured from here.
    built_at: Instant,
}

/// State shared by every worker thread: the DB source, the signing key, the TTL,
/// a tokio runtime for the async DB read, and the single-flight cache.
struct Shared {
    db: String,
    signing_key: SigningKey,
    ttl: Duration,
    runtime: tokio::runtime::Runtime,
    /// Last-good cache. `None` until the first successful rebuild (the server
    /// starts cold — no DB load until the first request).
    cache: RwLock<Option<Arc<Cache>>>,
    /// Single-flight guard: only the holder may rebuild.
    rebuild_lock: Mutex<()>,
    /// Count of DB pulls performed — exposed for tests to assert single-flight
    /// and TTL behaviour.
    rebuild_count: AtomicU64,
}

impl Shared {
    /// Is the cache fresh enough to serve without rebuilding? A zero TTL means
    /// "always rebuild", so it is never fresh.
    fn is_fresh(&self, cache: &Cache) -> bool {
        !self.ttl.is_zero() && cache.built_at.elapsed() < self.ttl
    }

    /// Return a cache guaranteed fresh per the TTL, rebuilding once under the
    /// single-flight lock if stale. On a rebuild failure the last-good cache is
    /// returned (and the error logged); only a failure with *no* last-good cache
    /// surfaces as `Err`.
    fn ensure_fresh(&self) -> Result<Arc<Cache>> {
        // Fast path: a fresh cache needs no lock and no DB hit.
        if let Some(cache) = self.cache.read().unwrap().as_ref() {
            if self.is_fresh(cache) {
                return Ok(cache.clone());
            }
        }

        // Stale (or cold): serialise rebuilds. Whoever loses the race for the
        // lock re-checks below and serves the winner's fresh cache.
        let _flight = self.rebuild_lock.lock().unwrap();

        // Double-check: a concurrent holder may have just rebuilt.
        if let Some(cache) = self.cache.read().unwrap().as_ref() {
            if self.is_fresh(cache) {
                return Ok(cache.clone());
            }
        }

        match self.rebuild() {
            Ok(cache) => {
                let cache = Arc::new(cache);
                *self.cache.write().unwrap() = Some(cache.clone());
                Ok(cache)
            }
            Err(e) => {
                // Keep serving the last-good cache if we have one; otherwise the
                // caller must surface the failure (nothing to serve yet).
                eprintln!("[transparency] rebuild failed, serving last-good cache: {e}");
                match self.cache.read().unwrap().as_ref() {
                    Some(cache) => Ok(cache.clone()),
                    None => Err(e),
                }
            }
        }
    }

    /// One read-through rebuild: pull `mls_commit_log`, build the signed bundle,
    /// regenerate the `/v1` artifact map. Counts exactly one DB pull.
    fn rebuild(&self) -> Result<Cache> {
        self.rebuild_count.fetch_add(1, Ordering::SeqCst);

        // The libSQL read is async; drive it on the shared runtime. Single-flight
        // means at most one of these runs at a time.
        let rows = self.runtime.block_on(async {
            let conn = source::connect(&self.db).await?;
            source::read_commit_log(&conn).await
        })?;

        // The serve runtime is allowed the clock; the builder stays deterministic
        // by taking the timestamp as an argument.
        let bundle = build_bundle(&rows, &self.signing_key, current_unix_ms())?;

        // The builder and serve crates carry byte-identical bundle shapes; round
        // -trip through JSON rather than depend on the builder's concrete type.
        let bundle: Bundle = serde_json::from_slice(&serde_json::to_vec(&bundle)?)?;

        let (_manifest, artifacts) = layout::generate_artifacts(&bundle)?;

        Ok(Cache {
            bundle,
            artifacts,
            built_at: Instant::now(),
        })
    }
}

/// A running live server bound to a `127.0.0.1` port, served from a pool of
/// background threads. Dropping it (or calling [`LiveServer::shutdown`]) stops
/// the pool.
pub struct LiveServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl LiveServer {
    /// Bind `127.0.0.1:<port>` (pass `0` for an ephemeral port) and start serving
    /// a live view of `db`, refreshed at most once per `ttl`. The server starts
    /// **cold** — the first request triggers the first DB pull, so an idle server
    /// imposes no DB load.
    ///
    /// `db` is a libSQL/Turso URL (auth via `TURSO_AUTH_TOKEN`) or a local SQLite
    /// path. The caller must supply the signing key — the CLI loads it from env
    /// or file and refuses to start without one.
    pub fn spawn(
        db: String,
        port: u16,
        ttl: Duration,
        signing_key: SigningKey,
    ) -> Result<LiveServer> {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| ServeError::Config(format!("failed to start async runtime: {e}")))?;

        let server = Server::http(("127.0.0.1", port))
            .map_err(|e| ServeError::Http(format!("bind failed: {e}")))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| ServeError::Http("server bound to a non-IP address".into()))?;

        let shared = Arc::new(Shared {
            db,
            signing_key,
            ttl,
            runtime,
            cache: RwLock::new(None),
            rebuild_lock: Mutex::new(()),
            rebuild_count: AtomicU64::new(0),
        });

        let server = Arc::new(server);
        let stop = Arc::new(AtomicBool::new(false));

        let handles = (0..WORKERS)
            .map(|_| {
                let server = server.clone();
                let shared = shared.clone();
                let stop = stop.clone();
                std::thread::spawn(move || serve_loop(&server, &shared, &stop))
            })
            .collect();

        Ok(LiveServer {
            addr,
            stop,
            handles,
            shared,
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

    /// How many DB pulls (rebuilds) have happened so far. Exposed so tests can
    /// assert single-flight and TTL behaviour.
    pub fn rebuild_count(&self) -> u64 {
        self.shared.rebuild_count.load(Ordering::SeqCst)
    }

    /// Block the calling thread, keeping the server alive until the process is
    /// killed. Used by the CLI `serve live` command.
    pub fn block_forever(self) {
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

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

/// Accept requests until `stop` is set, polling so shutdown is prompt.
fn serve_loop(server: &Server, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_request(request, shared),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

/// Route one request: CORS preflight, the dynamic verify endpoint, or a static
/// `/v1` artifact — each first ensuring the in-memory cache is fresh.
fn handle_request(request: Request, shared: &Shared) {
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
        handle_verify_group(request, shared, encoded);
        return;
    }

    // Static read API: GET/HEAD only.
    if method != Method::Get && method != Method::Head {
        respond_status(request, 405, "method not allowed");
        return;
    }

    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        respond_status(request, 404, "not found");
        return;
    }

    // Ensure the cache is fresh, then serve the in-memory artifact bytes.
    let cache = match shared.ensure_fresh() {
        Ok(cache) => cache,
        Err(e) => {
            respond_status(request, 503, &format!("log unavailable: {e}"));
            return;
        }
    };

    let bytes = match cache.artifacts.get(rel) {
        Some(b) => b.clone(),
        None => {
            respond_status(request, 404, "not found");
            return;
        }
    };

    let cache_control = cache_control_for(&path);
    let response = Response::from_data(bytes)
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", cache_control));
    let _ = request.respond(response);
}

/// Ensure freshness, then verify the requested group against the in-memory
/// bundle directly — the same [`verify_group_in_bundle`] the CLI path ends in.
fn handle_verify_group(request: Request, shared: &Shared, encoded_id: &str) {
    let id = percent_decode(encoded_id);
    if id.is_empty() {
        respond_json(request, 400, r#"{"error":"missing group id"}"#.to_string());
        return;
    }

    let cache = match shared.ensure_fresh() {
        Ok(cache) => cache,
        Err(e) => {
            respond_json(
                request,
                503,
                format!("{{\"error\":\"log unavailable: {}\"}}", escape(&e.to_string())),
            );
            return;
        }
    };

    let report = verify_group_in_bundle(&cache.bundle, &id);
    match serde_json::to_string(&report) {
        Ok(body) => respond_json(request, 200, body),
        Err(e) => respond_json(
            request,
            500,
            format!("{{\"error\":\"failed to serialize report: {}\"}}", escape(&e.to_string())),
        ),
    }
}

/// Current time in milliseconds since the Unix epoch (for the STH timestamp).
/// A pre-epoch clock — never expected — clamps to 0 rather than panicking.
fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
