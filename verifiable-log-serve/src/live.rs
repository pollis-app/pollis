//! A **live, lazily-refreshed** HTTP server over the verifiable log, reading the
//! commit log directly from Turso/libSQL instead of a pre-generated directory.
//!
//! Serves the `/v1` surface from memory, rebuilt single-flight at most once per TTL; a failed
//! rebuild keeps the last-good cache.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use verifiable_log::SigningKey;
// `verifying_key()` on a signing key comes from this trait (pin seam).
use ml_dsa::Keypair as _;
use tiny_http::{Method, Request, Response, Server};
use verifiable_log_builder::{build_bundle, source};

use crate::bundle::{now_ms, Bundle};
use crate::error::{Result, ServeError};
use crate::group::verify_group_in_bundle;
use crate::layout;
use crate::server::{
    cache_control_for, cors_preflight, escape, header, percent_decode, respond_json,
    respond_status, VERIFY_GROUP_PREFIX,
};

/// Number of request worker threads. Unlike the static server, the live verify
/// endpoint never calls back into itself over HTTP, so one worker could not deadlock.
const WORKERS: usize = 4;

/// Immutable once built; a rebuild swaps in a fresh `Arc`.
struct Cache {
    /// The signed bundle, used to answer `/verify/group/<id>` directly.
    bundle: Bundle,
    /// `relative path -> JSON bytes` for the `/v1` read API.
    artifacts: BTreeMap<String, Vec<u8>>,
    /// When this cache was built; the TTL is measured from here.
    built_at: Instant,
}

/// State shared by every worker thread: the DB source, the signing key, the TTL,
/// a tokio runtime for the async DB read, and the single-flight cache.
struct Shared {
    db: String,
    /// Auth token for the (log) DB; ignored for a local SQLite path.
    token: String,
    signing_key: SigningKey,
    ttl: Duration,
    runtime: tokio::runtime::Runtime,
    /// Last-good cache, `None` until the first rebuild (the server starts cold).
    cache: RwLock<Option<Arc<Cache>>>,
    /// Single-flight guard: only the holder may rebuild.
    rebuild_lock: Mutex<()>,
    /// Count of DB pulls, for tests.
    rebuild_count: AtomicU64,
}

impl Shared {
    /// Is the cache fresh enough to serve without rebuilding? A zero TTL means
    /// "always rebuild", so it is never fresh.
    fn is_fresh(&self, cache: &Cache) -> bool {
        !self.ttl.is_zero() && cache.built_at.elapsed() < self.ttl
    }

    /// Return a cache guaranteed fresh per the TTL, rebuilding once under the
    /// single-flight lock if stale. Only a failure with no last-good cache is `Err`.
    fn ensure_fresh(&self) -> Result<Arc<Cache>> {
        // Fast path: a fresh cache needs no lock and no DB hit.
        if let Some(cache) = self.cache.read().unwrap().as_ref() {
            if self.is_fresh(cache) {
                return Ok(cache.clone());
            }
        }

        // Stale or cold: serialise rebuilds.
        let _flight = self.rebuild_lock.lock().unwrap();

        // A concurrent holder may have just rebuilt.
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

        let rows = self.runtime.block_on(async {
            let conn = source::connect_with_token(&self.db, &self.token).await?;
            source::read_commit_log(&conn).await
        })?;

        let bundle = build_bundle(&rows, &self.signing_key, now_ms())?;

        // Round-trip through JSON rather than depend on the builder's concrete type.
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
    /// a live view of `db`, refreshed at most once per `ttl`. Starts cold.
    pub fn spawn(
        db: String,
        token: String,
        port: u16,
        ttl: Duration,
        signing_key: SigningKey,
    ) -> Result<LiveServer> {
        // Trust this server's own key so local runs and tests verify without the production pin.
        crate::pinned::trust_hex_for_dev(&hex::encode(signing_key.verifying_key().encode()));

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| ServeError::Config(format!("failed to start async runtime: {e}")))?;

        // Loopback by default; `VLOG_BIND_HOST=0.0.0.0` for a container behind a proxy.
        let host = std::env::var("VLOG_BIND_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let server = Server::http((host.as_str(), port))
            .map_err(|e| ServeError::Http(format!("bind failed: {e}")))?;
        let addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| ServeError::Http("server bound to a non-IP address".into()))?;

        let shared = Arc::new(Shared {
            db,
            token,
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

    /// How many DB pulls (rebuilds) have happened so far.
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
        handle_verify_group(request, shared, encoded);
        return;
    }

    if method != Method::Get && method != Method::Head {
        respond_status(request, 405, "method not allowed");
        return;
    }

    let rel = path.trim_start_matches('/');
    if rel.is_empty() {
        respond_status(request, 404, "not found");
        return;
    }

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

