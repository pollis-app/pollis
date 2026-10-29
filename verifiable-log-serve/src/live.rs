//! A live HTTP server that reads the commit log from Turso/libSQL and holds the
//! same `/v1` surface as the static mode in memory, plus `/verify/group/<id>`.
//!
//! Rebuilds are lazy and single-flight: at most one DB pull per TTL regardless
//! of request volume, none while idle. A failed rebuild keeps serving the
//! last-good cache. Only this runtime reads the clock; the log core and builder
//! stay clock-free.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use verifiable_log::SigningKey;
// Brings `verifying_key()` onto the signing key.
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

const WORKERS: usize = 4;

/// Immutable once built; a rebuild swaps in a fresh `Arc`.
struct Cache {
    bundle: Bundle,
    artifacts: BTreeMap<String, Vec<u8>>,
    built_at: Instant,
}

struct Shared {
    db: String,
    /// Empty for a local SQLite path, which ignores it.
    token: String,
    signing_key: SigningKey,
    ttl: Duration,
    runtime: tokio::runtime::Runtime,
    /// Last-good cache; `None` until the first successful rebuild.
    cache: RwLock<Option<Arc<Cache>>>,
    /// Single-flight guard: only the holder may rebuild.
    rebuild_lock: Mutex<()>,
    rebuild_count: AtomicU64,
}

impl Shared {
    /// A zero TTL means "always rebuild".
    fn is_fresh(&self, cache: &Cache) -> bool {
        !self.ttl.is_zero() && cache.built_at.elapsed() < self.ttl
    }

    /// A cache fresh per the TTL, rebuilt once under the single-flight lock if
    /// stale. `Err` only when a rebuild fails with no last-good cache.
    fn ensure_fresh(&self) -> Result<Arc<Cache>> {
        if let Some(cache) = self.cache.read().unwrap().as_ref() {
            if self.is_fresh(cache) {
                return Ok(cache.clone());
            }
        }

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
                eprintln!("[transparency] rebuild failed, serving last-good cache: {e}");
                match self.cache.read().unwrap().as_ref() {
                    Some(cache) => Ok(cache.clone()),
                    None => Err(e),
                }
            }
        }
    }

    /// Pull `mls_commit_log`, build the signed bundle, regenerate the artifacts.
    fn rebuild(&self) -> Result<Cache> {
        self.rebuild_count.fetch_add(1, Ordering::SeqCst);

        let rows = self.runtime.block_on(async {
            let conn = source::connect_with_token(&self.db, &self.token).await?;
            source::read_commit_log(&conn).await
        })?;

        let bundle = build_bundle(&rows, &self.signing_key, now_ms())?;

        // The two crates' bundle shapes are byte-identical; round-trip via JSON.
        let bundle: Bundle = serde_json::from_slice(&serde_json::to_vec(&bundle)?)?;

        let (_manifest, artifacts) = layout::generate_artifacts(&bundle)?;

        Ok(Cache {
            bundle,
            artifacts,
            built_at: Instant::now(),
        })
    }
}

/// A running live server on a pool of background threads; dropping it stops
/// the pool.
pub struct LiveServer {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    shared: Arc<Shared>,
}

impl LiveServer {
    /// Bind `127.0.0.1:<port>` (`0` = ephemeral) and serve a live view of `db` (a
    /// libSQL URL or local SQLite path), refreshed at most once per `ttl`. Starts
    /// cold: the first request triggers the first DB pull.
    pub fn spawn(
        db: String,
        token: String,
        port: u16,
        ttl: Duration,
        signing_key: SigningKey,
    ) -> Result<LiveServer> {
        // Trust our own key so local verification works without the production
        // pin; see `pinned::DEV_EXTRA` for why this is safe.
        crate::pinned::trust_hex_for_dev(&hex::encode(signing_key.verifying_key().encode()));

        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| ServeError::Config(format!("failed to start async runtime: {e}")))?;

        // Loopback by default; `VLOG_BIND_HOST=0.0.0.0` inside a container.
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

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn base_url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// DB pulls so far, for tests asserting single-flight and TTL behaviour.
    pub fn rebuild_count(&self) -> u64 {
        self.shared.rebuild_count.load(Ordering::SeqCst)
    }

    /// Block until the process is killed.
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

impl Drop for LiveServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

fn serve_loop(server: &Server, shared: &Shared, stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(request)) => handle_request(request, shared),
            Ok(None) => continue,
            Err(_) => break,
        }
    }
}

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

