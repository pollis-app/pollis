//! A recording reverse proxy: the client is configured to reach the DS (and,
//! on the local target, object storage) THROUGH it, so every request and
//! response body crosses a point where the soak can inspect it — the
//! "in transit" proof. Bodies are scanned off the request path.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderName, Request, Response, StatusCode};

use crate::proofs::{BlobVerdict, Scanner};

#[derive(Clone, Debug)]
pub struct Exchange {
    pub seq: usize,
    pub service: &'static str,
    pub method: String,
    /// Path only — a presigned query string carries a signature, and the
    /// report must not.
    pub path: String,
    pub status: u16,
    /// Upstream round trip, request sent to response body read.
    pub millis: u128,
    pub request: BlobVerdict,
    pub response: BlobVerdict,
    /// Kept for small (DS) bodies, so the final rescan covers secrets that
    /// only became known after the request went out.
    pub request_body: Option<Vec<u8>>,
    pub response_body: Option<Vec<u8>>,
}

pub struct Tap {
    pub service: &'static str,
    upstream: String,
    preserve_host: bool,
    keep_bodies: bool,
    scanner: RwLock<Arc<Scanner>>,
    exchanges: Mutex<Vec<Exchange>>,
    pending: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    seq: std::sync::atomic::AtomicUsize,
    client: soak_reqwest::Client,
}

impl Tap {
    pub fn new(service: &'static str, upstream: &str, preserve_host: bool, keep_bodies: bool, scanner: Arc<Scanner>) -> Arc<Self> {
        Arc::new(Self {
            service,
            upstream: upstream.trim_end_matches('/').to_string(),
            preserve_host,
            keep_bodies,
            scanner: RwLock::new(scanner),
            exchanges: Mutex::new(Vec::new()),
            pending: Mutex::new(Vec::new()),
            seq: Default::default(),
            // No pooled connections on either leg: hyper's server drops a
            // keep-alive connection that idles past its header-read timeout,
            // and a client that reuses one in that window fails a request the
            // soak would then blame on the product. `Connection: close` on
            // every response (below) does the same for the client's side.
            client: soak_reqwest::Client::builder()
                .redirect(soak_reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(0)
                .build()
                .expect("tap client"),
        })
    }

    pub fn set_scanner(&self, scanner: Arc<Scanner>) {
        *self.scanner.write().unwrap() = scanner;
    }

    /// Bind on loopback and serve; returns `http://127.0.0.1:<port>`.
    pub async fn serve(self: &Arc<Self>) -> String {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("tap bind");
        let addr: SocketAddr = listener.local_addr().expect("tap addr");
        let app = axum::Router::new().fallback(proxy).with_state(self.clone());
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{addr}")
    }

    /// Wait for every in-flight body analysis.
    pub async fn settle(&self) {
        let handles: Vec<_> = std::mem::take(&mut *self.pending.lock().unwrap());
        for h in handles {
            let _ = h.await;
        }
    }

    pub fn mark(&self) -> usize {
        self.seq.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn exchanges(&self) -> Vec<Exchange> {
        let mut v = self.exchanges.lock().unwrap().clone();
        v.sort_by_key(|e| e.seq);
        v
    }

    /// Rescan every retained body with `scanner` (which may know secrets the
    /// one in force at request time did not). Returns (exchanges rescanned,
    /// findings).
    pub fn rescan(&self, scanner: &Scanner) -> (usize, Vec<String>) {
        let ex = self.exchanges.lock().unwrap();
        let mut findings = Vec::new();
        let mut n = 0;
        for e in ex.iter() {
            for (dir, body) in [("request", &e.request_body), ("response", &e.response_body)] {
                if let Some(b) = body {
                    n += 1;
                    let hits = scanner.scan(b);
                    if !hits.is_empty() {
                        findings.push(format!(
                            "{} {} {} {dir}: {}",
                            e.service,
                            e.method,
                            e.path,
                            scanner.describe(&hits).join("; ")
                        ));
                    }
                }
            }
        }
        (n, findings)
    }
}

const HOP: [&str; 6] = ["connection", "keep-alive", "transfer-encoding", "upgrade", "proxy-connection", "content-length"];

async fn proxy(State(tap): State<Arc<Tap>>, req: Request<Body>) -> Response<Body> {
    let seq = tap.seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    let (parts, body) = req.into_parts();
    let req_bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_REQUEST, format!("tap: read body: {e}")),
    };
    let pq = parts.uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| "/".to_string());
    let url = format!("{}{}", tap.upstream, pq);
    let method = soak_reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).expect("method");
    let mut rb = tap.client.request(method, &url);
    for (k, v) in parts.headers.iter() {
        let name = k.as_str();
        if HOP.contains(&name) || (name == "host" && !tap.preserve_host) {
            continue;
        }
        rb = rb.header(name, v.as_bytes());
    }
    let started = std::time::Instant::now();
    let upstream = rb.body(req_bytes.clone()).send().await;
    let resp = match upstream {
        Ok(r) => r,
        Err(e) => {
            let mut chain = String::new();
            let mut src = std::error::Error::source(&e);
            while let Some(s) = src {
                chain.push_str(&format!(": {s}"));
                src = s.source();
            }
            let why = format!("{}{chain}", e.without_url());
            return error(StatusCode::BAD_GATEWAY, format!("tap: upstream {} {}: {why}", parts.method, parts.uri.path()));
        }
    };
    let status = resp.status().as_u16();
    let headers: Vec<(String, Vec<u8>)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.as_bytes().to_vec()))
        .collect();
    let resp_bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => return error(StatusCode::BAD_GATEWAY, format!("tap: upstream body {}: {}", parts.uri.path(), e.without_url())),
    };

    let millis = started.elapsed().as_millis();
    // Analyse off the request path; `settle` joins these.
    let scanner = tap.scanner.read().unwrap().clone();
    let (rq, rs) = (req_bytes.clone(), resp_bytes.clone());
    let path = parts.uri.path().to_string();
    let method_s = parts.method.as_str().to_string();
    let keep = tap.keep_bodies;
    let tap2 = tap.clone();
    let handle = tokio::task::spawn_blocking(move || {
        let ex = Exchange {
            seq,
            service: tap2.service,
            method: method_s,
            path,
            status,
            millis,
            request: BlobVerdict::of(&scanner, &rq),
            response: BlobVerdict::of(&scanner, &rs),
            request_body: keep.then(|| rq.to_vec()),
            response_body: keep.then(|| rs.to_vec()),
        };
        tap2.exchanges.lock().unwrap().push(ex);
    });
    tap.pending.lock().unwrap().push(handle);

    let mut out = Response::builder().status(status).header("connection", "close");
    for (k, v) in headers {
        if HOP.contains(&k.as_str()) {
            continue;
        }
        if let Ok(name) = HeaderName::from_bytes(k.as_bytes()) {
            out = out.header(name, v);
        }
    }
    out.body(Body::from(resp_bytes)).expect("tap response")
}

fn error(status: StatusCode, msg: String) -> Response<Body> {
    eprintln!("[vault-soak] {msg}");
    Response::builder().status(status).header("connection", "close").body(Body::from(msg)).expect("tap error")
}
