//! In-box, Tauri-free test rig for the pollis-tui data/sync core.
//!
//! This is the flows harness (`src-tauri/tests/flows/harness.rs`) distilled to
//! exactly what a two-client "A sends, B receives" scenario needs, and with the
//! Tauri layer removed: clients call `pollis_core::commands::*` **directly** (the
//! way the TUI does), not through a `MockRuntime` webview + `invoke`.
//!
//! Shared world, mirroring production `pollis_delivery::AppState { db, log_db }`:
//! - ONE writable main `RemoteDb` (users / groups / DMs / envelopes / auth) and
//!   ONE `RemoteDb` for the commit log (`mls_commit_log` / `mls_welcome` /
//!   `mls_group_info`) — two genuinely separate libsql files, so a misrouted
//!   query fails loudly (the #420 split).
//! - ONE in-process `pollis-delivery` axum server, bound on loopback, that both
//!   clients' writes route through (their own `remote_db` handle is a read-only
//!   `query_only_view`, so a stray direct write fails instead of silently
//!   passing — the definitive "everything went through the DS" gate).
//!
//! Each `TestClient` gets its OWN `AppState` + `InMemoryKeystore` + read-only
//! main view, exactly like the flows `TestClient`.
//!
//! DS routes wired here are ONLY the ones the DM message path exercises — see the
//! router in [`spawn_in_process_delivery`]. The handlers are copied
//! verbatim-in-pattern from the flows harness (same `pollis_delivery::*::apply_*`
//! calls); the harness-only fault-injection menu is dropped (not needed here).

#![allow(dead_code)]

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use pollis_core::accounts;
use pollis_core::commands::{auth, dm, messages, pin};
use pollis_core::config::Config;
use pollis_core::db::remote::RemoteDb;
use pollis_core::db::{
    BASELINE_SQL, LOG_DB_SCHEMA, POST_BASELINE_LOG_MIGRATIONS, POST_BASELINE_MIGRATIONS,
};
use pollis_core::keystore::{default_os_keystore, InMemoryKeystore, Keystore};
use pollis_core::state::AppState;

/// DEV_OTP short-circuits the email send in the DS and fixes the OTP so
/// verify-otp accepts a known code with no real email.
pub const DEV_OTP: &str = "000000";
/// Fixed PIN so every client's local SQLCipher DB is open after signup.
pub const TEST_PIN: &str = "0000";

/// The three MLS control-plane tables that live ONLY on the commit-log DB.
const LOG_TABLES: [&str; 3] = ["mls_commit_log", "mls_welcome", "mls_group_info"];

// ─── Schema bootstrap (public `pollis_core::db` constants — no src-tauri dep) ──

async fn bootstrap_schema(remote: &RemoteDb) -> anyhow::Result<()> {
    let conn = remote.conn().await?;
    // `users` is created by the baseline and never dropped — marker for "applied".
    let has_baseline = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type='table' AND name='users'",
            (),
        )
        .await?
        .next()
        .await?
        .is_some();
    if !has_baseline {
        run_sql_script(&conn, BASELINE_SQL).await?;
    }
    conn.execute(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY,
             description TEXT NOT NULL,
             applied_at TEXT NOT NULL DEFAULT (datetime('now'))
         )",
        (),
    )
    .await?;
    conn.execute(
        "INSERT OR IGNORE INTO schema_migrations (version, description) VALUES (0, 'baseline')",
        (),
    )
    .await?;
    for (version, description, sql) in POST_BASELINE_MIGRATIONS {
        let already = conn
            .query(
                "SELECT 1 FROM schema_migrations WHERE version = ?1",
                [libsql::Value::Integer(i64::from(*version))],
            )
            .await?
            .next()
            .await?
            .is_some();
        if already {
            continue;
        }
        run_sql_script(&conn, sql).await?;
        conn.execute(
            "INSERT INTO schema_migrations (version, description) VALUES (?1, ?2)",
            (
                libsql::Value::Integer(i64::from(*version)),
                libsql::Value::Text(description.to_string()),
            ),
        )
        .await?;
    }
    Ok(())
}

async fn bootstrap_log_schema(log: &RemoteDb) -> anyhow::Result<()> {
    let conn = log.conn().await?;
    run_sql_script(&conn, LOG_DB_SCHEMA).await?;
    // Post-baseline log-DB migrations (mirrors the flows harness + db-apply.sh's
    // second apply). Without these the log DB is missing e.g. migration 000002's
    // `mls_welcome` UNIQUE index, so the DS's idempotent welcome upsert
    // (`ON CONFLICT`) errors and the recipient never gets welcomed (#487). The
    // log DB is fresh per test and each migration is idempotent, so apply
    // unconditionally.
    for (_version, _description, sql) in POST_BASELINE_LOG_MIGRATIONS {
        run_sql_script(&conn, sql).await?;
    }
    Ok(())
}

/// Drop the three MLS control-plane tables from MAIN so a misrouted main-side
/// read of them fails loudly (they live only on the log DB now).
async fn drop_log_tables_from_main(remote: &RemoteDb) -> anyhow::Result<()> {
    let conn = remote.conn().await?;
    for t in LOG_TABLES {
        conn.execute(&format!("DROP TABLE IF EXISTS {t}"), ()).await?;
    }
    Ok(())
}

async fn run_sql_script(conn: &libsql::Connection, sql: &str) -> anyhow::Result<()> {
    for stmt in split_sql_statements(sql) {
        conn.execute(&stmt, ())
            .await
            .map_err(|e| anyhow::anyhow!("stmt failed: {stmt}\n→ {e}"))?;
    }
    Ok(())
}

/// Split on `;`, honoring `--` line comments and `'...'` string literals (both
/// appear in the migrations). Copied from the flows harness.
fn split_sql_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut chars = sql.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '-' if chars.peek() == Some(&'-') => {
                chars.next();
                for next in chars.by_ref() {
                    if next == '\n' {
                        current.push('\n');
                        break;
                    }
                }
            }
            '\'' => {
                current.push(c);
                while let Some(inner) = chars.next() {
                    current.push(inner);
                    if inner == '\'' {
                        if chars.peek() == Some(&'\'') {
                            current.push(chars.next().unwrap());
                        } else {
                            break;
                        }
                    }
                }
            }
            ';' => {
                let stmt = current.trim().to_string();
                if !stmt.is_empty() {
                    statements.push(stmt);
                }
                current.clear();
            }
            _ => current.push(c),
        }
    }
    let tail = current.trim();
    if !tail.is_empty() {
        statements.push(tail.to_string());
    }
    statements
}

// ─── Shared world ─────────────────────────────────────────────────────────────

/// The shared backend for one test: two libsql files (main + log), the config
/// pointing at the in-process DS, and the temp dir backing per-user SQLCipher
/// DBs. Every client shares `main`/`log`; each opens its own read-only view.
pub struct World {
    pub main: Arc<RemoteDb>,
    pub log: Arc<RemoteDb>,
    pub config: Config,
    // Kept alive so the temp dir (per-user DBs + libsql files) survives the test.
    _tmp: std::path::PathBuf,
}

impl World {
    /// Carve out a per-device `POLLIS_DATA_DIR` under the world's temp dir. Two
    /// devices of the SAME user MUST NOT share a data dir — their local SQLCipher
    /// DB (`pollis_{user_id}.db`), file keystore, and `accounts.json` all key off
    /// `POLLIS_DATA_DIR`, so a shared dir would have the second device clobber the
    /// first. Used by the multi-device enrollment + recovery smokes.
    pub fn device_dir(&self, name: &str) -> std::path::PathBuf {
        let dir = self._tmp.join(name);
        std::fs::create_dir_all(&dir).expect("create per-device data dir");
        dir
    }
}

/// Stand up the whole world: temp dir, two libsql files + schema, and the
/// in-process Delivery Service. Call once per test.
pub async fn spawn_world() -> World {
    let tmp = tempfile::tempdir().expect("tempdir").keep();
    // The file keystore is bypassed (InMemoryKeystore per client), but the local
    // SQLCipher DB path derives from POLLIS_DATA_DIR — keyed per user, so all
    // clients can share one dir.
    std::env::set_var("POLLIS_DATA_DIR", &tmp);
    std::env::set_var("DEV_OTP", DEV_OTP);

    let main = Arc::new(
        RemoteDb::connect_local(tmp.join("test_turso.db"))
            .await
            .expect("connect main libsql"),
    );
    bootstrap_schema(&main).await.expect("bootstrap main schema");

    let log = Arc::new(
        RemoteDb::connect_local(tmp.join("test_log.db"))
            .await
            .expect("connect log libsql"),
    );
    bootstrap_log_schema(&log).await.expect("bootstrap log schema");
    drop_log_tables_from_main(&main)
        .await
        .expect("drop log tables from main");

    let delivery_url = spawn_in_process_delivery(main.clone(), log.clone()).await;

    // Config literal: turso/R2/livekit fields are placeholders (clients use the
    // explicit `RemoteDb` handles, not these URLs); only the DS URL is real.
    let config = Config {
        turso_url: "libsql://placeholder.invalid".to_string(),
        turso_token: "placeholder".to_string(),
        log_db_url: None,
        log_db_token: None,
        r2_endpoint: String::new(),
        r2_access_key_id: String::new(),
        r2_secret_access_key: String::new(),
        r2_region: "auto".to_string(),
        r2_public_url: String::new(),
        livekit_url: String::new(),
        livekit_api_key: String::new(),
        livekit_api_secret: String::new(),
        pollis_delivery_url: Some(delivery_url),
        // Sealed Sender off: the smoke rig exercises sync/enroll flows, not
        // envelope blinding (mirrors the flows harness default).
        seal_sender: false,
    };

    World {
        main,
        log,
        config,
        _tmp: tmp,
    }
}

// ─── In-process Delivery Service ──────────────────────────────────────────────

/// Two-handle DS state, mirroring production `pollis_delivery::AppState`.
#[derive(Clone)]
struct DsState {
    main: Arc<RemoteDb>,
    log: Arc<RemoteDb>,
    otp: pollis_delivery::otp::OtpStore,
    sessions: pollis_delivery::session::SessionStore,
    otp_config: pollis_delivery::otp::OtpConfig,
}

fn ds_ok() -> Response {
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response()
}

fn ds_bad_request() -> Response {
    (StatusCode::BAD_REQUEST, "invalid body").into_response()
}

fn ds_internal_error(msg: String) -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
}

fn ds_conflict(msg: &str) -> Response {
    (
        StatusCode::CONFLICT,
        axum::Json(serde_json::json!({ "status": "conflict", "error": msg })),
    )
        .into_response()
}

fn ds_outcome(outcome: pollis_delivery::writes::WriteOutcome) -> Response {
    match outcome {
        pollis_delivery::writes::WriteOutcome::Ok => ds_ok(),
        pollis_delivery::writes::WriteOutcome::Forbidden => {
            pollis_delivery::error::AuthRejection::Forbidden.into_response()
        }
    }
}

fn b64d(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn now_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Verify the device signature over the raw body against the MAIN DB's
/// `user_device` rows; return the authenticated user or a rejection response.
async fn ds_auth(
    remote: &RemoteDb,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &axum::body::Bytes,
) -> Result<String, Response> {
    let conn = remote
        .conn()
        .await
        .map_err(|e| ds_internal_error(format!("conn: {e}")))?;
    pollis_delivery::auth::verify_request(
        &conn,
        headers,
        method.as_str(),
        uri.path(),
        body,
        pollis_delivery::auth::now_unix(),
    )
    .await
    .map_err(|rej| rej.into_response())
}

// ── /v1/commits — the MLS add/update/remove commit + GroupInfo + Welcomes ──────
async fn delivery_submit(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    use pollis_delivery::commit::{SubmitBody, SubmitResponse};
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    if parsed.sender_id != authed {
        return pollis_delivery::error::AuthRejection::Forbidden.into_response();
    }
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::commit::submit_commit(&conn, &parsed).await {
        Ok(outcome) => {
            let code = match &outcome {
                SubmitResponse::Accepted { .. } => StatusCode::OK,
                SubmitResponse::Rejected { .. } => StatusCode::CONFLICT,
            };
            (code, axum::Json(outcome)).into_response()
        }
        Err(e) => ds_internal_error(format!("submit: {e}")),
    }
}

// ── /v1/group-info — republish GroupInfo (member-gated) ───────────────────────
async fn delivery_group_info(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::writes::GroupInfoBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let main_conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::is_member(&main_conn, &parsed.conversation_id, &authed).await {
        Ok(true) => {}
        Ok(false) => return pollis_delivery::error::AuthRejection::Forbidden.into_response(),
        Err(e) => return ds_internal_error(format!("is_member: {e}")),
    }
    let log_conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::apply_group_info(&log_conn, &parsed).await {
        Ok(_) => ds_ok(),
        Err(e) => ds_internal_error(format!("group_info: {e}")),
    }
}

// ── /v1/welcomes/ack + /v1/welcomes/reset ─────────────────────────────────────
async fn delivery_welcomes_ack(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::writes::AckBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::ack_welcomes(&conn, &authed, &parsed.welcome_ids).await {
        Ok(_) => ds_ok(),
        Err(e) => ds_internal_error(format!("welcomes_ack: {e}")),
    }
}

async fn delivery_welcomes_reset(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::writes::ResetBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::reset_welcomes(&conn, &authed, parsed.device_id.as_deref()).await
    {
        Ok(_) => ds_ok(),
        Err(e) => ds_internal_error(format!("welcomes_reset: {e}")),
    }
}

// ── Domain A: messages/send + watermarks/advance + envelopes/gc (MAIN DB) ──────
async fn delivery_messages_send(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::SendMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_send_message(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("messages/send: {e}")),
    }
}

async fn delivery_watermarks_advance(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::WatermarkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_advance_watermark(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("watermarks/advance: {e}")),
    }
}

async fn delivery_envelopes_gc(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::EnvelopeGcBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_envelope_gc(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("envelopes/gc: {e}")),
    }
}

// ── Domain C: dm/create + dm/accept (MAIN DB) ─────────────────────────────────
async fn delivery_dm_create(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::CreateDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_create_dm(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("dm/create: {e}")),
    }
}

async fn delivery_dm_accept(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::AcceptDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_accept_dm(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("dm/accept: {e}")),
    }
}

// ── Domain D: key-packages (publish / claim / replenish) + devices/resign ──────
async fn delivery_key_packages(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::devices::PublishKeyPackagesBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::devices::apply_publish_key_packages(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("key-packages: {e}")),
    }
}

async fn delivery_key_packages_claim(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if let Err(resp) = ds_auth(&state.main, &method, &uri, &headers, &body).await {
        return resp;
    }
    let parsed: pollis_delivery::devices::ClaimKeyPackageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::devices::apply_claim_key_package(&conn, &parsed).await {
        Ok(o) => pollis_delivery::devices::claim_outcome_response(o),
        Err(e) => ds_internal_error(format!("key-packages/claim: {e}")),
    }
}

async fn delivery_key_packages_replenish(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::devices::ReplenishKeyPackagesBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::devices::apply_replenish_key_packages(&conn, Some(&authed), &parsed).await
    {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("key-packages/replenish: {e}")),
    }
}

async fn delivery_devices_resign(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::devices::ResignDeviceCertsBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::devices::apply_resign_device_certs(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("devices/resign: {e}")),
    }
}

// ── Server-side OTP + bootstrap (Goal B): the signup path ──────────────────────
async fn delivery_request_otp(State(state): State<DsState>, body: axum::body::Bytes) -> Response {
    let parsed: pollis_delivery::otp::RequestOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let email = parsed.email.trim().to_string();
    if !email.is_empty() {
        pollis_delivery::otp::process_request_otp(&state.otp, &state.otp_config, &email).await;
    }
    ds_ok()
}

async fn delivery_verify_otp(State(state): State<DsState>, body: axum::body::Bytes) -> Response {
    let parsed: pollis_delivery::otp::VerifyOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let email = parsed.email.trim().to_string();
    let device_id = parsed.device_id.trim().to_string();
    if email.is_empty() || device_id.is_empty() {
        return ds_bad_request();
    }
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::otp::apply_verify_otp(
        &conn,
        &state.otp,
        &state.sessions,
        &state.otp_config,
        &email,
        &parsed.code,
        &device_id,
    )
    .await
    {
        Ok(result) => pollis_delivery::otp::verify_otp_response(result),
        Err(e) => ds_internal_error(format!("verify-otp: {e}")),
    }
}

async fn delivery_establish_identity(
    State(state): State<DsState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let claims =
        match pollis_delivery::session::verify_session(&headers, &state.sessions, now_u64()) {
            Ok(c) => c,
            Err(rej) => return rej.into_response(),
        };
    let parsed: pollis_delivery::bootstrap::EstablishIdentityBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let (pub_bytes, salt, nonce, wrapped) = match (
        b64d(&parsed.account_id_pub),
        b64d(&parsed.salt),
        b64d(&parsed.nonce),
        b64d(&parsed.wrapped_key),
    ) {
        (Some(p), Some(s), Some(n), Some(w)) => (p, s, n, w),
        _ => return ds_bad_request(),
    };
    if pub_bytes.len() != 32 {
        return ds_bad_request();
    }
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::bootstrap::apply_establish_identity(
        &conn,
        &claims.user_id,
        &pub_bytes,
        &salt,
        &nonce,
        &wrapped,
    )
    .await
    {
        Ok(pollis_delivery::bootstrap::EstablishOutcome::Applied) => ds_ok(),
        Ok(pollis_delivery::bootstrap::EstablishOutcome::Conflict) => {
            ds_conflict("identity already established")
        }
        Err(e) => ds_internal_error(format!("establish-identity: {e}")),
    }
}

async fn delivery_register_device(
    State(state): State<DsState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let claims =
        match pollis_delivery::session::verify_session(&headers, &state.sessions, now_u64()) {
            Ok(c) => c,
            Err(rej) => return rej.into_response(),
        };
    let parsed: pollis_delivery::bootstrap::RegisterDeviceBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    if parsed.device_id.trim().is_empty() || parsed.device_id != claims.device_id {
        return pollis_delivery::error::AuthRejection::Forbidden.into_response();
    }
    let device_name = parsed
        .device_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "device".to_string());
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::bootstrap::apply_register_device(
        &conn,
        &claims.user_id,
        &claims.device_id,
        &device_name,
    )
    .await
    {
        Ok(()) => ds_ok(),
        Err(e) => ds_internal_error(format!("register-device: {e}")),
    }
}

async fn delivery_publish_device_cert(
    State(state): State<DsState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let now = now_u64();
    let parsed: pollis_delivery::bootstrap::PublishCertBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let cert_bytes = match b64d(&parsed.device_cert) {
        Some(b) => b,
        None => return ds_bad_request(),
    };
    let mls_sig_pub = match b64d(&parsed.mls_signature_pub) {
        Some(b) => b,
        None => return ds_bad_request(),
    };
    if parsed.cert_issued_at < 0 {
        return ds_bad_request();
    }
    let issued_at = parsed.cert_issued_at as u64;
    let session_token = pollis_delivery::session::session_token(&headers)
        .filter(|t| !t.is_empty())
        .map(|t| t.to_string());
    let session_claims = session_token
        .as_ref()
        .and_then(|t| state.sessions.resolve(t, now));
    let (user_id, device_id, invalidate_token) = match session_claims {
        Some(claims) => {
            if parsed.device_id != claims.device_id {
                return pollis_delivery::error::AuthRejection::Forbidden.into_response();
            }
            (claims.user_id, claims.device_id, session_token)
        }
        None => {
            let uid = match parsed.user_id.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(u) => u.to_string(),
                None => return pollis_delivery::error::AuthRejection::Unauthorized.into_response(),
            };
            (uid, parsed.device_id.clone(), None)
        }
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::bootstrap::apply_publish_device_cert(
        &conn,
        &user_id,
        &device_id,
        &cert_bytes,
        issued_at,
        parsed.cert_identity_version,
        &mls_sig_pub,
    )
    .await
    {
        Ok(pollis_delivery::bootstrap::PublishCertOutcome::Applied) => {
            if let Some(token) = invalidate_token {
                state.sessions.invalidate(&token);
            }
            ds_ok()
        }
        Ok(pollis_delivery::bootstrap::PublishCertOutcome::IdentityNotEstablished) => {
            ds_conflict("account identity not established")
        }
        Ok(pollis_delivery::bootstrap::PublishCertOutcome::CertInvalid) => {
            pollis_delivery::error::AuthRejection::Unauthorized.into_response()
        }
        Ok(pollis_delivery::bootstrap::PublishCertOutcome::DeviceNotRegistered) => {
            ds_conflict("device not registered for this user")
        }
        Err(e) => ds_internal_error(format!("publish-device-cert: {e}")),
    }
}

// ── Domains E + G (#419) — device-enrollment / security audit ─────────────────
// Copied verbatim-in-pattern from the flows harness (`flows/harness.rs`): the
// same `pollis_delivery::{bootstrap,account}::apply_*` calls the desktop DS runs.
// Only the set the M4 enrollment + recovery flows touch is wired.

// ── /v1/auth/enrollment-request — SESSION-gated INSERT of a pending request ────
// The requesting (new) device is pre-credential (`mls_signature_pub` NULL), so
// it cannot device-sign; the write authenticates via the `enrollment_session`
// minted by re-login `verify_otp`. The DS binds user + device from the session.
async fn delivery_enrollment_request(
    State(state): State<DsState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let claims =
        match pollis_delivery::session::verify_session(&headers, &state.sessions, now_u64()) {
            Ok(c) => c,
            Err(rej) => return rej.into_response(),
        };
    let parsed: pollis_delivery::bootstrap::EnrollmentRequestBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let ephemeral_pub = match b64d(&parsed.new_device_ephemeral_pub) {
        Some(b) => b,
        None => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::bootstrap::apply_enrollment_request(
        &conn,
        &claims.user_id,
        &claims.device_id,
        &parsed.request_id,
        &ephemeral_pub,
        &parsed.verification_code,
        &parsed.created_at,
        &parsed.expires_at,
    )
    .await
    {
        Ok(()) => ds_ok(),
        Err(e) => ds_internal_error(format!("enrollment-request: {e}")),
    }
}

// ── /v1/enrollment/approve — DEVICE-signed by an already-enrolled sibling ──────
async fn delivery_enrollment_approve(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::ApproveEnrollmentBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_approve_enrollment(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("enrollment/approve: {e}")),
    }
}

// ── /v1/enrollment/reject — DEVICE-signed by an already-enrolled sibling ───────
async fn delivery_enrollment_reject(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::RejectEnrollmentBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_reject_enrollment(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("enrollment/reject: {e}")),
    }
}

// ── /v1/security-events — DEVICE-signed audit rows (best-effort in the client) ─
async fn delivery_security_events(
    State(state): State<DsState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::SecurityEventBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_record_security_event(&conn, Some(&authed), &parsed).await
    {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("security-events: {e}")),
    }
}

/// Boot the axum router with ONLY the routes the DM message path exercises, on a
/// dedicated OS thread + runtime so the server outlives the per-test runtime.
async fn spawn_in_process_delivery(main: Arc<RemoteDb>, log: Arc<RemoteDb>) -> String {
    use std::sync::mpsc;

    let state = DsState {
        main,
        log,
        otp: pollis_delivery::otp::OtpStore::default(),
        sessions: pollis_delivery::session::SessionStore::default(),
        otp_config: pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
        },
    };

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("in-process-delivery".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("delivery: build runtime");
            rt.block_on(async move {
                let router = axum::Router::new()
                    // MLS control plane
                    .route("/v1/commits", axum::routing::post(delivery_submit))
                    .route("/v1/group-info", axum::routing::post(delivery_group_info))
                    .route("/v1/welcomes/ack", axum::routing::post(delivery_welcomes_ack))
                    .route(
                        "/v1/welcomes/reset",
                        axum::routing::post(delivery_welcomes_reset),
                    )
                    // Messages / envelopes (MAIN DB)
                    .route(
                        "/v1/messages/send",
                        axum::routing::post(delivery_messages_send),
                    )
                    .route(
                        "/v1/watermarks/advance",
                        axum::routing::post(delivery_watermarks_advance),
                    )
                    .route("/v1/envelopes/gc", axum::routing::post(delivery_envelopes_gc))
                    // DM membership
                    .route("/v1/dm/create", axum::routing::post(delivery_dm_create))
                    .route("/v1/dm/accept", axum::routing::post(delivery_dm_accept))
                    // Key packages + device certs
                    .route("/v1/key-packages", axum::routing::post(delivery_key_packages))
                    .route(
                        "/v1/key-packages/claim",
                        axum::routing::post(delivery_key_packages_claim),
                    )
                    .route(
                        "/v1/key-packages/replenish",
                        axum::routing::post(delivery_key_packages_replenish),
                    )
                    .route(
                        "/v1/devices/resign",
                        axum::routing::post(delivery_devices_resign),
                    )
                    // Signup / bootstrap
                    .route(
                        "/v1/auth/request-otp",
                        axum::routing::post(delivery_request_otp),
                    )
                    .route("/v1/auth/verify-otp", axum::routing::post(delivery_verify_otp))
                    .route(
                        "/v1/auth/establish-identity",
                        axum::routing::post(delivery_establish_identity),
                    )
                    .route(
                        "/v1/auth/register-device",
                        axum::routing::post(delivery_register_device),
                    )
                    .route(
                        "/v1/auth/publish-device-cert",
                        axum::routing::post(delivery_publish_device_cert),
                    )
                    // Device enrollment + recovery (Domains E + G) — M4
                    .route(
                        "/v1/auth/enrollment-request",
                        axum::routing::post(delivery_enrollment_request),
                    )
                    .route(
                        "/v1/enrollment/approve",
                        axum::routing::post(delivery_enrollment_approve),
                    )
                    .route(
                        "/v1/enrollment/reject",
                        axum::routing::post(delivery_enrollment_reject),
                    )
                    .route(
                        "/v1/security-events",
                        axum::routing::post(delivery_security_events),
                    )
                    .with_state(state);
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("delivery: bind loopback");
                let addr = listener.local_addr().expect("delivery: local_addr");
                tx.send(format!("http://{addr}")).expect("delivery: send url");
                if let Err(e) = axum::serve(listener, router).await {
                    eprintln!("[harness] in-process delivery exited: {e}");
                }
            });
        })
        .expect("delivery: spawn thread");

    rx.recv().expect("delivery: receive url")
}

// ─── Test client (no Tauri; calls pollis_core directly) ───────────────────────

/// One simulated device for one user. Owns its own read-only main view +
/// `AppState` + `InMemoryKeystore`; shares the world's writable `main`/`log`
/// (which only the DS writes through).
pub struct TestClient {
    pub state: Arc<AppState>,
    pub profile: Option<auth::UserProfile>,
    /// When set, this device's `POLLIS_DATA_DIR` — repointed (via [`use_dir`])
    /// before any keystore/local-DB/`accounts.json` touch. `None` means "use the
    /// world's shared dir" (the single-device smokes). Two devices of the SAME
    /// user MUST each set their own, or they collide on disk.
    data_dir: Option<std::path::PathBuf>,
}

impl TestClient {
    /// Build a fresh, signed-out client against the shared world.
    pub fn new(world: &World) -> Self {
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            // Read-only main view — mirrors the production read-only Turso token.
            // Shares the writable handle's `Database` (sees the DS's writes with no
            // WAL lag) but rejects any DIRECT write, so a client-side write that
            // should have gone through the DS fails the test loudly.
            Arc::new(world.main.query_only_view()),
            // Log handle: client only reads it (welcomes / commits); DS is the
            // sole writer.
            world.log.clone(),
            keystore,
        ));
        Self {
            state,
            profile: None,
            data_dir: None,
        }
    }

    /// Point `POLLIS_DATA_DIR` at this client's own dir, if it has one. Called at
    /// the start of every keystore/DB-touching path so a second device of the same
    /// user reads/writes its OWN on-disk state. No-op for shared-dir clients.
    ///
    /// Safe because a single test runs its clients sequentially (device A does its
    /// work, then device B does its), and each integration-test file is its own
    /// process — so this process-global swap never races another test.
    fn use_dir(&self) {
        if let Some(dir) = &self.data_dir {
            std::env::set_var("POLLIS_DATA_DIR", dir);
        }
    }

    /// Build a fresh, signed-out client whose keystore is the **file-backed**
    /// `default_os_keystore` (persistent under `POLLIS_DATA_DIR`) rather than the
    /// in-memory one. Needed by the restart/resync gate: the identity + session
    /// must survive a `drop` of the `AppState`, which only a file keystore does.
    /// Media/os-keystore are off under pollis-tui's build, so `default_os_keystore`
    /// resolves to the file JSON store (spec §5) — no dbus, zero extra deps.
    pub fn new_persistent(world: &World) -> Self {
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            Arc::new(world.main.query_only_view()),
            world.log.clone(),
            default_os_keystore(),
        ));
        Self {
            state,
            profile: None,
            data_dir: None,
        }
    }

    /// Like [`new_persistent`], but pinned to its OWN `POLLIS_DATA_DIR` (a subdir
    /// `name` under the world's temp dir). Required whenever two clients belong to
    /// the SAME user (the multi-device enrollment + Secret-Key recovery smokes):
    /// each device needs an isolated local DB + keystore + accounts index. The dir
    /// is repointed just-in-time by [`use_dir`] before every on-disk touch.
    pub fn new_persistent_in(world: &World, name: &str) -> Self {
        let dir = world.device_dir(name);
        // Build the keystore under THIS device's dir (default_os_keystore reads
        // POLLIS_DATA_DIR eagerly at construction).
        std::env::set_var("POLLIS_DATA_DIR", &dir);
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            Arc::new(world.main.query_only_view()),
            world.log.clone(),
            default_os_keystore(),
        ));
        Self {
            state,
            profile: None,
            data_dir: Some(dir),
        }
    }

    /// Simulate a quit→relaunch: drop the current `AppState` and rebuild a NEW
    /// one on the SAME `POLLIS_DATA_DIR` + same libsql handles, with a FRESH
    /// `default_os_keystore`. The profile is retained in-memory only as the
    /// test's record of "who this device is" — the rebuilt state knows nothing
    /// until `auth::boot` rehydrates it from the persisted accounts index +
    /// keystore. Re-activates this user first so the accounts index's
    /// `last_active_user` points back at them (a prior client's `activate` may
    /// have moved it), which is what `get_session` keys off.
    pub fn restart(&mut self, world: &World) {
        self.activate();
        // Rebuild the keystore against THIS device's dir (default_os_keystore reads
        // POLLIS_DATA_DIR eagerly); `activate` already repointed it via `use_dir`.
        // Replace the Arc: the old AppState (and its file keystore handle) drops
        // when the last reference goes. The rebuilt state reads the same on-disk
        // keystore + local SQLCipher DB the previous instance wrote.
        self.state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            Arc::new(world.main.query_only_view()),
            world.log.clone(),
            default_os_keystore(),
        ));
    }

    pub fn user_id(&self) -> &str {
        &self.profile.as_ref().expect("not signed in").id
    }

    /// Point the process-global active user (accounts.json `last_active_user`) at
    /// THIS client. `current_user_id` prefers `state.unlock`, so per-client
    /// signing already works after signup; this keeps the shared fallback honest
    /// for any path that consults it.
    fn activate(&self) {
        // Repoint POLLIS_DATA_DIR FIRST so the accounts-index write below (and
        // every subsequent DB/keystore touch) lands in THIS device's dir.
        self.use_dir();
        if let Some(p) = &self.profile {
            let _ = accounts::upsert_account(&p.id, &p.username, None, None);
        }
    }

    /// First-device signup: the exact §7 order via the pollis-tui auth wrappers
    /// (request_otp → verify_otp → set_pin → initialize_identity). Everything
    /// routes through the in-process DS.
    pub async fn sign_up(&mut self, email: &str) -> auth::UserProfile {
        // Repoint POLLIS_DATA_DIR before verify_otp generates identity material +
        // writes the device id to the keystore (both key off the data dir).
        self.use_dir();
        pollis_tui::auth::request_otp(&self.state, email)
            .await
            .unwrap_or_else(|e| panic!("request_otp({email}): {e}"));
        let profile = pollis_tui::auth::verify_otp(&self.state, email, DEV_OTP)
            .await
            .unwrap_or_else(|e| panic!("verify_otp({email}): {e}"));
        self.profile = Some(profile.clone());
        self.activate();
        pollis_tui::auth::set_pin_and_init(&self.state, &profile.id, TEST_PIN)
            .await
            .unwrap_or_else(|e| panic!("set_pin_and_init: {e}"));
        profile
    }

    /// Create a 2-person DM to `other`, returning the DM channel id. Reconcile
    /// (inside `create_dm_channel`) adds `other`'s device to the MLS tree and
    /// queues their Welcome.
    pub async fn create_dm(&self, other_user_id: &str) -> String {
        self.activate();
        let dm = dm::create_dm_channel(
            self.user_id().to_string(),
            vec![self.user_id().to_string(), other_user_id.to_string()],
            &self.state,
        )
        .await
        .expect("create_dm_channel");
        dm.id
    }

    /// Accept a pending DM request (flip our own `accepted_at`), via the DS.
    pub async fn accept_dm(&self, dm_channel_id: &str) {
        self.activate();
        dm::accept_dm_request(dm_channel_id.to_string(), self.user_id().to_string(), &self.state)
            .await
            .expect("accept_dm_request");
    }

    /// List this user's pending DM requests.
    pub async fn dm_requests(&self) -> Vec<dm::DmChannel> {
        self.activate();
        dm::list_dm_requests(self.user_id().to_string(), &self.state)
            .await
            .expect("list_dm_requests")
    }

    /// Send a text message to a conversation, via the DS.
    pub async fn send(&self, conversation_id: &str, content: &str) {
        self.activate();
        let username = self.profile.as_ref().map(|p| p.username.clone());
        messages::send_message(
            conversation_id.to_string(),
            self.user_id().to_string(),
            content.to_string(),
            None,
            username,
            &self.state,
        )
        .await
        .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
    }

    /// Send a text message through the TUI's OWN write layer
    /// (`pollis_tui::send::send_text`) — the code under test for the M3 gate —
    /// rather than reaching into `pollis_core::commands` directly like [`send`].
    /// Routes through the DS exactly the same way.
    pub async fn send_text(&self, conversation_id: &str, content: &str) {
        self.activate();
        let username = self.profile.as_ref().map(|p| p.username.clone());
        pollis_tui::send::send_text(&self.state, self.user_id(), username, conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_text({conversation_id}): {e}"));
    }

    /// Drive one full §6 sync pass for this client (the code under test).
    pub async fn sync_once(&self) {
        self.activate();
        pollis_tui::sync::sync_once(&self.state, self.user_id())
            .await
            .expect("sync_once");
    }

    /// Run `rounds` sync passes (the spec's ~4-round interleaved catch-up).
    pub async fn sync_rounds(&self, rounds: usize) {
        self.activate();
        pollis_tui::sync::sync_rounds(&self.state, self.user_id(), rounds)
            .await
            .expect("sync_rounds");
    }

    /// Read one page of a DM's messages via the data layer (ingest + decrypt).
    pub async fn read_dm(&self, dm_channel_id: &str) -> messages::MessagePage {
        self.activate();
        pollis_tui::data::dm_messages(&self.state, self.user_id(), dm_channel_id, None)
            .await
            .expect("dm_messages")
    }

    /// The set of conversation ids this device currently sees (proves an enrolled
    /// device picked up the account's DMs/groups after external-join).
    pub async fn conversation_ids(&self) -> Vec<String> {
        self.activate();
        pollis_tui::data::load_conversations(&self.state, self.user_id())
            .await
            .expect("load_conversations")
            .conversation_ids()
    }

    // ── M4: multi-device enrollment + Secret-Key recovery (the code under test) ──

    /// A FRESH device signing in against an EXISTING account's email. Runs
    /// `request_otp` → `verify_otp`; `verify_otp` resolves to the existing
    /// `user_id`, registers this device (session-gated), mints the in-memory
    /// `enrollment_session`, and reports `enrollment_required = true`. Sets
    /// `self.profile` but the device does NOT yet hold the account key.
    pub async fn begin_enrollment(&mut self, email: &str) -> auth::UserProfile {
        self.use_dir();
        pollis_tui::auth::request_otp(&self.state, email)
            .await
            .unwrap_or_else(|e| panic!("request_otp({email}) on new device: {e}"));
        let profile = pollis_tui::auth::verify_otp(&self.state, email, DEV_OTP)
            .await
            .unwrap_or_else(|e| panic!("verify_otp({email}) on new device: {e}"));
        assert!(
            profile.enrollment_required,
            "a fresh device for an existing account must see enrollment_required=true",
        );
        self.profile = Some(profile.clone());
        self.activate();
        profile
    }

    /// New device: kick off an enrollment request (returns the handle carrying the
    /// request id + verification code).
    pub async fn request_enrollment(&self) -> pollis_tui::enroll::EnrollmentHandle {
        self.activate();
        pollis_tui::enroll::request_enrollment(&self.state, self.user_id().to_string())
            .await
            .expect("request_enrollment")
    }

    /// New device: poll the enrollment status once.
    pub async fn enrollment_status(&self, request_id: &str) -> pollis_tui::enroll::EnrollmentStatus {
        self.activate();
        pollis_tui::enroll::enrollment_status(&self.state, request_id.to_string())
            .await
            .expect("enrollment_status")
    }

    /// Existing device: list open enrollment requests for this account.
    pub async fn pending_enrollment_requests(
        &self,
    ) -> Vec<pollis_tui::enroll::PendingEnrollmentRequest> {
        self.activate();
        pollis_tui::enroll::pending_requests(&self.state, self.user_id().to_string())
            .await
            .expect("pending_requests")
    }

    /// Existing device: approve a pending request, confirming its code.
    pub async fn approve_enrollment(&self, request_id: &str, verification_code: &str) {
        self.activate();
        pollis_tui::enroll::approve(
            &self.state,
            request_id.to_string(),
            verification_code.to_string(),
        )
        .await
        .expect("approve_enrollment");
    }

    /// New device: complete an enrollment/recovery after the account key is in
    /// `AppState.unlock`. Mirrors what the desktop `App.tsx` runs after pin-create:
    /// `set_pin` (opens the local DB) → `finalize` (cert/KPs/external-join) →
    /// `initialize_identity`.
    pub async fn finish_enrollment(&self) {
        self.activate();
        pin::set_pin(&self.state, None, TEST_PIN.to_string())
            .await
            .expect("set_pin on enrolled device");
        pollis_tui::enroll::finalize(&self.state, self.user_id().to_string())
            .await
            .expect("finalize_device_enrollment");
        auth::initialize_identity(&self.state, self.user_id().to_string())
            .await
            .expect("initialize_identity on enrolled device");
    }

    /// Poll to approval, then finish. Bounded loop — the approve write has already
    /// committed by the time we get here, so this resolves on the first poll.
    pub async fn await_approval_and_finish(&self, request_id: &str) {
        use pollis_tui::enroll::EnrollmentStatus;
        for attempt in 0..20 {
            match self.enrollment_status(request_id).await {
                EnrollmentStatus::Approved => break,
                EnrollmentStatus::Pending if attempt < 19 => continue,
                other => panic!("enrollment did not reach Approved; last status = {other:?}"),
            }
        }
        self.finish_enrollment().await;
    }

    /// New device: Secret-Key recovery. Unwraps the account key with `secret_key`
    /// (into `AppState.unlock`), then runs the same tail as enrollment: `set_pin`
    /// → `finalize` → `initialize_identity`.
    pub async fn recover(&self, secret_key: &str) {
        self.activate();
        pollis_tui::enroll::recover(
            &self.state,
            self.user_id().to_string(),
            secret_key.to_string(),
        )
        .await
        .expect("recover_with_secret_key");
        self.finish_enrollment().await;
    }
}
