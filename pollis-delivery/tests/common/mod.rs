//! Shared fixture plumbing for the `pollis-delivery` integration tests.
//!
//! `db` is declared before `_dir` so the database closes before the directory goes.

#![allow(dead_code)]

pub mod fake_livekit;
pub mod mls;

use pollis_delivery::db::Db;
use std::ops::Deref;
use std::sync::Arc;
use tempfile::TempDir;

/// A libsql database in a temp directory removed on drop.
pub struct TempDb {
    db: Arc<Db>,
    _dir: TempDir,
}

impl TempDb {
    /// Open an empty database at `<tempdir>/<file>`; no schema is applied.
    pub async fn open(file: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join(file);
        let db = Db::connect_local(path.to_str().expect("utf-8 path"))
            .await
            .expect("local db");
        Self {
            db: Arc::new(db),
            _dir: dir,
        }
    }

    pub fn arc(&self) -> Arc<Db> {
        Arc::clone(&self.db)
    }
}

/// A fresh ML-DSA-44 device signing key.
pub fn gen_signing_key() -> ml_dsa::SigningKey<ml_dsa::MlDsa44> {
    use rand_core::RngCore as _;
    let mut seed = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut seed);
    ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&seed.into())
}

/// POST `body` to `path` through the router, device-signed as `user_id`/`device_id`.
pub async fn send_signed(
    state: &pollis_delivery::AppState,
    path: &str,
    body: serde_json::Value,
    user_id: &str,
    device_id: &str,
    sk: &ml_dsa::SigningKey<ml_dsa::MlDsa44>,
) -> (axum::http::StatusCode, serde_json::Value) {
    use base64::Engine as _;
    use http_body_util::BodyExt as _;
    use ml_dsa::Signer as _;
    use tower::ServiceExt as _;

    let bytes = serde_json::to_vec(&body).unwrap();
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let msg = pollis_delivery::auth::canonical_message("POST", path, ts, &bytes);
    let sig = base64::engine::general_purpose::STANDARD.encode(sk.sign(&msg).encode());
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", user_id)
        .header("X-Pollis-Device", device_id)
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(axum::body::Body::from(bytes))
        .unwrap();
    let resp = pollis_delivery::build_router_with_state(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let val = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, val)
}

impl Deref for TempDb {
    type Target = Arc<Db>;

    fn deref(&self) -> &Arc<Db> {
        &self.db
    }
}
