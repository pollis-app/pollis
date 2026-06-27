//! End-to-end integration harness for Pollis.
//!
//! Drives the real `#[tauri::command]` functions through the tauri IPC
//! pipeline — no `_inner` shims, no mocked DB layer. Each [`TestClient`]
//! owns its own `App<MockRuntime>` backed by its own `InMemoryKeystore`,
//! while all clients share a single [`TestWorld`] pointed at a process-local
//! libsql file (no network round-trip — see `RemoteDb::connect_local`).
//!
//! Run with:
//! ```
//! cargo test --features test-harness --test flows
//! ```
//!
//! Tests serialize on a process-wide mutex (`serial_test`) so the shared
//! Turso wipe between tests can't race.

use std::sync::Arc;

use pollis_lib::commands::auth::UserProfile;
use pollis_lib::config::Config;
use pollis_lib::db::remote::RemoteDb;
use pollis_lib::keystore::{InMemoryKeystore, Keystore};
use pollis_lib::state::AppState;
use pollis_lib::test_harness::{
    bootstrap_log_schema, bootstrap_schema, build_client_app, drop_log_tables_from_main, invoke,
    wipe_remote,
};
use serde_json::json;
use tauri::test::MockRuntime;
use tauri::{App, WebviewWindow};
use tokio::sync::OnceCell;

pub(crate) const DEV_OTP: &str = "000000";
/// Fixed PIN used by `TestClient::sign_up` so every harness client
/// has its DB open after signup. Real users get four random digits;
/// the test value is just a constant.
pub(crate) const TEST_PIN: &str = "0000";

// ─── World ──────────────────────────────────────────────────────────────────

/// Shared across all clients in a single test. Owns the libsql file that
/// stands in for "remote Turso" plus a temp dir that backs per-user
/// SQLCipher files.
///
/// Construction is lazy + process-wide so integration tests share one
/// backend file but still run serially (the wipe would race otherwise).
pub(crate) struct TestWorld {
    /// Main DB — users/devices/groups/channels/membership/messages, plus the
    /// auth lookups (`user_device`) the DS verifies against.
    pub(crate) remote: Arc<RemoteDb>,
    /// Commit-log DB — the three MLS control-plane tables (`mls_commit_log`,
    /// `mls_welcome`, `mls_group_info`) and NOTHING else. A genuinely separate
    /// libsql file so a misrouted query (a main read on the log handle, or a
    /// log read on the main handle) fails with "no such table" instead of
    /// silently succeeding on one shared file. Mirrors the #420 production split.
    pub(crate) log: Arc<RemoteDb>,
    pub(crate) config: Config,
}

static WORLD: OnceCell<TestWorld> = OnceCell::const_new();

pub(crate) async fn world() -> &'static TestWorld {
    WORLD
        .get_or_init(|| async {
            // Loads .env.test and bypasses R2/LiveKit/Resend with placeholders.
            let mut config = Config::for_test().expect("Config::for_test");

            // Isolate local SQLCipher files to a process-unique temp dir so
            // stale `pollis_{user_id}.db` files can't leak between `cargo test`
            // invocations.
            let tmp = tempfile::tempdir().expect("tempdir");
            // Keep the tempdir alive for the life of the process — cleanup
            // runs at exit. Dropping it during an ongoing test would delete
            // open DBs.
            let path = tmp.keep();
            std::env::set_var("POLLIS_DATA_DIR", &path);

            // DEV_OTP short-circuits email send in request_otp and fixes the
            // OTP to a known value — safe because debug_assertions is on in
            // integration tests.
            std::env::set_var("DEV_OTP", DEV_OTP);

            // Stand-in for "remote Turso" (the MAIN DB) — a libsql file in the
            // same temp dir as the per-user SQLCipher DBs. No network round-trip.
            let remote_db_path = path.join("test_turso.db");
            let remote = Arc::new(
                RemoteDb::connect_local(&remote_db_path)
                    .await
                    .expect("connect local libsql"),
            );

            bootstrap_schema(&remote)
                .await
                .expect("bootstrap test turso schema");

            // SECOND physical libsql file for the commit-log DB. This is the
            // whole point of the split harness: the three MLS control-plane
            // tables live ONLY here, so a query routed to the wrong connection
            // hits "no such table" instead of silently finding every table on a
            // single shared file (which masked the #420 cross-signing misroute,
            // commit e4cfe9e).
            let log_db_path = path.join("test_log.db");
            let log = Arc::new(
                RemoteDb::connect_local(&log_db_path)
                    .await
                    .expect("connect local log libsql"),
            );
            bootstrap_log_schema(&log)
                .await
                .expect("bootstrap log db schema");
            // The baseline created the three MLS tables on MAIN too (for old
            // shipped clients); drop them so main no longer has them and any
            // stray main-side access fails loudly.
            drop_log_tables_from_main(&remote)
                .await
                .expect("drop log tables from main");

            // Spin up the real MLS Delivery Service in-process and route every
            // client's commit submission through it. This exercises the deployed
            // DS path (HTTP → pollis-delivery::commit::submit_commit) end to end
            // in the flows suite instead of the Direct write path.
            //
            // The DS gets BOTH handles, exactly mirroring production
            // (`pollis-delivery`'s `AppState { db, log_db }`): it authenticates
            // and checks membership on the MAIN handle, and writes/reads the
            // commit log on the LOG handle. Sharing the harness's own handles
            // (rather than opening second `Builder::new_local`s) makes the DS the
            // sole writer to the very rows the clients read — two independent
            // libsql `Database`s on one file don't share WAL writes promptly. A
            // single shared DS for the whole test run is fine.
            let delivery_url = spawn_in_process_delivery(remote.clone(), log.clone()).await;
            config.pollis_delivery_url = Some(delivery_url);

            TestWorld { remote, log, config }
        })
        .await
}

/// One-shot fault for the in-process DS: when armed, the next ACCEPTED commit is
/// fully written (commit + GroupInfo + Welcomes land) but its success response
/// is turned into a 500 — simulating a lost success-response. This exercises the
/// client's idempotent adopt-on-lost-response path (issue #411) on the real DS
/// (HTTP) path, which is the only path that can actually lose a response.
pub(crate) static LOSE_NEXT_DS_RESPONSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Two-handle state for the in-process DS, mirroring production
/// `pollis_delivery::AppState { db, log_db }`: auth + membership lookups run on
/// `main`, commit-log reads/writes run on `log`. Splitting them is what lets a
/// misrouted query (a `users`/`user_device` read on the log handle, or an
/// `mls_*` read on the main handle) fail loudly in the flows suite.
#[derive(Clone)]
struct DsState {
    /// Main DB — `user_device` (auth) + group/DM membership (`is_member`).
    main: Arc<RemoteDb>,
    /// Commit-log DB — `mls_commit_log` / `mls_welcome` / `mls_group_info`.
    log: Arc<RemoteDb>,
    /// In-memory OTP store shared by request-otp / verify-otp (Goal B #419).
    /// Shallow-`Clone` (shared `Arc`), so every cloned `DsState` — one per
    /// request — sees the same codes.
    otp: pollis_delivery::otp::OtpStore,
    /// In-memory OTP-session store gating the three bootstrap writes.
    sessions: pollis_delivery::session::SessionStore,
    /// OTP/session tunables. DEV_OTP is forced so verify-otp accepts the fixed
    /// harness code with no real email send.
    otp_config: pollis_delivery::otp::OtpConfig,
    /// Email-change OTP store + requester binding (device-signed). Separate from
    /// `otp` so a signup OTP and an email-change to the same address can't collide.
    email_change: pollis_delivery::email_change::EmailChangeStore,
}

/// Shared auth gate for the in-process DS: verify the device signature over the
/// RAW body (the signature binds `sha256(body)`, so we must hash the exact wire
/// bytes) and return the authenticated `user_id`, or an auth-rejection response.
///
/// Auth is ALWAYS enforced here — the in-process DS mirrors production with
/// `POLLIS_DS_REQUIRE_AUTH` on, so the flows suite exercises the signed write
/// path end to end (this is #420 / #419 Step 1's acceptance: "auth on + clients
/// signing → full flows suite still passes"). It reuses the real
/// `pollis_delivery::auth::verify_request` against the MAIN DB — `user_device`
/// (the auth lookup) lives there, not on the log DB.
async fn ds_auth(
    remote: &RemoteDb,
    method: &axum::http::Method,
    uri: &axum::http::Uri,
    headers: &axum::http::HeaderMap,
    body: &axum::body::Bytes,
) -> Result<String, axum::response::Response> {
    use axum::response::IntoResponse;
    let conn = remote.conn().await.map_err(|e| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("conn: {e}"),
        )
            .into_response()
    })?;
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

fn ds_internal_error(msg: String) -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
}

fn ds_bad_request() -> axum::response::Response {
    use axum::response::IntoResponse;
    (axum::http::StatusCode::BAD_REQUEST, "invalid body").into_response()
}

fn ds_ok() -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::OK,
        axum::Json(serde_json::json!({ "status": "ok" })),
    )
        .into_response()
}

/// 409 with the DS's conflict envelope — the bootstrap CAS / out-of-order cases.
fn ds_conflict(msg: &str) -> axum::response::Response {
    use axum::response::IntoResponse;
    (
        axum::http::StatusCode::CONFLICT,
        axum::Json(serde_json::json!({ "status": "conflict", "error": msg })),
    )
        .into_response()
}

/// STANDARD base64 decode, `None` on malformed input — for the bootstrap
/// endpoints' base64-encoded key/cert fields.
fn b64d(s: &str) -> Option<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

/// Unix seconds as `u64` — the OTP/session stores' clock type (distinct from
/// `pollis_delivery::auth::now_unix`'s `i64` signature-timestamp clock).
fn now_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Map a domain-A [`pollis_delivery::writes::WriteOutcome`] to a Response —
/// 200 on success, 403 on a refused authz check.
fn ds_outcome(outcome: pollis_delivery::writes::WriteOutcome) -> axum::response::Response {
    use axum::response::IntoResponse;
    match outcome {
        pollis_delivery::writes::WriteOutcome::Ok => ds_ok(),
        pollis_delivery::writes::WriteOutcome::Forbidden => {
            pollis_delivery::error::AuthRejection::Forbidden.into_response()
        }
    }
}

/// The `/v1/commits` submit handler, driven against the SHARED `RemoteDb` so
/// the DS writes land on the exact handle the clients read from. Mirrors
/// `pollis_delivery`'s real `submit` arm: verify the signature over the raw
/// body, bind `sender_id` to the authenticated user, then submit — PLUS the
/// harness-only lost-response fault injection the #411 test depends on.
async fn delivery_submit(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use pollis_delivery::commit::{SubmitBody, SubmitResponse};

    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };

    let parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };

    // Identity binding: a validly-signed request may only write as itself.
    if parsed.sender_id != authed {
        return pollis_delivery::error::AuthRejection::Forbidden.into_response();
    }

    // The commit log lives on the LOG DB.
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::commit::submit_commit(&conn, &parsed).await {
        Ok(outcome) => {
            // Lost-response fault: the commit + GroupInfo + Welcomes have already
            // LANDED above; drop the success response so the client must recover
            // by observing the commit is canonical and adopting it (issue #411).
            if matches!(outcome, SubmitResponse::Accepted { .. })
                && LOSE_NEXT_DS_RESPONSE.swap(false, std::sync::atomic::Ordering::SeqCst)
            {
                return (
                    axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                    "simulated lost submit response (test fault injection)",
                )
                    .into_response();
            }
            let code = match &outcome {
                SubmitResponse::Accepted { .. } => axum::http::StatusCode::OK,
                SubmitResponse::Rejected { .. } => axum::http::StatusCode::CONFLICT,
            };
            (code, axum::Json(outcome)).into_response()
        }
        Err(e) => ds_internal_error(format!("submit: {e}")),
    }
}

/// `POST /v1/group-info` (W4) — republish GroupInfo, authed user must be a
/// current member of the conversation. Reuses the real `pollis_delivery::writes`
/// handlers against the shared connection.
async fn delivery_group_info(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::writes::GroupInfoBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    // Membership is checked on the MAIN DB (group/DM membership lives there).
    let main_conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::is_member(&main_conn, &parsed.conversation_id, &authed).await {
        Ok(true) => {}
        Ok(false) => return pollis_delivery::error::AuthRejection::Forbidden.into_response(),
        Err(e) => return ds_internal_error(format!("is_member: {e}")),
    }
    // The GroupInfo write lands on the LOG DB.
    let log_conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::apply_group_info(&log_conn, &parsed).await {
        Ok(_) => ds_ok(),
        Err(e) => ds_internal_error(format!("group_info: {e}")),
    }
}

/// `POST /v1/welcomes/ack` (W5) — mark Welcomes delivered, scoped to the
/// authenticated recipient.
async fn delivery_welcomes_ack(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/welcomes/reset` (W6/W7) — re-arm Welcomes for redelivery, scoped to
/// the authenticated recipient.
async fn delivery_welcomes_reset(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/welcomes/purge` (W8) — delete all of the authenticated user's
/// Welcomes. An empty body is valid (recipient comes from auth).
async fn delivery_welcomes_purge(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    // Tolerate an empty body — the recipient is derived from auth.
    if !body.is_empty() {
        if serde_json::from_slice::<pollis_delivery::writes::PurgeBody>(&body).is_err() {
            return ds_bad_request();
        }
    }
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::writes::purge_welcomes(&conn, &authed).await {
        Ok(_) => ds_ok(),
        Err(e) => ds_internal_error(format!("welcomes_purge: {e}")),
    }
}

// ─── Domain A (#419) — messages / envelopes / watermarks / reactions ────────
//
// Every domain-A endpoint runs the SAME pure `apply_*` fn the production axum
// handler runs, against the MAIN DB (these tables are not on the log DB). Auth
// is always enforced here (`ds_auth` → `Some(authed)`), so the flows suite
// exercises the signed write + server-side authz path end to end.

/// `POST /v1/messages/send`.
async fn delivery_messages_send(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/messages/edit`.
async fn delivery_messages_edit(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::EditMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_edit_message(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("messages/edit: {e}")),
    }
}

/// `POST /v1/messages/delete`.
async fn delivery_messages_delete(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::DeleteMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_delete_message(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("messages/delete: {e}")),
    }
}

/// `POST /v1/reactions/add`.
async fn delivery_reactions_add(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::ReactionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_add_reaction(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("reactions/add: {e}")),
    }
}

/// `POST /v1/reactions/remove`.
async fn delivery_reactions_remove(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::ReactionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_remove_reaction(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("reactions/remove: {e}")),
    }
}

/// `POST /v1/watermarks/advance`.
async fn delivery_watermarks_advance(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/envelopes/gc`.
async fn delivery_envelopes_gc(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/attachments/register`.
async fn delivery_attachments_register(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::AttachmentRegisterBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_register_attachment(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("attachments/register: {e}")),
    }
}

/// `POST /v1/attachments/delete`.
async fn delivery_attachments_delete(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::messages::AttachmentDeleteBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::messages::apply_delete_attachment(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("attachments/delete: {e}")),
    }
}

// ─── Domain B (#419) — groups / channels / membership / invites / join-reqs ──
//
// Every domain-B endpoint runs the SAME pure `apply_*` fn the production axum
// handler runs, against the MAIN DB. Auth is always enforced here, so the flows
// suite exercises the signed write + server-side authz path end to end.

/// Generate a domain-B delivery handler: gate → parse → run the `apply_*`
/// against the MAIN DB → map the outcome. Collapses the otherwise-identical
/// per-endpoint boilerplate the domain-A handlers spell out by hand.
macro_rules! delivery_b {
    ($name:ident, $body:ty, $apply:path, $label:literal) => {
        async fn $name(
            axum::extract::State(state): axum::extract::State<DsState>,
            method: axum::http::Method,
            uri: axum::http::Uri,
            headers: axum::http::HeaderMap,
            body: axum::body::Bytes,
        ) -> axum::response::Response {
            let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
                Ok(u) => u,
                Err(resp) => return resp,
            };
            let parsed: $body = match serde_json::from_slice(&body) {
                Ok(b) => b,
                Err(_) => return ds_bad_request(),
            };
            let conn = match state.main.conn().await {
                Ok(c) => c,
                Err(e) => return ds_internal_error(format!("conn: {e}")),
            };
            match $apply(&conn, Some(&authed), &parsed).await {
                Ok(o) => ds_outcome(o),
                Err(e) => ds_internal_error(format!("{}: {e}", $label)),
            }
        }
    };
}

delivery_b!(
    delivery_groups_create,
    pollis_delivery::groups::CreateGroupBody,
    pollis_delivery::groups::apply_create_group,
    "groups/create"
);
delivery_b!(
    delivery_groups_update,
    pollis_delivery::groups::UpdateGroupBody,
    pollis_delivery::groups::apply_update_group,
    "groups/update"
);
delivery_b!(
    delivery_groups_delete,
    pollis_delivery::groups::DeleteGroupBody,
    pollis_delivery::groups::apply_delete_group,
    "groups/delete"
);
delivery_b!(
    delivery_groups_leave,
    pollis_delivery::groups::LeaveGroupBody,
    pollis_delivery::groups::apply_leave_group,
    "groups/leave"
);
delivery_b!(
    delivery_channels_create,
    pollis_delivery::groups::CreateChannelBody,
    pollis_delivery::groups::apply_create_channel,
    "channels/create"
);
delivery_b!(
    delivery_channels_update,
    pollis_delivery::groups::UpdateChannelBody,
    pollis_delivery::groups::apply_update_channel,
    "channels/update"
);
delivery_b!(
    delivery_channels_delete,
    pollis_delivery::groups::DeleteChannelBody,
    pollis_delivery::groups::apply_delete_channel,
    "channels/delete"
);
delivery_b!(
    delivery_members_remove,
    pollis_delivery::groups::RemoveMemberBody,
    pollis_delivery::groups::apply_remove_member,
    "members/remove"
);
delivery_b!(
    delivery_members_role,
    pollis_delivery::groups::SetMemberRoleBody,
    pollis_delivery::groups::apply_set_member_role,
    "members/role"
);
delivery_b!(
    delivery_invites_create,
    pollis_delivery::groups::CreateInviteBody,
    pollis_delivery::groups::apply_create_invite,
    "invites/create"
);
delivery_b!(
    delivery_invites_accept,
    pollis_delivery::groups::AcceptInviteBody,
    pollis_delivery::groups::apply_accept_invite,
    "invites/accept"
);
delivery_b!(
    delivery_invites_decline,
    pollis_delivery::groups::DeclineInviteBody,
    pollis_delivery::groups::apply_decline_invite,
    "invites/decline"
);
delivery_b!(
    delivery_join_requests_create,
    pollis_delivery::groups::CreateJoinRequestBody,
    pollis_delivery::groups::apply_create_join_request,
    "join-requests/create"
);
delivery_b!(
    delivery_join_requests_approve,
    pollis_delivery::groups::ApproveJoinRequestBody,
    pollis_delivery::groups::apply_approve_join_request,
    "join-requests/approve"
);
delivery_b!(
    delivery_join_requests_reject,
    pollis_delivery::groups::RejectJoinRequestBody,
    pollis_delivery::groups::apply_reject_join_request,
    "join-requests/reject"
);
// ─── Domain C (#419) — profile / preferences / blocks / DMs ─────────────────
//
// Every domain-C endpoint runs the SAME pure `apply_*` fn the production axum
// handler runs, against the MAIN DB (these tables are not on the log DB). Auth
// is always enforced here, so the flows suite exercises the signed write +
// server-side authz path end to end.

/// `POST /v1/profile/update`.
async fn delivery_profile_update(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::UpdateProfileBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_update_profile(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("profile/update: {e}")),
    }
}

/// `POST /v1/profile/preferences`.
async fn delivery_profile_preferences(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::SavePreferencesBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_save_preferences(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("profile/preferences: {e}")),
    }
}

/// `POST /v1/blocks/add`.
async fn delivery_blocks_add(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::BlockBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_block_user(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("blocks/add: {e}")),
    }
}

/// `POST /v1/blocks/remove`.
async fn delivery_blocks_remove(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::BlockBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_unblock_user(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("blocks/remove: {e}")),
    }
}

/// `POST /v1/dm/create`.
async fn delivery_dm_create(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/dm/accept`.
async fn delivery_dm_accept(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/dm/add`.
async fn delivery_dm_add(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::AddDmMemberBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_add_dm_member(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("dm/add: {e}")),
    }
}

/// `POST /v1/dm/remove`.
async fn delivery_dm_remove(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::RemoveDmMemberBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_remove_dm_member(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("dm/remove: {e}")),
    }
}

/// `POST /v1/dm/leave`.
async fn delivery_dm_leave(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::profile::LeaveDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::profile::apply_leave_dm(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("dm/leave: {e}")),
    }
}

/// Boot an axum router exposing the DS's full write surface (`/v1/commits` plus
/// the W4–W8 endpoints) backed by the shared `RemoteDb`, bind it on a loopback
/// port, and serve it on a DEDICATED OS thread with its own runtime so the
/// server outlives every individual `#[tokio::test]` runtime.
///
/// Why a dedicated thread: each `#[tokio::test(flavor = "multi_thread")]` spins
/// up and tears down its OWN Tokio runtime. If we spawned the server on the
/// first test's runtime, it would die when that test finished, and every later
/// test would hit a dead port (connection refused). Owning the server on a
/// separate thread + runtime decouples its lifetime from the per-test runtimes,
/// so the single shared DS stays up for the whole `cargo test` process.
// ─── Domain D (#419) — key-packages / device-cert re-sign / push tokens ──────
//
// Every domain-D endpoint runs the SAME pure `apply_*` fn the production axum
// handler runs, against the MAIN DB (these tables are not on the log DB). Auth
// is always enforced here, so the flows suite exercises the signed write +
// owner-scoped authz path end to end. `/v1/key-packages/replenish` runs in EVERY
// test's `sign_up` (via `initialize_identity` → `ensure_mls_key_package`), so a
// regression in the signed key-package path fails the whole suite immediately.

/// `POST /v1/key-packages`.
async fn delivery_key_packages(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/key-packages/replenish`.
async fn delivery_key_packages_replenish(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/devices/resign`.
async fn delivery_devices_resign(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/push-tokens`.
async fn delivery_push_tokens(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::devices::PushTokenBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::devices::apply_register_push_token(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("push-tokens: {e}")),
    }
}

// ─── Domains E + G (#419) — account lifecycle / identity rotation / recovery /
// device-enrollment / security audit ─────────────────────────────────────────
//
// Every endpoint runs the SAME pure `apply_*` fn the production axum handler
// runs, against the MAIN DB (none of these tables is on the log DB). Auth is
// always enforced here, so the flows suite exercises the signed write +
// server-side self-scoped authz (and, for rotate-identity, the account_key_log
// CAS) end to end.

/// Map a domain-E [`pollis_delivery::account::RotateOutcome`] to a Response —
/// 200 (+ new version) / 403 / 409 (CAS loss), mirroring the production handler.
fn ds_rotate_outcome(outcome: pollis_delivery::account::RotateOutcome) -> axum::response::Response {
    use axum::response::IntoResponse;
    use pollis_delivery::account::RotateOutcome;
    match outcome {
        RotateOutcome::Applied { new_version } => (
            axum::http::StatusCode::OK,
            axum::Json(serde_json::json!({ "status": "ok", "identity_version": new_version })),
        )
            .into_response(),
        RotateOutcome::Forbidden => {
            pollis_delivery::error::AuthRejection::Forbidden.into_response()
        }
        RotateOutcome::Conflict { head_version } => (
            axum::http::StatusCode::CONFLICT,
            axum::Json(serde_json::json!({ "status": "conflict", "head_version": head_version })),
        )
            .into_response(),
    }
}

/// `POST /v1/account/rotate-identity`.
async fn delivery_account_rotate_identity(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::RotateIdentityBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_rotate_identity(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_rotate_outcome(o),
        Err(e) => ds_internal_error(format!("account/rotate-identity: {e}")),
    }
}

/// `POST /v1/account/delete`.
async fn delivery_account_delete(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::DeleteAccountBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_delete_account(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("account/delete: {e}")),
    }
}

/// `POST /v1/account/reset-recover`.
async fn delivery_account_reset_recover(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::ResetRecoverBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_reset_recover(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("account/reset-recover: {e}")),
    }
}

/// `POST /v1/security-events`.
async fn delivery_security_events(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/enrollment/approve`.
async fn delivery_enrollment_approve(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::ApproveEnrollmentBody = match serde_json::from_slice(&body)
    {
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

/// `POST /v1/enrollment/reject`.
async fn delivery_enrollment_reject(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::RejectEnrollmentBody = match serde_json::from_slice(&body)
    {
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

/// `POST /v1/devices/revoke`.
async fn delivery_devices_revoke(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::account::RevokeDeviceBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::account::apply_revoke_device(&conn, Some(&authed), &parsed).await {
        Ok(o) => ds_outcome(o),
        Err(e) => ds_internal_error(format!("devices/revoke: {e}")),
    }
}

// ─── Server-side OTP + bootstrap (Goal B #419) ──────────────────────────────
//
// These five run BEFORE the device has an MLS signing key, so they are NOT
// device-signed: request/verify-otp are unauthenticated (the OTP is the proof),
// and the three bootstrap writes are gated by the OTP-session token in
// `X-Pollis-Session`. They drive the SAME `pollis_delivery::{otp,bootstrap}`
// logic the production handlers do (the `process_*` / `apply_*` fns), against the
// SHARED main handle (`state.main`) — the sole writer the clients read from —
// plus the OTP + session stores held on `DsState`. With the client seam flipped,
// every test's `sign_up` exercises this full DS bootstrap path end to end.

/// `POST /v1/auth/request-otp` — unauthenticated; honors DEV_OTP so no email is
/// sent and the fixed harness code verifies.
async fn delivery_request_otp(
    axum::extract::State(state): axum::extract::State<DsState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/auth/verify-otp` — unauthenticated; validates the code, creates or
/// loads the account on the MAIN DB, and mints an OTP-session token.
async fn delivery_verify_otp(
    axum::extract::State(state): axum::extract::State<DsState>,
    body: axum::body::Bytes,
) -> axum::response::Response {
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

/// `POST /v1/auth/establish-identity` — session-gated version-1 identity (CAS).
async fn delivery_establish_identity(
    axum::extract::State(state): axum::extract::State<DsState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let claims = match pollis_delivery::session::verify_session(
        &headers,
        &state.sessions,
        now_u64(),
    ) {
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

/// `POST /v1/auth/register-device` — session-gated device row + watermarks.
async fn delivery_register_device(
    axum::extract::State(state): axum::extract::State<DsState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let claims = match pollis_delivery::session::verify_session(
        &headers,
        &state.sessions,
        now_u64(),
    ) {
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

/// `POST /v1/auth/publish-device-cert` — the PIVOT. DUAL gate, mirroring the
/// production handler: (a) session + cert-validity (first-device signup; session
/// invalidated on success), or (b) cert-validity ALONE with the body `user_id`
/// (subsequent device whose session may have expired during sibling approval).
async fn delivery_publish_device_cert(
    axum::extract::State(state): axum::extract::State<DsState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
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

    // Gate selection: a live session (gate a) wins; else cert-validity-alone with
    // the body user_id (gate b). A present-but-expired token falls through to (b).
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

/// `POST /v1/auth/enrollment-request` — session-gated INSERT of a pending
/// `device_enrollment_request`, user + device bound from the session.
async fn delivery_enrollment_request(
    axum::extract::State(state): axum::extract::State<DsState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let claims = match pollis_delivery::session::verify_session(&headers, &state.sessions, now_u64())
    {
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

/// `POST /v1/auth/request-email-change-otp` — DEVICE-SIGNED. Record the
/// authenticated requester for the new email and prepare+store the OTP (DEV_OTP,
/// no send). Always 200.
async fn delivery_request_email_change_otp(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::email_change::RequestEmailChangeBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let new_email = parsed.new_email.trim().to_string();
    if !new_email.is_empty() {
        state
            .email_change
            .request(&state.otp_config, &authed, &new_email)
            .await;
    }
    ds_ok()
}

/// `POST /v1/auth/verify-email-change` — DEVICE-SIGNED. Validate the OTP +
/// requester binding (authed == requester), then swap `users.email` on the MAIN
/// DB. The authed user comes from the SIGNATURE, never the body.
async fn delivery_verify_email_change(
    axum::extract::State(state): axum::extract::State<DsState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    let authed = match ds_auth(&state.main, &method, &uri, &headers, &body).await {
        Ok(u) => u,
        Err(resp) => return resp,
    };
    let parsed: pollis_delivery::email_change::VerifyEmailChangeBody =
        match serde_json::from_slice(&body) {
            Ok(b) => b,
            Err(_) => return ds_bad_request(),
        };
    let conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::email_change::apply_verify_email_change(
        &conn,
        &state.email_change,
        &state.otp_config,
        &authed,
        &parsed.new_email,
        &parsed.code,
    )
    .await
    {
        Ok(outcome) => pollis_delivery::email_change::email_change_response(outcome),
        Err(e) => ds_internal_error(format!("verify-email-change: {e}")),
    }
}

/// Boot an axum router exposing the DS's full write surface (`/v1/commits` plus
/// the W4–W8 endpoints) backed by the shared `RemoteDb`, bind it on a loopback
/// port, and serve it on a DEDICATED OS thread with its own runtime so the
/// server outlives every individual `#[tokio::test]` runtime.
///
/// Why a dedicated thread: each `#[tokio::test(flavor = "multi_thread")]` spins
/// up and tears down its OWN Tokio runtime. If we spawned the server on the
/// first test's runtime, it would die when that test finished, and every later
/// test would hit a dead port (connection refused). Owning the server on a
/// separate thread + runtime decouples its lifetime from the per-test runtimes,
/// so the single shared DS stays up for the whole `cargo test` process.

async fn spawn_in_process_delivery(main: Arc<RemoteDb>, log: Arc<RemoteDb>) -> String {
    use std::sync::mpsc;

    let state = DsState {
        main,
        log,
        otp: pollis_delivery::otp::OtpStore::default(),
        sessions: pollis_delivery::session::SessionStore::default(),
        // DEV_OTP forces the fixed harness code and skips the email send; no
        // resend throttle so back-to-back sign-ups in one test aren't blocked.
        otp_config: pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
        },
        email_change: pollis_delivery::email_change::EmailChangeStore::default(),
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
                    .route("/v1/commits", axum::routing::post(delivery_submit))
                    .route("/v1/group-info", axum::routing::post(delivery_group_info))
                    .route("/v1/welcomes/ack", axum::routing::post(delivery_welcomes_ack))
                    .route(
                        "/v1/welcomes/reset",
                        axum::routing::post(delivery_welcomes_reset),
                    )
                    .route(
                        "/v1/welcomes/purge",
                        axum::routing::post(delivery_welcomes_purge),
                    )
                    // Domain A (#419) — all on the MAIN DB.
                    .route("/v1/messages/send", axum::routing::post(delivery_messages_send))
                    .route("/v1/messages/edit", axum::routing::post(delivery_messages_edit))
                    .route(
                        "/v1/messages/delete",
                        axum::routing::post(delivery_messages_delete),
                    )
                    .route("/v1/reactions/add", axum::routing::post(delivery_reactions_add))
                    .route(
                        "/v1/reactions/remove",
                        axum::routing::post(delivery_reactions_remove),
                    )
                    .route(
                        "/v1/watermarks/advance",
                        axum::routing::post(delivery_watermarks_advance),
                    )
                    .route("/v1/envelopes/gc", axum::routing::post(delivery_envelopes_gc))
                    .route(
                        "/v1/attachments/register",
                        axum::routing::post(delivery_attachments_register),
                    )
                    .route(
                        "/v1/attachments/delete",
                        axum::routing::post(delivery_attachments_delete),
                    )
                    // Domain B (#419) — groups / channels / membership / invites
                    // / join-requests. All on the MAIN DB.
                    .route("/v1/groups/create", axum::routing::post(delivery_groups_create))
                    .route("/v1/groups/update", axum::routing::post(delivery_groups_update))
                    .route("/v1/groups/delete", axum::routing::post(delivery_groups_delete))
                    .route("/v1/groups/leave", axum::routing::post(delivery_groups_leave))
                    .route(
                        "/v1/channels/create",
                        axum::routing::post(delivery_channels_create),
                    )
                    .route(
                        "/v1/channels/update",
                        axum::routing::post(delivery_channels_update),
                    )
                    .route(
                        "/v1/channels/delete",
                        axum::routing::post(delivery_channels_delete),
                    )
                    .route(
                        "/v1/members/remove",
                        axum::routing::post(delivery_members_remove),
                    )
                    .route("/v1/members/role", axum::routing::post(delivery_members_role))
                    .route(
                        "/v1/invites/create",
                        axum::routing::post(delivery_invites_create),
                    )
                    .route(
                        "/v1/invites/accept",
                        axum::routing::post(delivery_invites_accept),
                    )
                    .route(
                        "/v1/invites/decline",
                        axum::routing::post(delivery_invites_decline),
                    )
                    .route(
                        "/v1/join-requests/create",
                        axum::routing::post(delivery_join_requests_create),
                    )
                    .route(
                        "/v1/join-requests/approve",
                        axum::routing::post(delivery_join_requests_approve),
                    )
                    .route(
                        "/v1/join-requests/reject",
                        axum::routing::post(delivery_join_requests_reject),
                    )
                    // Domain C (#419) — all on the MAIN DB.
                    .route(
                        "/v1/profile/update",
                        axum::routing::post(delivery_profile_update),
                    )
                    .route(
                        "/v1/profile/preferences",
                        axum::routing::post(delivery_profile_preferences),
                    )
                    .route("/v1/blocks/add", axum::routing::post(delivery_blocks_add))
                    .route(
                        "/v1/blocks/remove",
                        axum::routing::post(delivery_blocks_remove),
                    )
                    .route("/v1/dm/create", axum::routing::post(delivery_dm_create))
                    .route("/v1/dm/accept", axum::routing::post(delivery_dm_accept))
                    .route("/v1/dm/add", axum::routing::post(delivery_dm_add))
                    .route("/v1/dm/remove", axum::routing::post(delivery_dm_remove))
                    .route("/v1/dm/leave", axum::routing::post(delivery_dm_leave))
                    // Domain D (#419) — all on the MAIN DB.
                    .route("/v1/key-packages", axum::routing::post(delivery_key_packages))
                    .route(
                        "/v1/key-packages/replenish",
                        axum::routing::post(delivery_key_packages_replenish),
                    )
                    .route("/v1/devices/resign", axum::routing::post(delivery_devices_resign))
                    .route("/v1/push-tokens", axum::routing::post(delivery_push_tokens))
                    // Domains E + G (#419) — all on the MAIN DB.
                    .route(
                        "/v1/account/rotate-identity",
                        axum::routing::post(delivery_account_rotate_identity),
                    )
                    .route("/v1/account/delete", axum::routing::post(delivery_account_delete))
                    .route(
                        "/v1/account/reset-recover",
                        axum::routing::post(delivery_account_reset_recover),
                    )
                    .route(
                        "/v1/security-events",
                        axum::routing::post(delivery_security_events),
                    )
                    .route(
                        "/v1/enrollment/approve",
                        axum::routing::post(delivery_enrollment_approve),
                    )
                    .route(
                        "/v1/enrollment/reject",
                        axum::routing::post(delivery_enrollment_reject),
                    )
                    .route("/v1/devices/revoke", axum::routing::post(delivery_devices_revoke))
                    // Server-side OTP + bootstrap (Goal B #419) — the first-device
                    // signup path the client seam now routes through the DS.
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
                    .route(
                        "/v1/auth/enrollment-request",
                        axum::routing::post(delivery_enrollment_request),
                    )
                    // Email change (Goal B #419 final piece) — device-signed.
                    .route(
                        "/v1/auth/request-email-change-otp",
                        axum::routing::post(delivery_request_email_change_otp),
                    )
                    .route(
                        "/v1/auth/verify-email-change",
                        axum::routing::post(delivery_verify_email_change),
                    )
                    .with_state(state);
                // Bind :0 so the OS hands us a free port; read it back before serving.
                let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
                    .await
                    .expect("delivery: bind loopback");
                let addr = listener.local_addr().expect("delivery: local_addr");
                tx.send(format!("http://{addr}")).expect("delivery: send url");
                if let Err(e) = axum::serve(listener, router).await {
                    eprintln!("[harness] in-process delivery service exited: {e}");
                }
            });
        })
        .expect("delivery: spawn thread");

    rx.recv().expect("delivery: receive url")
}

pub(crate) async fn wipe() {
    let w = world().await;
    wipe_remote(&w.remote, &w.log)
        .await
        .expect("wipe test turso");
}

/// The in-process Delivery Service base URL (e.g. `http://127.0.0.1:NNNNN`).
pub(crate) async fn delivery_url() -> String {
    world()
        .await
        .config
        .pollis_delivery_url
        .clone()
        .expect("in-process delivery URL configured")
}

/// Minimal, dependency-free HTTP/1.1 `POST` used only by the sole-writer
/// acceptance test: send `body` to `{base}{path}` with the given extra headers
/// and `Connection: close`, then return the numeric HTTP status code. We craft
/// the request over a raw `TcpStream` rather than pull an HTTP-client crate into
/// the test deps just to prove the DS rejects unsigned writes.
pub(crate) async fn raw_post_status(
    base: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let addr = base.trim_start_matches("http://");
    let mut stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("connect to in-process DS");
    let mut req = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(k);
        req.push_str(": ");
        req.push_str(v);
        req.push_str("\r\n");
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).await.expect("write head");
    stream.write_all(body).await.expect("write body");
    stream.flush().await.expect("flush request");
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).await.expect("read response");
    let text = String::from_utf8_lossy(&resp);
    let status_line = text.lines().next().expect("HTTP status line");
    // "HTTP/1.1 401 Unauthorized" → 401
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("could not parse status from: {status_line:?}"))
}

/// Sign `body` with `client`'s stable device key — byte-for-byte as
/// `ds_client::ds_post` does — and POST it to `{delivery_url}{path}`, returning
/// the HTTP status. This is the ONLY way to drive a *validly signed* request as
/// an arbitrary user, which is what the domain-A authorization tests need: a
/// non-member / non-sender must get 403 (proved identity, lacking permission),
/// not the 401 an unsigned/garbled request gets.
pub(crate) async fn signed_post_status(client: &TestClient, path: &str, body: &[u8]) -> u16 {
    use openmls_traits::signatures::Signer;

    let base = delivery_url().await;
    let user_id = client.user_id().to_string();
    let device_id = client
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("client device_id set");
    let timestamp = pollis_delivery::auth::now_unix();
    let message = pollis_delivery::auth::canonical_message("POST", path, timestamp, body);

    let signature_b64 = {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("client local db open");
        let provider = pollis_lib::commands::mls::PollisProvider::new(db.conn());
        let (signer, _pub) =
            pollis_lib::commands::mls::load_or_create_device_signer(&provider, &user_id, &device_id)
                .expect("load device signer");
        let sig = signer.sign(&message).expect("sign request");
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(sig)
    };

    let ts_str = timestamp.to_string();
    let headers = [
        ("X-Pollis-User", user_id.as_str()),
        ("X-Pollis-Device", device_id.as_str()),
        ("X-Pollis-Timestamp", ts_str.as_str()),
        ("X-Pollis-Signature", signature_b64.as_str()),
    ];
    raw_post_status(&base, path, &headers, body).await
}

// ─── Client ─────────────────────────────────────────────────────────────────

/// One simulated device for one user. Holds a `MockRuntime` app with its own
/// isolated keystore + managed `AppState`. All clients in a given test share
/// the `Arc<RemoteDb>` on `TestWorld`, so they actually round-trip through
/// the same test Turso DB the way real clients round-trip through production
/// Turso.
pub(crate) struct TestClient {
    /// App must outlive the webview — keep it alive for the lifetime of the
    /// client.
    pub(crate) _app: App<MockRuntime>,
    pub(crate) webview: WebviewWindow<MockRuntime>,
    #[allow(dead_code)]
    pub(crate) state: Arc<AppState>,
    /// Populated after `sign_up` / `sign_in`. Commands like `create_group`
    /// need this to identify the caller.
    pub(crate) profile: Option<UserProfile>,
}

impl TestClient {
    /// Build a fresh client. Does NOT sign in — call [`sign_up`] after
    /// construction.
    pub(crate) async fn new() -> Self {
        let w = world().await;
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(
            w.config.clone(),
            // Main DB.
            w.remote.clone(),
            // Commit-log DB — a genuinely separate libsql file, so a query that
            // should hit one DB but is routed to the other fails loudly.
            w.log.clone(),
            keystore,
        ));
        let (app, webview) = build_client_app(state.clone()).expect("build client app");
        Self {
            _app: app,
            webview,
            state,
            profile: None,
        }
    }

    /// First-device signup via the real OTP flow (bypassed by `DEV_OTP`).
    /// Populates `self.profile`, sets a fixed test PIN ("0000") so the
    /// local SQLCipher DB opens, and runs `initialize_identity` to
    /// publish the device's MLS key package. Returns the final profile.
    ///
    /// PIN is required: post-#194, `verify_otp` deliberately leaves the
    /// local DB closed; `set_pin` is what calls `load_user_db_with_key`.
    /// Skipping it would make every DB-touching command in the test
    /// harness fail with "Not signed in".
    pub(crate) async fn sign_up(&mut self, email: &str) -> UserProfile {
        invoke::<()>(&self.webview, "request_otp", json!({ "email": email }))
            .await
            .unwrap_or_else(|e| panic!("request_otp({email}): {e}"));

        let profile: UserProfile = invoke(
            &self.webview,
            "verify_otp",
            json!({ "email": email, "code": DEV_OTP }),
        )
        .await
        .unwrap_or_else(|e| panic!("verify_otp({email}): {e}"));

        invoke::<()>(
            &self.webview,
            "set_pin",
            json!({ "newPin": TEST_PIN, "oldPin": null }),
        )
        .await
        .unwrap_or_else(|e| panic!("set_pin({TEST_PIN}): {e}"));

        invoke::<serde_json::Value>(
            &self.webview,
            "initialize_identity",
            json!({ "userId": profile.id }),
        )
        .await
        .unwrap_or_else(|e| panic!("initialize_identity: {e}"));

        self.profile = Some(profile.clone());
        profile
    }

    pub(crate) fn user_id(&self) -> &str {
        &self.profile.as_ref().expect("not signed in").id
    }

    /// Point the process-global "active user" (`accounts.json`'s
    /// `last_active_user`) at THIS client before it dispatches a command.
    ///
    /// Production runs exactly one active user per process, and the signed DS
    /// write client (`ds_client::ds_post`) derives the `X-Pollis-User` signing
    /// identity from that global. The harness runs many users in one process
    /// sharing one `accounts.json`, so without this every signed DS write would
    /// be attributed to whichever client signed up LAST and fail auth (401/403).
    /// Tests are `#[serial]` and each command is fully awaited before the next,
    /// so flipping the active user immediately before dispatch is race-free.
    fn activate(&self) {
        if let Some(p) = &self.profile {
            let _ = pollis_lib::accounts::upsert_account(&p.id, &p.username, None, None);
        }
    }

    pub(crate) async fn invoke_json(&self, cmd: &str, args: serde_json::Value) -> serde_json::Value {
        self.activate();
        invoke(&self.webview, cmd, args)
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }

    /// Like [`invoke_json`] but returns the `Result` so a test can assert on a
    /// command that is EXPECTED to fail (e.g. a rejected DS write). Still
    /// activates this client first so signed DS writes go out under its identity.
    pub(crate) async fn invoke_try(
        &self,
        cmd: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.activate();
        invoke(&self.webview, cmd, args).await
    }
}

// ─── Helpers for multi-client scenarios ─────────────────────────────────────

impl TestClient {
    /// Drain any pending MLS welcomes queued for this client on Turso. Real
    /// clients call this on login and when the livekit inbox pings them; the
    /// harness drives it explicitly. Scoped to welcomes only — commits are
    /// per-channel (see [`process_commits_for`]).
    pub(crate) async fn poll(&self) {
        self.activate();
        let _: serde_json::Value = invoke(
            &self.webview,
            "poll_mls_welcomes",
            json!({ "userId": self.user_id() }),
        )
        .await
        .unwrap_or_else(|e| panic!("poll_mls_welcomes: {e}"));
    }

    /// Drain pending MLS commits for a single channel. Must be called
    /// per-channel because commit processing is keyed by MLS group, which
    /// corresponds 1:1 with a conversation (channel or DM).
    #[allow(dead_code)]
    pub(crate) async fn process_commits_for(&self, channel_id: &str) {
        self.activate();
        let _: serde_json::Value = invoke(
            &self.webview,
            "process_pending_commits",
            json!({ "conversationId": channel_id, "userId": self.user_id() }),
        )
        .await
        .unwrap_or_else(|e| panic!("process_pending_commits({channel_id}): {e}"));
    }

    pub(crate) async fn create_group(&self, name: &str) -> String {
        // Tests expect the auto-created #General text channel; opt in
        // explicitly because the production frontend now defaults both
        // toggles to off.
        let g: serde_json::Value = self
            .invoke_json(
                "create_group",
                json!({
                    "name": name,
                    "description": null,
                    "ownerId": self.user_id(),
                    "createDefaultTextChannel": true,
                    "createDefaultVoiceChannel": true,
                }),
            )
            .await;
        g["id"].as_str().expect("group id").to_string()
    }

    pub(crate) async fn invite(&self, group_id: &str, invitee_identifier: &str) {
        let _: serde_json::Value = self
            .invoke_json(
                "send_group_invite",
                json!({
                    "groupId": group_id,
                    "inviterId": self.user_id(),
                    "inviteeIdentifier": invitee_identifier,
                }),
            )
            .await;
    }

    /// Fetch this client's pending invites and return the first one, if any.
    pub(crate) async fn first_pending_invite(&self) -> Option<serde_json::Value> {
        let invites: serde_json::Value = self
            .invoke_json("get_pending_invites", json!({ "userId": self.user_id() }))
            .await;
        invites
            .as_array()
            .and_then(|arr| arr.first().cloned())
    }

    pub(crate) async fn accept_invite(&self, invite_id: &str) {
        self.invoke_json(
            "accept_group_invite",
            json!({ "inviteId": invite_id, "userId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn decline_invite(&self, invite_id: &str) {
        self.invoke_json(
            "decline_group_invite",
            json!({ "inviteId": invite_id, "userId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn list_group_ids(&self) -> Vec<String> {
        let groups: serde_json::Value = self
            .invoke_json("list_user_groups", json!({ "userId": self.user_id() }))
            .await;
        groups
            .as_array()
            .expect("groups array")
            .iter()
            .map(|g| g["id"].as_str().expect("group id").to_string())
            .collect()
    }

    pub(crate) async fn group_member_ids(&self, group_id: &str) -> Vec<String> {
        let members: serde_json::Value = self
            .invoke_json("get_group_members", json!({ "groupId": group_id }))
            .await;
        members
            .as_array()
            .expect("members array")
            .iter()
            .map(|m| m["user_id"].as_str().expect("user_id").to_string())
            .collect()
    }

    pub(crate) async fn list_group_channels(&self, group_id: &str) -> Vec<serde_json::Value> {
        let channels: serde_json::Value = self
            .invoke_json("list_group_channels", json!({ "groupId": group_id }))
            .await;
        channels.as_array().expect("channels array").clone()
    }

    /// Return the #General text channel ID for a group.
    pub(crate) async fn general_channel_id(&self, group_id: &str) -> String {
        self.list_group_channels(group_id)
            .await
            .into_iter()
            .find(|c| c["channel_type"] == "text")
            .expect("a text channel")["id"]
            .as_str()
            .expect("channel id")
            .to_string()
    }

    pub(crate) async fn request_group_access(&self, group_id: &str) {
        self.invoke_json(
            "request_group_access",
            json!({ "groupId": group_id, "requesterId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn list_join_requests(&self, group_id: &str) -> Vec<serde_json::Value> {
        let reqs: serde_json::Value = self
            .invoke_json(
                "get_group_join_requests",
                json!({ "groupId": group_id, "requesterId": self.user_id() }),
            )
            .await;
        reqs.as_array().expect("requests array").clone()
    }

    pub(crate) async fn approve_join_request(&self, request_id: &str) {
        self.invoke_json(
            "approve_join_request",
            json!({ "requestId": request_id, "approverId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn reject_join_request(&self, request_id: &str) {
        self.invoke_json(
            "reject_join_request",
            json!({ "requestId": request_id, "approverId": self.user_id() }),
        )
        .await;
    }

    /// Voluntarily leave a group. Deletes this user's `group_member` row,
    /// forgets local MLS state, and signals remaining members to reconcile
    /// away the leaver's stale leaf.
    pub(crate) async fn leave_group(&self, group_id: &str) {
        self.invoke_json(
            "leave_group",
            json!({ "groupId": group_id, "userId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn remove_member(&self, group_id: &str, target_user_id: &str) {
        self.invoke_json(
            "remove_member_from_group",
            json!({
                "groupId": group_id,
                "userId": target_user_id,
                "requesterId": self.user_id(),
            }),
        )
        .await;
    }

    pub(crate) async fn create_dm(&self, other_user_ids: &[&str]) -> String {
        let members: Vec<&str> = other_user_ids.to_vec();
        let dm: serde_json::Value = self
            .invoke_json(
                "create_dm_channel",
                json!({ "creatorId": self.user_id(), "memberIds": members }),
            )
            .await;
        dm["id"].as_str().expect("dm id").to_string()
    }

    pub(crate) async fn list_dm_requests(&self) -> Vec<serde_json::Value> {
        let dms: serde_json::Value = self
            .invoke_json("list_dm_requests", json!({ "userId": self.user_id() }))
            .await;
        dms.as_array().expect("dm requests array").clone()
    }

    pub(crate) async fn list_dms(&self) -> Vec<serde_json::Value> {
        let dms: serde_json::Value = self
            .invoke_json("list_dm_channels", json!({ "userId": self.user_id() }))
            .await;
        dms.as_array().expect("dm channels array").clone()
    }

    pub(crate) async fn accept_dm_request(&self, dm_channel_id: &str) {
        self.invoke_json(
            "accept_dm_request",
            json!({ "dmChannelId": dm_channel_id, "userId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn block(&self, blocked_user_id: &str) {
        self.invoke_json(
            "block_user",
            json!({ "blockerId": self.user_id(), "blockedId": blocked_user_id }),
        )
        .await;
    }

    /// Try to invoke `send_message`, returning the error string if it failed.
    pub(crate) async fn try_send_message(
        &self,
        conversation_id: &str,
        content: &str,
    ) -> Result<serde_json::Value, String> {
        self.activate();
        invoke(
            &self.webview,
            "send_message",
            json!({
                "conversationId": conversation_id,
                "senderId": self.user_id(),
                "content": content,
                "replyToId": null,
                "senderUsername": self.profile.as_ref().map(|p| p.username.clone()),
            }),
        )
        .await
    }

    pub(crate) async fn send_channel_message(&self, conversation_id: &str, content: &str) {
        self.try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
    }

    /// Like `send_channel_message` but returns the new message's ID so the
    /// caller can later edit or delete it.
    pub(crate) async fn send_channel_message_id(&self, conversation_id: &str, content: &str) -> String {
        let msg = self
            .try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
        msg["id"].as_str().expect("message id").to_string()
    }

    /// Edit a previously-sent message. Republishes the ciphertext at the
    /// current MLS epoch and replaces any prior edit envelope.
    pub(crate) async fn edit_message(&self, conversation_id: &str, message_id: &str, new_content: &str) {
        self.invoke_json(
            "edit_message",
            json!({
                "conversationId": conversation_id,
                "messageId": message_id,
                "userId": self.user_id(),
                "newContent": new_content,
            }),
        )
        .await;
    }

    pub(crate) async fn fetch_channel_messages(&self, channel_id: &str) -> Vec<serde_json::Value> {
        let page: serde_json::Value = self
            .invoke_json(
                "get_channel_messages",
                json!({ "userId": self.user_id(), "channelId": channel_id, "limit": 50 }),
            )
            .await;
        page["messages"]
            .as_array()
            .expect("messages array")
            .clone()
    }

    /// Change a member's role in a group (`"admin"` or `"member"`).
    pub(crate) async fn set_member_role(&self, group_id: &str, target_user_id: &str, role: &str) {
        self.invoke_json(
            "set_member_role",
            json!({
                "groupId": group_id,
                "userId": target_user_id,
                "role": role,
                "requesterId": self.user_id(),
            }),
        )
        .await;
    }

    /// Return the (user_id, role) pairs for every current member of a group.
    pub(crate) async fn group_member_roles(&self, group_id: &str) -> Vec<(String, String)> {
        let members: serde_json::Value = self
            .invoke_json("get_group_members", json!({ "groupId": group_id }))
            .await;
        members
            .as_array()
            .expect("members array")
            .iter()
            .map(|m| (
                m["user_id"].as_str().expect("user_id").to_string(),
                m["role"].as_str().expect("role").to_string(),
            ))
            .collect()
    }

}

// ─── Multi-device helpers ───────────────────────────────────────────────────

impl TestClient {
    /// Fetch a DM channel page through the real `get_dm_messages` command.
    /// Mirrors `fetch_channel_messages` but drives the DM code path, which
    /// polls welcomes and processes commits before decrypting.
    pub(crate) async fn fetch_dm_messages(&self, dm_channel_id: &str) -> Vec<serde_json::Value> {
        let page: serde_json::Value = self
            .invoke_json(
                "get_dm_messages",
                json!({ "userId": self.user_id(), "dmChannelId": dm_channel_id, "limit": 50 }),
            )
            .await;
        page["messages"]
            .as_array()
            .expect("messages array")
            .clone()
    }

    /// Leave a DM. Used both as "reject pending request" (when the user hasn't
    /// accepted yet) and "leave accepted channel" — the row is deleted either
    /// way, so both flows go through this single command.
    pub(crate) async fn leave_dm(&self, dm_channel_id: &str) {
        self.invoke_json(
            "leave_dm_channel",
            json!({ "dmChannelId": dm_channel_id, "userId": self.user_id() }),
        )
        .await;
    }

    pub(crate) async fn unblock(&self, blocked_user_id: &str) {
        self.invoke_json(
            "unblock_user",
            json!({ "blockerId": self.user_id(), "blockedId": blocked_user_id }),
        )
        .await;
    }
}

/// Spin up a new `TestClient` and enroll it as a second device for an
/// existing user. Drives the real `device_enrollment` command chain end to
/// end — `start_device_enrollment` → `list_pending_enrollment_requests` →
/// `approve_device_enrollment` → `poll_enrollment_status` — so the returned
/// client holds a valid local copy of `account_id_key`, has published its
/// own device cert + MLS key packages, and can participate in MLS groups
/// immediately.
///
/// `primary` must already be signed in as the target user.
pub(crate) async fn enroll_second_device(primary: &TestClient, email: &str) -> TestClient {
    // 1. Build a fresh client. Unlike `TestClient::new` → `sign_up`, we sign
    //    in against the email of an existing user, so `verify_otp` finds the
    //    user row and returns enrollment_required = true (instead of minting
    //    a new account).
    let mut new_client = TestClient::new().await;

    invoke::<()>(
        &new_client.webview,
        "request_otp",
        json!({ "email": email }),
    )
    .await
    .unwrap_or_else(|e| panic!("request_otp({email}) on new device: {e}"));

    let profile: UserProfile = invoke(
        &new_client.webview,
        "verify_otp",
        json!({ "email": email, "code": DEV_OTP }),
    )
    .await
    .unwrap_or_else(|e| panic!("verify_otp({email}) on new device: {e}"));

    assert_eq!(
        profile.id,
        primary.user_id(),
        "second device verify_otp should resolve to the primary's user_id"
    );
    assert!(
        profile.enrollment_required,
        "second device must see enrollment_required=true"
    );

    new_client.profile = Some(profile.clone());

    // 2. New device kicks off an enrollment request — ephemeral X25519 pub
    //    lands on Turso, the private half stays in AppState.
    let handle: serde_json::Value = new_client
        .invoke_json(
            "start_device_enrollment",
            json!({ "userId": profile.id }),
        )
        .await;
    let request_id = handle["request_id"]
        .as_str()
        .expect("request_id")
        .to_string();
    let verification_code = handle["verification_code"]
        .as_str()
        .expect("verification_code")
        .to_string();

    // 3. Primary sees the pending request and approves it. The approver
    //    wraps account_id_key under the requester's ephemeral pub and
    //    flips the row to 'approved'.
    let pending: serde_json::Value = primary
        .invoke_json(
            "list_pending_enrollment_requests",
            json!({ "userId": profile.id }),
        )
        .await;
    let pending_arr = pending.as_array().expect("pending array");
    let matching = pending_arr
        .iter()
        .find(|r| r["request_id"].as_str() == Some(request_id.as_str()))
        .unwrap_or_else(|| {
            panic!(
                "primary did not see pending enrollment request {request_id}; \
                 got {pending_arr:#?}"
            )
        });
    assert_eq!(
        matching["verification_code"].as_str(),
        Some(verification_code.as_str()),
        "verification code should match between devices"
    );

    primary
        .invoke_json(
            "approve_device_enrollment",
            json!({
                "requestId": request_id,
                "verificationCode": verification_code,
            }),
        )
        .await;

    // 4. New device polls until approved. Bounded loop — 20 iterations is
    //    orders of magnitude more than needed because the approve write
    //    above has already committed to Turso by the time we get here. No
    //    raw sleeps.
    let mut status: String = String::new();
    for attempt in 0..20 {
        let resp: serde_json::Value = new_client
            .invoke_json(
                "poll_enrollment_status",
                json!({ "requestId": request_id }),
            )
            .await;
        status = resp["status"]
            .as_str()
            .unwrap_or("(missing status)")
            .to_string();
        if status == "approved" {
            break;
        }
        if status == "rejected" || status == "expired" {
            panic!("enrollment terminal status before approval: {status}");
        }
        if attempt == 19 {
            panic!("enrollment never approved; last status={status}");
        }
    }
    assert_eq!(status, "approved", "enrollment should end in 'approved'");

    // 5. Post-#194: poll_enrollment_status now hands the unwrapped
    //    account_id_key to AppState.unlock instead of writing it raw,
    //    and defers finalize_enrollment. The test must mirror what
    //    App.tsx does after pin-create completes:
    //       set_pin → finalize_device_enrollment → initialize_identity.
    invoke::<()>(
        &new_client.webview,
        "set_pin",
        json!({ "newPin": TEST_PIN, "oldPin": null }),
    )
    .await
    .unwrap_or_else(|e| panic!("set_pin on enrolled device: {e}"));

    invoke::<()>(
        &new_client.webview,
        "finalize_device_enrollment",
        json!({ "userId": profile.id }),
    )
    .await
    .unwrap_or_else(|e| panic!("finalize_device_enrollment: {e}"));

    invoke::<serde_json::Value>(
        &new_client.webview,
        "initialize_identity",
        json!({ "userId": profile.id }),
    )
    .await
    .unwrap_or_else(|e| panic!("initialize_identity on enrolled device: {e}"));

    // 6. Sanity: remote now lists two devices for this user.
    let devices: serde_json::Value = new_client
        .invoke_json(
            "list_user_devices",
            json!({ "userId": profile.id }),
        )
        .await;
    assert_eq!(
        devices.as_array().map(|a| a.len()).unwrap_or(0),
        2,
        "user should have exactly two registered devices after enrollment, got {devices:?}"
    );

    new_client
}
