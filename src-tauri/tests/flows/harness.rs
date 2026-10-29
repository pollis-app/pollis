//! End-to-end integration harness for Pollis.
//!
//! Clients hold no DB handle (#987); [`writable_remote`] / [`writable_log`] are the DS's, for server-side state only.
//!
//! Run with:
//! ```
//! cargo test --features test-harness --test flows
//! ```
//!
//! No shared Turso: an intermittent failure here is a real race, never DB contention (#832).

use std::sync::Arc;

use pollis_lib::commands::auth::UserProfile;
use pollis_lib::config::Config;
use pollis_delivery::db::Db;
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
/// Fixed PIN so every harness client has its DB open after signup.
pub(crate) const TEST_PIN: &str = "0000";

/// Process-wide shared backend; tests run serially or the wipe would race.
pub(crate) struct TestWorld {
    /// Main DB, including the `user_device` auth lookups.
    pub(crate) remote: Arc<Db>,
    /// Commit-log DB, a separate file so a misrouted query fails with "no such table" (#420).
    pub(crate) log: Arc<Db>,
    pub(crate) config: Config,
}

static WORLD: OnceCell<TestWorld> = OnceCell::const_new();

pub(crate) async fn world() -> &'static TestWorld {
    WORLD
        .get_or_init(|| async {
            let mut config = Config::for_test().expect("Config::for_test");

            // Process-unique, so stale DB files can't leak between runs.
            let tmp = tempfile::tempdir().expect("tempdir");
            // Kept for the process; dropping it mid-test would delete open DBs.
            let path = tmp.keep();
            // Not `std::env::set_var`: mutating the env with other threads running is a data race (#923).
            pollis_core::db::local::set_data_dir(&path);

            let remote_db_path = path.join("test_turso.db");
            let remote = Arc::new(
                Db::connect_local(remote_db_path.to_str().expect("utf-8 path"))
                    .await
                    .expect("connect local libsql"),
            );

            bootstrap_schema(&remote.conn().await.expect("main conn"))
                .await
                .expect("bootstrap test turso schema");

            // A second file, so a misrouted control-plane query fails loudly.
            let log_db_path = path.join("test_log.db");
            let log = Arc::new(
                Db::connect_local(log_db_path.to_str().expect("utf-8 path"))
                    .await
                    .expect("connect local log libsql"),
            );
            bootstrap_log_schema(&log.conn().await.expect("log conn"))
                .await
                .expect("bootstrap log db schema");
            // Drop the baseline's MLS tables from main so stray access fails loudly.
            drop_log_tables_from_main(&remote.conn().await.expect("main conn"))
                .await
                .expect("drop log tables from main");

            // The real DS gets the harness's own handles: two `Database`s on one file don't share WAL writes promptly.
            let delivery_url = spawn_in_process_delivery(remote.clone(), log.clone()).await;
            config.pollis_delivery_url = Some(delivery_url);

            TestWorld { remote, log, config }
        })
        .await
}

/// One-shot fault menu for the in-process Delivery Service.
/// SAFETY / SCOPE: every fault lives in the HARNESS layer (this file), NEVER in
/// `pollis-delivery`. Production commit semantics stay untouched — the real
/// client code path runs against a DS that behaves exactly as prod plus the one
/// injected perturbation. `DropWelcome` is realized by clearing the Welcomes off
/// the parsed body *before* `submit_commit` (so they're simply never persisted),
/// not by patching the delivery crate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DsFault {
    /// Lands, then the success response becomes a 500 (#411). The client must adopt its commit.
    DropResponse,
    /// Same mechanism as [`DsFault::DropResponse`]; recovery is `our_commit_is_canonical`.
    Fail500PostWrite,
    /// 500 with nothing persisted; a resubmit succeeds.
    Fail500PreWrite,
    /// Commit + GroupInfo land but this add's Welcomes don't; recovery is external-join.
    DropWelcome,

    // Message path (#1524). A "timeout" is a 504: the client sets no timeout of its own.
    /// A send or edit lands, then the answer is lost.
    SendLandsThenTimesOut,
    /// A message send or edit times out before the DS stores anything.
    SendTimesOutUnlanded,
    /// The envelope list arrives, then the commit fetch times out.
    CatchUpTimesOutMidway,
    /// Everything decrypts, then the cursor report times out.
    WatermarkTimesOut,
}

impl DsFault {
    pub(crate) const MESSAGE_PATH: [DsFault; 4] = [
        DsFault::SendLandsThenTimesOut,
        DsFault::SendTimesOutUnlanded,
        DsFault::CatchUpTimesOutMidway,
        DsFault::WatermarkTimesOut,
    ];
}

/// The armed one-shot fault; only locked across non-await sections.
pub(crate) static NEXT_DS_FAULT: std::sync::Mutex<Option<DsFault>> =
    std::sync::Mutex::new(None);

/// One-shot: consumed the first time it applies.
pub(crate) fn arm_ds_fault(fault: DsFault) {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned") = Some(fault);
}

/// Force-disarm; the fuzzer calls it per case so nothing bleeds across cases.
pub(crate) fn clear_ds_fault() {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned") = None;
}

fn peek_ds_fault() -> Option<DsFault> {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned")
}

fn take_ds_fault() -> Option<DsFault> {
    NEXT_DS_FAULT
        .lock()
        .expect("NEXT_DS_FAULT poisoned")
        .take()
}

/// True iff a fault is still armed, to prove it fired exactly once.
pub(crate) fn ds_fault_armed() -> bool {
    peek_ds_fault().is_some()
}

/// State for the one harness-served endpoint, the fault-injecting `/v1/commits` override.
#[derive(Clone)]
struct DsState {
    main: Arc<Db>,
    log: Arc<Db>,
}

/// Verify the device signature over the RAW body against MAIN; auth is always on.
async fn ds_auth(
    remote: &Db,
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
        pollis_delivery::util::now_unix() as i64,
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

/// `/v1/commits`, mirroring the real handler plus harness-only fault injection.
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

    let mut parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };

    if parsed.sender_id != authed {
        return pollis_delivery::error::AuthRejection::Forbidden.into_response();
    }

    // Pre-write faults: peek, then take only where the fault applies.
    match peek_ds_fault() {
        Some(DsFault::Fail500PreWrite) => {
            take_ds_fault();
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "simulated pre-write failure (test fault injection)",
            )
                .into_response();
        }
        Some(DsFault::DropWelcome) => {
            take_ds_fault();
            // Cleared Welcomes are never written, so the new member must external-join.
            parsed.welcomes.clear();
        }
        _ => {}
    }

    // The log is on LOG; Welcome membership checks are on MAIN.
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    let main_conn = match state.main.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::commit::submit_commit(&main_conn, &conn, &parsed).await {
        Ok(pollis_delivery::commit::SubmitVerdict::Forbidden) => {
            pollis_delivery::error::AuthRejection::Forbidden.into_response()
        }
        Ok(pollis_delivery::commit::SubmitVerdict::Invalid(why)) => (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(serde_json::json!({ "error": why })),
        )
            .into_response(),
        Ok(pollis_delivery::commit::SubmitVerdict::Response(outcome)) => {
            // Post-write faults: everything landed; turn success into a 500 so the client must adopt.
            if matches!(outcome, SubmitResponse::Accepted { .. })
                && matches!(
                    peek_ds_fault(),
                    Some(DsFault::DropResponse) | Some(DsFault::Fail500PostWrite)
                )
            {
                take_ds_fault();
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

/// The in-process DS's device-key cache, captured when the DS boots.
static DS_DEVICE_KEYS: std::sync::OnceLock<pollis_delivery::auth::DeviceKeyCache> =
    std::sync::OnceLock::new();

/// Evict `user_id`'s cached device keys from the DS instead of sleeping out the TTL.
pub(crate) fn expire_ds_device_keys(user_id: &str) {
    DS_DEVICE_KEYS
        .get()
        .expect("the in-process DS has not booted yet")
        .invalidate_user(user_id);
}

/// Forget the DS's `last_seen` throttle for `user_id`.
pub(crate) fn expire_ds_last_seen_throttle(user_id: &str) {
    DS_DEVICE_KEYS
        .get()
        .expect("the in-process DS has not booted yet")
        .last_seen()
        .forget_user(user_id);
}

async fn spawn_in_process_delivery(main: Arc<Db>, log: Arc<Db>) -> String {
    use std::sync::mpsc;

    // The harness's own handles: a second `Database` on one file doesn't see WAL writes promptly.
    let ds_main = Arc::clone(&main);
    let ds_log = Arc::clone(&log);

    // Auth is ENFORCED; no-auth mode is covered by `pollis-delivery` unit tests.
    let real_state = pollis_delivery::AppState::new_with_log_db(ds_main, ds_log, true)
        // Fixed DEV_OTP, no email send, no resend throttle.
        .with_otp_config(pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
            // A handful of fixed mailboxes; raised, not disabled (OTP unit tests pin it).
            max_sends_per_window: u32::MAX,
            ..pollis_delivery::otp::OtpConfig::default()
        })
        // Everything comes from 127.0.0.1; raised, not disabled (`ratelimit.rs` pins it).
        .with_ratelimit_config(pollis_delivery::ratelimit::RateLimitConfig {
            request_otp_max: 1_000_000,
            request_otp_window_secs: 1,
            verify_otp_max: 1_000_000,
            verify_otp_window_secs: 1,
            write_max: 1_000_000,
            write_window_secs: 1,
            invite_redeem_max: 1_000_000,
            invite_redeem_window_secs: 1,
            read_max: 1_000_000,
            read_window_secs: 1,
            probe_max: 1_000_000,
            probe_window_secs: 1,
            get_max: 1_000_000,
            get_window_secs: 1,
        });

    // The very map the auth gate consults, for `expire_ds_device_keys`.
    let _ = DS_DEVICE_KEYS.set(real_state.device_keys.clone());

    let fault_state = DsState { main, log };
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("in-process-delivery".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("delivery: build runtime");
            rt.block_on(async move {
                // The production router with `/v1/commits` overridden; `fallback_service` avoids axum's duplicate-route panic.
                let router = axum::Router::new()
                    .route(
                        <pollis_delivery::commit::SubmitBody as pollis_api::DsRequest>::PATH,
                        axum::routing::post(delivery_submit),
                    )
                    .with_state(fault_state)
                    .fallback_service(pollis_delivery::build_router_with_state(real_state))
                    .layer(axum::middleware::from_fn(record_send_data))
                    .layer(axum::middleware::from_fn(inject_message_fault));
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

/// One `POST /v1/livekit/send-data` wake-up as the in-process DS saw it.
#[derive(Clone, Debug)]
pub(crate) struct SentWakeUp {
    pub(crate) room: String,
    pub(crate) payload: serde_json::Value,
    /// No LiveKit here: accepted = 503 "not configured", refused = 4xx.
    pub(crate) status: u16,
}

static SENT_WAKE_UPS: std::sync::Mutex<Vec<SentWakeUp>> = std::sync::Mutex::new(Vec::new());

/// Log every `send-data` request; the real handler still decides it.
async fn record_send_data(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = <pollis_api::broker::LivekitSendDataBody as pollis_api::DsRequest>::PATH;
    let send_path = <pollis_api::messages::SendMessageBody as pollis_api::DsRequest>::PATH;
    if req.uri().path() == send_path {
        return record_push_audience(req, next).await;
    }
    if req.uri().path() != path {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => return ds_internal_error(format!("send-data body: {e}")),
    };
    let parsed: Option<pollis_api::broker::LivekitSendDataBody> = serde_json::from_slice(&bytes).ok();
    let resp = next
        .run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await;
    if let Some(p) = parsed {
        SENT_WAKE_UPS.lock().expect("wake-up log").push(SentWakeUp {
            room: p.room,
            payload: p.payload,
            status: resp.status().as_u16(),
        });
    }
    resp
}

/// Apply an armed message-path fault (#1524), consumed only when it applies.
async fn inject_message_fault(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    use pollis_api::DsRequest;
    let path = req.uri().path();
    let is_post = path == pollis_api::messages::SendMessageBody::PATH
        || path == pollis_api::messages::EditMessageBody::PATH;
    let timeout = || {
        (
            axum::http::StatusCode::GATEWAY_TIMEOUT,
            "simulated timeout (test fault injection)",
        )
            .into_response()
    };
    match peek_ds_fault() {
        Some(DsFault::SendLandsThenTimesOut) if is_post => {
            let resp = next.run(req).await;
            if resp.status().is_success() && take_ds_fault().is_some() {
                return timeout();
            }
            resp
        }
        Some(DsFault::SendTimesOutUnlanded) if is_post => {
            take_ds_fault();
            timeout()
        }
        Some(DsFault::CatchUpTimesOutMidway)
            if path == pollis_api::reads::ConversationStateBody::PATH =>
        {
            take_ds_fault();
            timeout()
        }
        Some(DsFault::WatermarkTimesOut) if path == pollis_api::messages::WatermarkBody::PATH => {
            take_ds_fault();
            timeout()
        }
        _ => next.run(req).await,
    }
}

/// The push audience one send asked for (#1431), as sent.
#[derive(Clone, Debug)]
pub(crate) struct SentPushAudience {
    pub(crate) message_id: String,
    pub(crate) conversation_id: String,
    pub(crate) push_to: Option<Vec<String>>,
    pub(crate) push_mentions: Option<Vec<String>>,
}

static SENT_PUSH_AUDIENCES: std::sync::Mutex<Vec<SentPushAudience>> =
    std::sync::Mutex::new(Vec::new());

async fn record_push_audience(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(e) => return ds_internal_error(format!("send body: {e}")),
    };
    if let Ok(p) = serde_json::from_slice::<pollis_api::messages::SendMessageBody>(&bytes) {
        SENT_PUSH_AUDIENCES.lock().expect("push log").push(SentPushAudience {
            message_id: p.id,
            conversation_id: p.conversation_id,
            push_to: p.push_to,
            push_mentions: p.push_mentions,
        });
    }
    next.run(axum::extract::Request::from_parts(parts, axum::body::Body::from(bytes)))
        .await
}

pub(crate) fn sent_push_audience(message_id: &str) -> Option<SentPushAudience> {
    SENT_PUSH_AUDIENCES
        .lock()
        .expect("push log")
        .iter()
        .rev()
        .find(|a| a.message_id == message_id)
        .cloned()
}

/// Bounded wait for a matching wake-up; publishes are spawned.
pub(crate) async fn await_wake_up(
    room: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
) -> Option<SentWakeUp> {
    await_nth_wake_up(room, pred, 1).await
}

/// Bounded wait for the `n`th matching wake-up (1-based).
pub(crate) async fn await_nth_wake_up(
    room: &str,
    pred: impl Fn(&serde_json::Value) -> bool,
    n: usize,
) -> Option<SentWakeUp> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let hit = SENT_WAKE_UPS
            .lock()
            .expect("wake-up log")
            .iter()
            .filter(|w| w.room == room && pred(&w.payload))
            .nth(n - 1)
            .cloned();
        if hit.is_some() || std::time::Instant::now() >= deadline {
            return hit;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

pub(crate) fn wake_up_count(room: &str, pred: impl Fn(&serde_json::Value) -> bool) -> usize {
    SENT_WAKE_UPS
        .lock()
        .expect("wake-up log")
        .iter()
        .filter(|w| w.room == room && pred(&w.payload))
        .count()
}

// Zero-loss invariant (#1524): unless [`expect_losses`], a dropped client must hold no placeholder and log no `[loss]`.

static LOSSES_EXPECTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Opt out of the zero-loss check. Reset by [`wipe`].
pub(crate) fn expect_losses() {
    LOSSES_EXPECTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn losses_expected() -> bool {
    LOSSES_EXPECTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// Placeholders in the local DB, or `None` when it is closed or stayed busy.
fn placeholder_rows(client: &TestClient) -> Option<Vec<(String, String, String)>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let guard = loop {
        if let Ok(g) = client.state.local_db.try_lock() {
            break g;
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    let db = guard.as_ref()?;
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT conversation_id, id, undecryptable FROM message
             WHERE undecryptable IS NOT NULL",
        )
        .ok()?;
    let rows = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();
    Some(rows)
}

fn assert_no_unexpected_loss(client: &TestClient) {
    if std::thread::panicking() || losses_expected() {
        return;
    }
    let lines = pollis_core::diag::loss_tap::take();
    let placeholders = placeholder_rows(client).unwrap_or_default();
    if lines.is_empty() && placeholders.is_empty() {
        return;
    }
    let who = client
        .profile
        .as_ref()
        .map(|p| p.id.clone())
        .unwrap_or_else(|| "<not signed in>".into());
    panic!(
        "zero-loss invariant (#1524): a message was lost without the test setting out to \
         lose one (call `harness::expect_losses()` in a test that does).\n\
         placeholders on {who}: {placeholders:#?}\n[loss] lines: {lines:#?}"
    );
}

pub(crate) async fn wipe() {
    LOSSES_EXPECTED.store(false, std::sync::atomic::Ordering::SeqCst);
    pollis_core::diag::loss_tap::take();
    pollis_core::commands::messages::test_hooks::disarm_crash();
    clear_ds_fault();
    SENT_WAKE_UPS.lock().expect("wake-up log").clear();
    SENT_PUSH_AUDIENCES.lock().expect("push log").clear();
    let w = world().await;
    wipe_remote(
        &w.remote.conn().await.expect("main conn"),
        &w.log.conn().await.expect("log conn"),
    )
    .await
    .expect("wipe test turso");
    // The suite pin is process-global; reset it here so no test can forget.
    set_current_suite(None);
}

/// The DS's main-DB handle, for server-side effects only.
pub(crate) async fn writable_remote() -> Arc<Db> {
    world().await.remote.clone()
}

/// The DS's commit-log handle; the MLS control-plane tables live only here.
pub(crate) async fn writable_log() -> Arc<Db> {
    world().await.log.clone()
}

/// Seed a custom emoji server-side (#917 / #848), with a deliberately wrong `r2_key` so rendering stops before any network call.
pub(crate) async fn seed_group_emoji(
    group_id: &str,
    shortcode: &str,
    content_hash: &str,
    created_by: &str,
) {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("writable conn for seed_group_emoji");
    conn.execute(
        "INSERT OR IGNORE INTO custom_emoji_object \
             (content_hash, r2_key, content_type, size_bytes, animated) \
         VALUES (?1, ?2, 'image/webp', 1024, 0)",
        libsql::params![
            content_hash.to_string(),
            format!("emoji/{content_hash}-deliberately-not-the-derived-key.webp"),
        ],
    )
    .await
    .expect("insert custom_emoji_object");
    conn.execute(
        "INSERT OR REPLACE INTO group_emoji (group_id, shortcode, content_hash, created_by) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            group_id.to_string(),
            shortcode.to_string(),
            content_hash.to_string(),
            created_by.to_string(),
        ],
    )
    .await
    .expect("insert group_emoji");
}

/// Delete one interior commit row post-hoc to make a durable F1 gap; asserts exactly one row went.
pub(crate) async fn drop_commit_row(conversation_id: &str, epoch: i64) {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for drop_commit_row");
    let affected = conn
        .execute(
            "DELETE FROM mls_commit_log \
             WHERE conversation_id = ?1 AND epoch = ?2 \
               AND generation = (SELECT COALESCE(MAX(generation), 0) FROM mls_commit_log \
                                  WHERE conversation_id = ?1)",
            libsql::params![conversation_id.to_string(), epoch],
        )
        .await
        .expect("delete commit row");
    assert_eq!(
        affected, 1,
        "drop_commit_row: expected exactly ONE commit at epoch {epoch} for \
         {conversation_id} to delete, deleted {affected} — the gap scenario's \
         setup is wrong (no such epoch, or a duplicate row)"
    );
}

/// Truncate one interior commit's bytes (epoch intact) for the #680 deserialize wedge.
///
/// DELETE+INSERT, not UPDATE: `trg_mls_commit_log_immutable` (#691) aborts payload updates. Don't "simplify" it.
pub(crate) async fn corrupt_commit_row(conversation_id: &str, epoch: i64) {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for corrupt_commit_row");
    // Re-insert byte-for-byte except the payload, keeping `seq` (the replay order key).
    let mut rows = conn
        .query(
            "SELECT seq, generation, sender_id, created_at, added_user_id, added_device_ids \
             FROM mls_commit_log \
             WHERE conversation_id = ?1 AND epoch = ?2 \
               AND generation = (SELECT COALESCE(MAX(generation), 0) FROM mls_commit_log \
                                  WHERE conversation_id = ?1)",
            libsql::params![conversation_id.to_string(), epoch],
        )
        .await
        .expect("select commit row to corrupt");
    let row = rows
        .next()
        .await
        .expect("read commit row")
        .unwrap_or_else(|| {
            panic!(
                "corrupt_commit_row: no commit at epoch {epoch} for {conversation_id} — \
                 the scenario's setup is wrong (no such epoch)"
            )
        });
    // Read every column BEFORE the duplicate check: stepping the cursor invalidates `row`.
    let seq: i64 = row.get(0).expect("seq");
    let generation: i64 = row.get(1).expect("generation");
    let sender_id: String = row.get(2).expect("sender_id");
    let created_at: String = row.get(3).expect("created_at");
    let added_user_id: Option<String> = row.get(4).expect("added_user_id");
    let added_device_ids: Option<String> = row.get(5).expect("added_device_ids");
    // Only now is it safe to step the cursor.
    assert!(
        rows.next().await.expect("second row check").is_none(),
        "corrupt_commit_row: more than one commit at epoch {epoch} for {conversation_id} — \
         the scenario's setup is wrong (a duplicate row)"
    );

    // Too short to deserialize; interior, so neither DELETE nor INSERT guard trips.
    let deleted = conn
        .execute(
            "DELETE FROM mls_commit_log WHERE seq = ?1",
            libsql::params![seq],
        )
        .await
        .expect("delete commit row to corrupt");
    assert_eq!(
        deleted, 1,
        "corrupt_commit_row: expected to delete exactly ONE row at seq {seq} for \
         {conversation_id}, deleted {deleted}"
    );
    let inserted = conn
        .execute(
            "INSERT INTO mls_commit_log \
               (seq, conversation_id, generation, epoch, sender_id, commit_data, \
                created_at, added_user_id, added_device_ids) \
             VALUES (?1, ?2, ?3, ?4, ?5, X'0001', ?6, ?7, ?8)",
            libsql::params![
                seq,
                conversation_id.to_string(),
                generation,
                epoch,
                sender_id,
                created_at,
                added_user_id,
                added_device_ids,
            ],
        )
        .await
        .expect("re-insert corrupted commit row");
    assert_eq!(
        inserted, 1,
        "corrupt_commit_row: expected to re-insert exactly ONE row at epoch {epoch} for \
         {conversation_id}, inserted {inserted} — the scenario's setup is wrong"
    );
}

/// Prune below `floor` (exclusive) via the real retention DELETE (#539). Asserts it pruned something.
pub(crate) async fn prune_commits_below(conversation_id: &str, floor: i64) -> u64 {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for prune_commits_below");
    let generation = pollis_delivery::commit::head_generation(&conn, conversation_id)
        .await
        .expect("head_generation");
    let pruned =
        pollis_delivery::commit::delete_commits_below(&conn, conversation_id, generation, floor)
            .await
            .expect("delete_commits_below");
    assert!(
        pruned > 0,
        "prune_commits_below: expected to prune at least one commit below epoch \
         {floor} for {conversation_id}, pruned 0 — the scenario's setup is wrong"
    );
    pruned
}

/// Record a forged high-water for every current member device (#681's effect).
pub(crate) async fn force_high_water_for_all_members(conversation_id: &str, since: i64) {
    let w = world().await;
    let main_conn = w.remote.conn().await.expect("main conn for force_high_water");
    let log_conn = w.log.conn().await.expect("log conn for force_high_water");
    let generation = pollis_delivery::commit::head_generation(&log_conn, conversation_id)
        .await
        .expect("head_generation");
    let mut rows = main_conn
        .query(
            "SELECT DISTINCT ud.device_id, ud.user_id FROM user_device ud \
             WHERE ud.revoked_at IS NULL AND ud.user_id IN ( \
                 SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1 \
                 UNION SELECT user_id FROM group_member WHERE group_id = ?1 \
                 UNION SELECT gm.user_id FROM channels c \
                     JOIN group_member gm ON gm.group_id = c.group_id WHERE c.id = ?1 )",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("member devices");
    let mut devices = Vec::new();
    while let Some(r) = rows.next().await.expect("member device row") {
        devices.push((r.get::<String>(0).unwrap(), r.get::<String>(1).unwrap()));
    }
    assert!(
        !devices.is_empty(),
        "force_high_water_for_all_members: no member devices for {conversation_id}"
    );
    for (device_id, user_id) in devices {
        pollis_delivery::commit::record_commit_since(
            &log_conn,
            conversation_id,
            &user_id,
            &device_id,
            generation,
            since,
        )
        .await
        .expect("record forced high-water");
    }
}

/// Run the real event-driven retention prune and return its report.
pub(crate) async fn ds_prune_commit_log(
    conversation_id: &str,
) -> pollis_delivery::commit::PruneReport {
    let w = world().await;
    let main_conn = w.remote.conn().await.expect("main conn for ds_prune_commit_log");
    let log_conn = w.log.conn().await.expect("log conn for ds_prune_commit_log");
    pollis_delivery::commit::prune_commit_log(&main_conn, &log_conn, conversation_id)
        .await
        .expect("prune_commit_log")
}

/// The DS head epoch (`MAX(epoch) + 1`), read from the LOG DB.
pub(crate) async fn ds_head_epoch(conversation_id: &str) -> i64 {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for ds_head_epoch");
    pollis_delivery::commit::head_epoch(&conn, conversation_id)
        .await
        .expect("head_epoch")
}

/// The DS head suite generation (#454 P4); 0 until a migration.
pub(crate) async fn ds_head_generation(conversation_id: &str) -> i64 {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for ds_head_generation");
    pollis_delivery::commit::head_generation(&conn, conversation_id)
        .await
        .expect("head_generation")
}

pub(crate) async fn delivery_url() -> String {
    world()
        .await
        .config
        .pollis_delivery_url
        .clone()
        .expect("in-process delivery URL configured")
}

/// Raw-`TcpStream` HTTP POST returning the status, to prove the DS rejects unsigned writes.
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
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("could not parse status from: {status_line:?}"))
}

/// POST signed as `client`'s device key, byte-for-byte as `ds_client::ds_post` does.
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
    let timestamp = pollis_delivery::util::now_unix() as i64;
    let message = pollis_delivery::auth::canonical_message("POST", path, timestamp, body);

    let signature_b64 = {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("client local db open");
        let provider = pollis_lib::commands::mls::PollisProvider::new(db.conn());
        // ML-DSA-44 half of the device identity (#668 P5).
        let (signer, _pub) = pollis_lib::commands::mls::load_or_create_device_signer(
            &provider,
            &user_id,
            &device_id,
            openmls::prelude::SignatureScheme::MLDSA44,
        )
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

/// One simulated device: its own `MockRuntime` app, keystore and `AppState`.
pub(crate) struct TestClient {
    /// Must outlive the webview.
    pub(crate) _app: App<MockRuntime>,
    pub(crate) webview: WebviewWindow<MockRuntime>,
    #[allow(dead_code)]
    pub(crate) state: Arc<AppState>,
    pub(crate) profile: Option<UserProfile>,
}

impl TestClient {
    /// Does NOT sign in; call [`sign_up`].
    pub(crate) async fn new() -> Self {
        Self::new_with_config(world().await.config.clone()).await
    }

    /// Sealed sender is unconditional (#607); there is no toggle.
    async fn new_with_config(config: Config) -> Self {
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        // No database handle (#987); `pollis-core/tests/no_client_side_remote_reads.rs` pins it.
        let state = Arc::new(AppState::new_with_parts(config, keystore));
        // Each device gets its own local DB (#1524); `accounts.json` stays shared.
        static NEXT_DEVICE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT_DEVICE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = pollis_core::db::local::dirs_path().join(format!("device-{n}"));
        state.local_dir.set(dir).expect("a fresh state has no local dir");
        let (app, webview) = build_client_app(state.clone()).expect("build client app");
        Self {
            _app: app,
            webview,
            state,
            profile: None,
        }
    }

    /// Signup via the real OTP flow, then `set_pin` (required post-#194 to open the DB).
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

    /// Point `accounts.json`'s active user at this client: `ds_post` signs as that global user.
    pub(crate) fn activate(&self) {
        if let Some(p) = &self.profile {
            let _ = pollis_lib::accounts::upsert_account(&p.id, &p.username, None);
        }
    }

    pub(crate) async fn invoke_json(&self, cmd: &str, args: serde_json::Value) -> serde_json::Value {
        self.activate();
        invoke(&self.webview, cmd, args)
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }

    /// Like [`invoke_json`] but returns the `Result`, for expected failures.
    pub(crate) async fn invoke_try(
        &self,
        cmd: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        self.activate();
        invoke(&self.webview, cmd, args).await
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        assert_no_unexpected_loss(self);
    }
}

impl TestClient {
    /// Drain pending MLS welcomes; commits are per-channel ([`process_commits_for`]).
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

    /// The cold-launch / reconnect MLS sweep, including the reconcile backstop (#430 P1).
    #[allow(dead_code)]
    pub(crate) async fn sweep(&self) {
        self.activate();
        let _: serde_json::Value = invoke(
            &self.webview,
            "catch_up_all_mls_groups",
            json!({ "userId": self.user_id() }),
        )
        .await
        .unwrap_or_else(|e| panic!("catch_up_all_mls_groups: {e}"));
    }

    /// Drain pending commits for one conversation's MLS group.
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
        // Opt in to #General: the frontend now defaults to off.
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

    pub(crate) async fn list_group_invites(&self, group_id: &str) -> Vec<serde_json::Value> {
        let invites: serde_json::Value = self
            .invoke_json(
                "list_group_invites",
                json!({ "groupId": group_id, "userId": self.user_id() }),
            )
            .await;
        invites.as_array().expect("invites array").clone()
    }

    pub(crate) async fn try_revoke_invite(
        &self,
        group_id: &str,
        invite_id: &str,
    ) -> Result<serde_json::Value, String> {
        self.invoke_try(
            "revoke_group_invite",
            json!({
                "inviteId": invite_id,
                "groupId": group_id,
                "userId": self.user_id(),
            }),
        )
        .await
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
            .invoke_json(
                "get_group_members",
                json!({ "groupId": group_id, "requesterId": self.user_id() }),
            )
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
            .invoke_json(
                "list_group_channels",
                json!({ "groupId": group_id, "requesterId": self.user_id() }),
            )
            .await;
        channels.as_array().expect("channels array").clone()
    }

    /// Another text channel; all channels share the group's one MLS group.
    #[allow(dead_code)]
    pub(crate) async fn create_channel(&self, group_id: &str, name: &str) -> String {
        let ch: serde_json::Value = self
            .invoke_json(
                "create_channel",
                json!({
                    "groupId": group_id,
                    "name": name,
                    "description": null,
                    "channelType": "text",
                    "creatorId": self.user_id(),
                }),
            )
            .await;
        ch["id"].as_str().expect("channel id").to_string()
    }

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

    /// Voluntarily leave, signalling remaining members to reconcile.
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

    /// Reply in a thread (#825); `thread_id` is the root's ULID.
    pub(crate) async fn send_thread_reply(
        &self,
        conversation_id: &str,
        thread_id: &str,
        content: &str,
    ) -> serde_json::Value {
        self.activate();
        invoke(
            &self.webview,
            "send_message",
            json!({
                "conversationId": conversation_id,
                "senderId": self.user_id(),
                "content": content,
                "replyToId": null,
                "threadId": thread_id,
                "senderUsername": self.profile.as_ref().map(|p| p.username.clone()),
            }),
        )
        .await
        .unwrap_or_else(|e| panic!("send_thread_reply({conversation_id}, {thread_id}): {e}"))
    }

    pub(crate) async fn read_thread(&self, thread_id: &str) -> Vec<serde_json::Value> {
        self.activate();
        invoke::<Vec<serde_json::Value>>(
            &self.webview,
            "read_thread_messages",
            json!({ "threadId": thread_id }),
        )
        .await
        .unwrap_or_else(|e| panic!("read_thread_messages({thread_id}): {e}"))
    }

    pub(crate) async fn thread_summaries(&self, conversation_id: &str) -> Vec<serde_json::Value> {
        self.activate();
        invoke::<Vec<serde_json::Value>>(
            &self.webview,
            "list_thread_summaries",
            json!({ "conversationId": conversation_id }),
        )
        .await
        .unwrap_or_else(|e| panic!("list_thread_summaries({conversation_id}): {e}"))
    }

    pub(crate) async fn send_channel_message(&self, conversation_id: &str, content: &str) {
        self.try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
    }

    pub(crate) async fn send_channel_message_id(&self, conversation_id: &str, content: &str) -> String {
        let msg = self
            .try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
        msg["id"].as_str().expect("message id").to_string()
    }

    /// Re-seals at the current epoch and replaces any prior edit envelope.
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

    /// Delete for everyone.
    pub(crate) async fn delete_message(&self, message_id: &str) {
        self.invoke_json(
            "delete_message",
            json!({
                "messageId": message_id,
                "userId": self.user_id(),
            }),
        )
        .await;
    }

    /// The FULL history across every page; one page would fake losses in long runs.
    pub(crate) async fn fetch_channel_messages(&self, channel_id: &str) -> Vec<serde_json::Value> {
        let mut all: Vec<serde_json::Value> = Vec::new();
        let mut cursor: Option<serde_json::Value> = None;
        loop {
            let mut args = json!({ "userId": self.user_id(), "channelId": channel_id, "limit": 50 });
            if let Some(c) = &cursor {
                args["cursor"] = c.clone();
            }
            let page: serde_json::Value = self.invoke_json("get_channel_messages", args).await;
            let msgs = page["messages"].as_array().expect("messages array");
            all.extend(msgs.iter().cloned());
            match page.get("next_cursor") {
                Some(nc) if !nc.is_null() => cursor = Some(nc.clone()),
                _ => break,
            }
        }
        all
    }

    /// Group-level catch-up without reading a page.
    pub(crate) async fn ingest_channel(&self, channel_id: &str) {
        self.invoke_json(
            "ingest_channel_envelopes",
            json!({ "userId": self.user_id(), "channelId": channel_id }),
        )
        .await;
    }

    /// Drain the outbox, returning its report.
    pub(crate) async fn flush_outbox(&self) -> serde_json::Value {
        self.invoke_json("flush_outbox", json!({})).await
    }

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

    /// Fake media-server values (#917), so `get_emoji_url` reaches the object lookup.
    pub(crate) async fn arm_media_server(&self) {
        *self.state.media_server_port.lock().await = Some(1);
        *self.state.media_server_token.lock().await = Some("test-token".to_string());
    }

    pub(crate) async fn group_member_roles(&self, group_id: &str) -> Vec<(String, String)> {
        let members: serde_json::Value = self
            .invoke_json(
                "get_group_members",
                json!({ "groupId": group_id, "requesterId": self.user_id() }),
            )
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

impl TestClient {
    /// `get_dm_messages`, which polls welcomes and processes commits first.
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

    /// Leave a DM; also how a pending request is rejected.
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

/// Enroll a second device for `primary`'s user through the real enrollment commands.
pub(crate) async fn enroll_second_device(primary: &TestClient, email: &str) -> TestClient {
    // 1. Sign in as an existing user, so enrollment is required.
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

    // 3. The approver wraps account_id_key under the requester's ephemeral key.
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
    // The approver's list carries NO verification code (#1096); it must be typed.
    assert!(
        matching.get("verification_code").is_none(),
        "the approver's list must not carry a verification code: {matching:#?}"
    );

    // The new device's own code, as a human would type it.
    primary
        .invoke_json(
            "approve_device_enrollment",
            json!({
                "requestId": request_id,
                "verificationCode": verification_code,
            }),
        )
        .await;

    // 4. Bounded poll; the approve has already committed.
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

    // 5. Mirror App.tsx after pin-create: set_pin → finalize_device_enrollment → initialize_identity.
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

/// A passive adversary with a copy of one device's `mls_kv`, riding the commit chain until the victim commits (#666).
pub(crate) struct StolenLeaf {
    /// The attacker's own client; only the MLS key material is the victim's.
    client: TestClient,
    victim: String,
}

/// Copy `victim`'s `mls_kv` onto a fresh attacker device, replacing its own rows.
pub(crate) async fn steal_leaf(victim: &TestClient) -> StolenLeaf {
    let mut client = TestClient::new().await;
    client.sign_up("attacker@test.local").await;

    // As `Value`s: `scope` is stored as blob or text depending on the writer.
    type Row = (
        rusqlite::types::Value,
        rusqlite::types::Value,
        rusqlite::types::Value,
    );
    let rows: Vec<Row> = {
        let guard = victim.state.local_db.lock().await;
        let db = guard.as_ref().expect("victim local db open");
        let mut stmt = db
            .conn()
            .prepare("SELECT scope, key, value FROM mls_kv")
            .expect("read victim mls_kv");
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query victim mls_kv")
            .map(|r| r.expect("victim mls_kv row"))
            .collect::<Vec<Row>>();
        rows
    };
    assert!(!rows.is_empty(), "stealing an empty MLS state proves nothing");

    {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("attacker local db open");
        db.conn()
            .execute("DELETE FROM mls_kv", [])
            .expect("clear attacker mls_kv");
        for (scope, key, value) in &rows {
            db.conn()
                .execute(
                    "INSERT INTO mls_kv (scope, key, value) VALUES (?1, ?2, ?3)",
                    rusqlite::params![scope, key, value],
                )
                .expect("seed attacker mls_kv");
        }
    }

    // Sign as the victim: the stolen `mls_kv` holds its signing key, and the DS only answers the verified signer (#987).
    *client.state.device_id.lock().await = victim.state.device_id.lock().await.clone();
    if let Some(u) = victim.state.unlock.lock().await.as_ref() {
        *client.state.unlock.lock().await = Some(pollis_lib::commands::pin::UnlockState {
            user_id: u.user_id.clone(),
            db_key: u.db_key.clone(),
            account_id_key: u.account_id_key.clone(),
        });
    }

    StolenLeaf {
        client,
        victim: victim.user_id().to_string(),
    }
}

impl StolenLeaf {
    /// Ride the commit chain as far as the stolen keys allow; stalls at the victim's own commit.
    pub(crate) async fn catch_up(&self, mls_group_id: &str) {
        if let Err(e) = pollis_lib::commands::mls::process_pending_commits_inner(
            &self.client.state,
            mls_group_id,
            &self.victim,
        )
        .await
        {
            eprintln!("[attacker] replay stalled for {mls_group_id}: {e}");
        }
    }

    /// Catch up, then try the real decrypt over every stored envelope (channel id vs group id).
    pub(crate) async fn can_read(
        &self,
        conversation_id: &str,
        mls_group_id: &str,
        body: &str,
    ) -> bool {
        self.catch_up(mls_group_id).await;
        let ciphertexts = envelope_ciphertexts(conversation_id).await;
        let guard = self.client.state.local_db.lock().await;
        let db = guard.as_ref().expect("attacker local db open");
        for ciphertext in ciphertexts {
            let Some((plain, _sender)) = pollis_lib::commands::mls::try_mls_decrypt(
                db.conn(),
                mls_group_id,
                &ciphertext,
            ) else {
                continue;
            };
            // Plaintext is padded, so the body is a substring.
            if plain
                .windows(body.len())
                .any(|w| w == body.as_bytes())
            {
                return true;
            }
        }
        false
    }

    pub(crate) fn victim(&self) -> &str {
        &self.victim
    }
}

/// Every MLS ciphertext the DS holds for `conversation_id`.
pub(crate) async fn envelope_ciphertexts(conversation_id: &str) -> Vec<Vec<u8>> {
    let remote = world().await.remote.clone();
    let conn = remote.conn().await.expect("remote conn for envelopes");
    let mut rows = conn
        .query(
            "SELECT ciphertext FROM message_envelope WHERE conversation_id = ?1 ORDER BY sent_at ASC",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("query message_envelope");

    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("envelope row") {
        let ciphertext: String = row.get(0).expect("ciphertext column");
        if let Some(bytes) = ciphertext.strip_prefix("mls:").and_then(|h| hex::decode(h).ok()) {
            out.push(bytes);
        }
    }
    out
}

/// The pre-#454 suite, retired by #669; the only other suite the provider implements.
pub(crate) const SUITE_LEGACY: u16 = 0x0001;

/// Pin the process's current suite (`None` = `CS_PQ`) to simulate a code-point renumber; [`wipe`] clears it.
pub(crate) fn set_current_suite(code_point: Option<u16>) {
    pollis_lib::commands::mls::set_current_suite_override(code_point);
}

/// Production's `ensure_mls_key_package`, as at login.
pub(crate) async fn republish_key_packages(client: &TestClient) {
    let user_id = client.user_id().to_string();
    let device_id = client
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("client device_id set");
    pollis_lib::commands::mls::ensure_mls_key_package(&client.state, &user_id, &device_id)
        .await
        .expect("republish key package pool");
}

/// Local suite generation for `conversation_id` (#454 P4).
pub(crate) async fn local_generation_of(client: &TestClient, conversation_id: &str) -> i64 {
    let guard = client.state.local_db.lock().await;
    let db = guard.as_ref().expect("client local db open");
    db.conn()
        .query_row(
            "SELECT generation FROM mls_generation WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
}

/// The published GroupInfo's ciphersuite code point, read off the server blob.
pub(crate) async fn ds_group_info_suite(conversation_id: &str) -> u16 {
    use openmls::prelude::*;
    use tls_codec::Deserialize as _;

    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for group_info");
    let mut rows = conn
        .query(
            "SELECT group_info FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("query mls_group_info");
    let blob: Vec<u8> = rows
        .next()
        .await
        .expect("group_info row")
        .expect("a published GroupInfo")
        .get(0)
        .expect("group_info column");

    let mut reader: &[u8] = &blob;
    let msg = MlsMessageIn::tls_deserialize(&mut reader).expect("deserialize GroupInfo envelope");
    match msg.extract() {
        MlsMessageBodyIn::GroupInfo(gi) => u16::from(gi.ciphersuite()),
        _ => panic!("mls_group_info holds something that is not a GroupInfo"),
    }
}

/// Every Welcome blob the DS holds for `conversation_id`, oldest first.
pub(crate) async fn welcome_blobs(conversation_id: &str) -> Vec<Vec<u8>> {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for welcomes");
    let mut rows = conn
        .query(
            "SELECT welcome_data FROM mls_welcome WHERE conversation_id = ?1 \
             ORDER BY created_at ASC, id ASC",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("query mls_welcome");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("welcome row") {
        out.push(row.get::<Vec<u8>>(0).expect("welcome column"));
    }
    out
}

/// Ceiling for a commit that adds nobody: must stay logarithmic in the roster (#666).
pub(crate) const COMMIT_CEILING_NO_ADDS: usize = 20_480;

/// Ceiling per inlined KeyPackage (~8.7 KiB measured today).
pub(crate) const COMMIT_CEILING_PER_ADD: usize = 12_288;

/// Check each commit against its own budget: no-add commits are logarithmic, k-add commits linear in k.
pub(crate) async fn commit_budget_violation(conversation_id: &str) -> Option<String> {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for commit budgets");
    let mut rows = conn
        .query(
            "SELECT generation, epoch, LENGTH(commit_data), added_device_ids \
             FROM mls_commit_log WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("query commit budgets");

    let mut worst: Option<(i64, String)> = None;
    while let Some(row) = rows.next().await.expect("commit budget row") {
        let generation: i64 = row.get(0).expect("generation");
        let epoch: i64 = row.get(1).expect("epoch");
        let size = row.get::<i64>(2).expect("commit length") as usize;
        // Comma-separated added device ids, NULL for none.
        let added = row
            .get::<Option<String>>(3)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .map_or(0, |s| s.split(',').count());

        let allowed = COMMIT_CEILING_NO_ADDS + COMMIT_CEILING_PER_ADD * added;
        if size > allowed {
            let overage = size as i64 - allowed as i64;
            let detail = format!(
                "generation {generation} epoch {epoch}: {size} B against a {allowed} B budget \
                 ({COMMIT_CEILING_NO_ADDS} B + {added} added × {COMMIT_CEILING_PER_ADD} B) \
                 — {overage} B over"
            );
            if worst.as_ref().is_none_or(|(w, _)| overage > *w) {
                worst = Some((overage, detail));
            }
        }
    }
    worst.map(|(_, detail)| detail)
}

impl TestClient {
    /// Rotate this device's leaf (#666); `false` means another member won the epoch.
    pub(crate) async fn self_update(&self, conversation_id: &str) -> bool {
        self.try_self_update(conversation_id)
            .await
            .unwrap_or_else(|e| panic!("self_update_group({conversation_id}): {e}"))
    }

    /// Fallible [`self_update`](Self::self_update): an `Err` is a fuzzer finding.
    pub(crate) async fn try_self_update(&self, conversation_id: &str) -> Result<bool, String> {
        let user_id = self.user_id().to_string();
        pollis_lib::commands::mls::self_update_group(&self.state, conversation_id, &user_id)
            .await
            .map_err(|e| e.to_string())
    }
}
