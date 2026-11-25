//! An in-process stand-in for R2's S3 API (#2109), so attachment uploads and
//! downloads run their real path — DS presign, PUT, register, GET — in the
//! flows world instead of being stubbed.
//!
//! It honours what the shared-object presign relies on: `If-None-Match: *`
//! makes a PUT create-only (412 when the key exists) and
//! `x-amz-checksum-sha256` must match the body (400 otherwise), as R2 does.
//! Signatures are not checked: the DS that signs them is the code under test,
//! and its SigV4 is pinned by `pollis-delivery`'s own tests.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use axum::body::Bytes;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};

pub(crate) const BUCKET: &str = "flows";

fn objects() -> &'static Mutex<HashMap<String, Vec<u8>>> {
    static OBJECTS: OnceLock<Mutex<HashMap<String, Vec<u8>>>> = OnceLock::new();
    OBJECTS.get_or_init(Mutex::default)
}

/// Every key the bucket holds, sorted.
pub(crate) fn keys() -> Vec<String> {
    let mut keys: Vec<String> = objects().lock().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

/// The bytes stored at `key`.
pub(crate) fn get(key: &str) -> Option<Vec<u8>> {
    objects().lock().unwrap().get(key).cloned()
}

/// Delete `key`, as the DS's own bucket handle does.
pub(crate) fn delete(key: &str) {
    objects().lock().unwrap().remove(key);
}

/// Empty the bucket (each test's world starts clean).
pub(crate) fn clear() {
    objects().lock().unwrap().clear();
}

async fn handle(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let path = uri.path().trim_start_matches('/');
    let Some(key) = path.strip_prefix(&format!("{BUCKET}/")) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let key = key.to_string();
    let mut objects = objects().lock().unwrap();
    match method {
        Method::PUT => {
            let create_only = headers
                .get("if-none-match")
                .is_some_and(|v| v.as_bytes() == b"*");
            if create_only && objects.contains_key(&key) {
                return StatusCode::PRECONDITION_FAILED.into_response();
            }
            if let Some(want) = headers.get("x-amz-checksum-sha256") {
                use base64::Engine as _;
                use sha2::Digest as _;
                let got = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&body));
                if want.as_bytes() != got.as_bytes() {
                    return (StatusCode::BAD_REQUEST, "BadDigest").into_response();
                }
            }
            objects.insert(key, body.to_vec());
            StatusCode::OK.into_response()
        }
        Method::GET => match objects.get(&key) {
            Some(bytes) => (StatusCode::OK, bytes.clone()).into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        },
        Method::HEAD => {
            if objects.contains_key(&key) {
                StatusCode::OK.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        Method::DELETE => {
            objects.remove(&key);
            StatusCode::NO_CONTENT.into_response()
        }
        _ => StatusCode::METHOD_NOT_ALLOWED.into_response(),
    }
}

/// Serve the bucket on a loopback port; returns its endpoint URL.
pub(crate) fn spawn() -> String {
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("fake-r2".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("fake r2: runtime");
            rt.block_on(async move {
                // R2 takes objects far over axum's 2 MiB default body limit.
                let router = axum::Router::new()
                    .fallback(handle)
                    .layer(axum::extract::DefaultBodyLimit::disable());
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("fake r2: bind");
                let addr = listener.local_addr().expect("fake r2: addr");
                tx.send(format!("http://{addr}")).expect("fake r2: send url");
                let _ = axum::serve(listener, router).await;
            });
        })
        .expect("fake r2: thread");
    rx.recv().expect("fake r2: url")
}
