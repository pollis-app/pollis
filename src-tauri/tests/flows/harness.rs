//! End-to-end integration harness for Pollis.
//!
//! Drives the real `#[tauri::command]` functions through the tauri IPC
//! pipeline — no `_inner` shims, no mocked DB layer. Each [`TestClient`]
//! owns its own `App<MockRuntime>` backed by its own `InMemoryKeystore`,
//! while all clients share a single [`TestWorld`] pointed at a process-local
//! libsql file (no network round-trip — see `Db::connect_local`).
//!
//! Since #987 a client has NO database handle of its own: `pollis-core` does not
//! link `libsql` at all, so every read a client performs is a signed POST to the
//! in-process Delivery Service, exactly like its writes. The world's two handles
//! belong to the DS. Tests that need to construct server-side state directly —
//! backdating envelopes, clearing watermarks, seeding an emoji row — use
//! [`writable_remote`] / [`writable_log`], which are those DS handles: they stand
//! in for effects that happen server-side in production, never for client
//! writes.
//!
//! Run with:
//! ```
//! cargo test --features test-harness --test flows
//! ```
//!
//! Tests serialize on a process-wide mutex (`serial_test`) so the per-test DB
//! wipe can't race. Note there is NO shared Turso involved: since #420 each run
//! uses process-local libsql files, and since #987 there is no Turso credential
//! anywhere in the build to point at one. An intermittent failure here is a real
//! race in the code, never contention on a shared database — a stale comment
//! claiming otherwise sent one investigation down exactly that wrong path
//! (#832).

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
    pub(crate) remote: Arc<Db>,
    /// Commit-log DB — the three MLS control-plane tables (`mls_commit_log`,
    /// `mls_welcome`, `mls_group_info`) and NOTHING else. A genuinely separate
    /// libsql file so a misrouted query (a main read on the log handle, or a
    /// log read on the main handle) fails with "no such table" instead of
    /// silently succeeding on one shared file. Mirrors the #420 production split.
    pub(crate) log: Arc<Db>,
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
            // Not `std::env::set_var`: the harness runs an in-process DS and
            // per-client background work on other threads, and mutating the
            // process environment while another thread may be reading it is a
            // data race (#923). `set_data_dir` is a locked write.
            pollis_core::db::local::set_data_dir(&path);

            // No `DEV_OTP` in the environment: the in-process DS is handed the
            // code explicitly as `OtpConfig { dev_otp }` and every client posts
            // `DEV_OTP` literally to verify-otp, so the env var was carrying
            // nothing but the same `set_var` race (#923).

            // Stand-in for "remote Turso" (the MAIN DB) — a libsql file in the
            // same temp dir as the per-user SQLCipher DBs. No network round-trip.
            let remote_db_path = path.join("test_turso.db");
            let remote = Arc::new(
                Db::connect_local(remote_db_path.to_str().expect("utf-8 path"))
                    .await
                    .expect("connect local libsql"),
            );

            bootstrap_schema(&remote.conn().await.expect("main conn"))
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
                Db::connect_local(log_db_path.to_str().expect("utf-8 path"))
                    .await
                    .expect("connect local log libsql"),
            );
            bootstrap_log_schema(&log.conn().await.expect("log conn"))
                .await
                .expect("bootstrap log db schema");
            // The baseline created the three MLS tables on MAIN too (for old
            // shipped clients); drop them so main no longer has them and any
            // stray main-side access fails loudly.
            drop_log_tables_from_main(&remote.conn().await.expect("main conn"))
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

/// One-shot fault menu for the in-process Delivery Service.
///
/// When armed, the next commit submit (`POST /v1/commits`) that the fault
/// *applies to* is perturbed exactly once and the arm is then atomically
/// consumed — so a fault set up for one operation can never bleed into a later
/// one, even though every `#[serial]` test shares the single in-process DS
/// (`spawn_in_process_delivery`). This generalizes the old
/// `LOSE_NEXT_DS_RESPONSE: AtomicBool` (issue #411) into the failure taxonomy the
/// adversarial recovery suite drives (see `flows/adversarial.rs`).
///
/// SAFETY / SCOPE: every fault lives in the HARNESS layer (this file), NEVER in
/// `pollis-delivery`. Production commit semantics stay untouched — the real
/// client code path runs against a DS that behaves exactly as prod plus the one
/// injected perturbation. `DropWelcome` is realized by clearing the Welcomes off
/// the parsed body *before* `submit_commit` (so they're simply never persisted),
/// not by patching the delivery crate.
///
/// The commit-log GAP fault (`DropCommitRow`, #430-P2 / F1) is deliberately NOT a
/// variant here: deleting an already-acked commit row is done post-hoc by
/// [`drop_commit_row`], because a live-path deletion of the *head* row only lowers
/// the head (the next committer refills it) and never leaves a durable interior
/// gap. See that helper's docs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DsFault {
    /// The commit + GroupInfo + Welcomes LAND, then the success response is
    /// turned into a 500 — the #411 "lost success-response" shape from the
    /// client's view. The client must adopt its own canonical commit, not wedge.
    DropResponse,
    /// Same mechanism as [`DsFault::DropResponse`], named for the generalized-#411
    /// scenario: the DS persists the write, THEN returns 500, so the client
    /// believes the submit failed while the DS has in fact committed it. Recovery
    /// is the `our_commit_is_canonical` adopt path.
    Fail500PostWrite,
    /// The DS returns 500 WITHOUT persisting anything — the clean, retryable
    /// failure (the contrast case to `Fail500PostWrite`). Nothing lands, so a
    /// straight resubmit succeeds; no adoption, no gap.
    Fail500PreWrite,
    /// The commit + GroupInfo land, but the Welcomes carried by this add are
    /// never persisted, so the added member's Welcome never becomes fetchable.
    /// The new member must recover via external-join instead.
    DropWelcome,
}

/// The single armed one-shot DS fault, or `None`. A `Mutex<Option<_>>` (rather
/// than an atomic) so it can carry the fault *kind*, and `const`-constructible so
/// it can live in a `static`. Only ever locked across non-await sections
/// ([`peek_ds_fault`] / [`take_ds_fault`] copy the value out and drop the guard
/// immediately), so it never blocks the async runtime.
pub(crate) static NEXT_DS_FAULT: std::sync::Mutex<Option<DsFault>> =
    std::sync::Mutex::new(None);

/// Arm the next-commit DS fault. One-shot: consumed the first time it applies.
pub(crate) fn arm_ds_fault(fault: DsFault) {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned") = Some(fault);
}

/// Force-disarm any armed fault. Deterministic scenarios arm-and-consume in the
/// same breath, so they never need this; the model/proptest fuzzer
/// (`flows/model.rs`) calls it at the start of every generated case as a
/// belt-and-suspenders guard, so a fault armed for a commit-producing op that a
/// later case never issued can never bleed across cases sharing the singleton
/// `NEXT_DS_FAULT`.
pub(crate) fn clear_ds_fault() {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned") = None;
}

/// Peek the armed fault without consuming it.
fn peek_ds_fault() -> Option<DsFault> {
    *NEXT_DS_FAULT.lock().expect("NEXT_DS_FAULT poisoned")
}

/// Atomically consume (disarm) the armed fault, returning what was armed.
fn take_ds_fault() -> Option<DsFault> {
    NEXT_DS_FAULT
        .lock()
        .expect("NEXT_DS_FAULT poisoned")
        .take()
}

/// True iff a DS fault is still armed — for the "armed before, disarmed after"
/// assertion that proves the fault fired exactly once (replaces the old
/// `LOSE_NEXT_DS_RESPONSE.load(...)`).
pub(crate) fn ds_fault_armed() -> bool {
    peek_ds_fault().is_some()
}

/// State for the ONE endpoint the harness serves itself: the fault-injecting
/// `/v1/commits` override. Everything else runs on the real
/// `pollis_delivery::AppState`.
///
/// Two handles, mirroring production `AppState { db, log_db }`: the signature
/// check runs on `main`, the commit-log write on `log`. Splitting them is what
/// lets a misrouted query (a `user_device` read on the log handle, or an `mls_*`
/// read on the main handle) fail loudly rather than silently succeed.
#[derive(Clone)]
struct DsState {
    /// Main DB — `user_device`, for the signature check.
    main: Arc<Db>,
    /// Commit-log DB — `mls_commit_log` / `mls_welcome` / `mls_group_info`.
    log: Arc<Db>,
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

/// The `/v1/commits` submit handler, driven against the SHARED `Db` that also
/// answers every client read, so a write is visible to the next read. Mirrors
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

    let mut parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return ds_bad_request(),
    };

    // Identity binding: a validly-signed request may only write as itself.
    if parsed.sender_id != authed {
        return pollis_delivery::error::AuthRejection::Forbidden.into_response();
    }

    // ── Pre-write faults, each consumed ONLY when it applies ──────────────────
    // (peek, then take at the application point, so an armed fault is never
    // wasted on a submit it wasn't meant for).
    match peek_ds_fault() {
        Some(DsFault::Fail500PreWrite) => {
            take_ds_fault();
            // Nothing is persisted — the clean, retryable failure. The client
            // may simply resubmit and win the same epoch.
            return (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "simulated pre-write failure (test fault injection)",
            )
                .into_response();
        }
        Some(DsFault::DropWelcome) => {
            take_ds_fault();
            // Commit + GroupInfo still land; the added member's Welcome never
            // does. Clearing the body's Welcomes means `submit_commit` never
            // writes an `mls_welcome` row, so the new member can't join from a
            // Welcome and must recover via external-join.
            parsed.welcomes.clear();
        }
        _ => {}
    }

    // The commit log lives on the LOG DB.
    let conn = match state.log.conn().await {
        Ok(c) => c,
        Err(e) => return ds_internal_error(format!("conn: {e}")),
    };
    match pollis_delivery::commit::submit_commit(&conn, &parsed).await {
        Ok(outcome) => {
            // Post-write faults: the commit + GroupInfo + any Welcomes have
            // already LANDED above; turn the success response into a 500 so the
            // client must recover by observing the commit is canonical and
            // adopting it (issue #411 / `our_commit_is_canonical`). `DropResponse`
            // and `Fail500PostWrite` share this mechanism.
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

/// Boot the REAL `pollis-delivery` router in-process against the harness's own
/// libsql handles, bind it on a loopback port, and serve it on a DEDICATED OS
/// thread with its own runtime so the server outlives every individual
/// `#[tokio::test]` runtime.
///
/// # This mounts the production router, not a copy of it (#918)
///
/// It used to re-declare every route and re-implement every handler here — 60
/// wrappers and a 70-line route table, all of them `gate → parse → apply →
/// map outcome` around the same `pollis_delivery::*` functions the real handlers
/// call. The only thing that forced the duplication was the DB handle type
/// (`Arc<Db>` here, `Arc<pollis_delivery::db::Db>` there). #918 removed it by
/// sharing one handle; #987 removed the question entirely, because the clients
/// hold no database handle to disagree with — the harness builds ONE
/// `pollis_delivery::db::Db` and `Arc::clone`s it into the router, and every
/// client read is a signed POST to that router.
///
/// The copy was not free. It silently omitted ~10 routes: `/v1/invite-links/*`
/// 404'd under test while working in production, and custom emoji (#848) had no
/// integration coverage at all. A mirror cannot go stale if there is no mirror.
///
/// `/v1/commits` is the ONE exception, and it is an override rather than a
/// re-declaration: the harness needs pre- and post-write fault injection
/// ([`DsFault`]) that production must never have. It is mounted on an outer
/// router whose `fallback_service` is the real one, so every other path — today's
/// and tomorrow's — reaches the production handler with no edit here.
///
/// Why a dedicated thread: each `#[tokio::test(flavor = "multi_thread")]` spins
/// up and tears down its OWN Tokio runtime. If we spawned the server on the
/// first test's runtime, it would die when that test finished, and every later
/// test would hit a dead port (connection refused). Owning the server on a
/// separate thread + runtime decouples its lifetime from the per-test runtimes,
/// so the single shared DS stays up for the whole `cargo test` process.
async fn spawn_in_process_delivery(main: Arc<Db>, log: Arc<Db>) -> String {
    use std::sync::mpsc;

    // The DS gets the harness's OWN handles, not second ones opened on the same
    // files: two independent `Database`s on one file do not share WAL writes
    // promptly, and a test reading rows the DS has "already written" is exactly
    // the ghost failure this suite exists to rule out. Since #987 they are
    // `pollis_delivery::db::Db` outright — there is no client-side handle type
    // left to bridge from.
    let ds_main = Arc::clone(&main);
    let ds_log = Arc::clone(&log);

    // Auth is ENFORCED. The suite drives the signed write path end to end, so a
    // regression in request signing or in server-side authz fails here rather
    // than in production. (The DS's no-auth mode is covered by unit tests in
    // `pollis-delivery`, which can exercise it without a signing client.)
    let real_state = pollis_delivery::AppState::new_with_log_db(ds_main, ds_log, true)
        // DEV_OTP forces the fixed harness code and skips the email send; no
        // resend throttle so back-to-back sign-ups in one test aren't blocked.
        .with_otp_config(pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
        })
        // Every request in the suite arrives from 127.0.0.1, so the production
        // per-IP limits (10 OTP requests / 10 min) read the whole test run as one
        // abusive client. Raised, not disabled — `ratelimit.rs`'s own unit tests
        // pin the real numbers; here the limiter must simply not be the thing
        // that fails a flow.
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
        });

    // Only `/v1/commits` is served by the harness, for fault injection.
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
                // The real production router, verbatim, with ONE path overridden for
                // fault injection. `fallback_service` (not `merge`/`route`) is what makes
                // the override legal: axum panics on a duplicate route, but everything the
                // outer router does not match falls through to the inner one — including
                // every endpoint added to `pollis-delivery` after today, with no edit here.
                let router = axum::Router::new()
                    .route(
                        <pollis_delivery::commit::SubmitBody as pollis_api::DsRequest>::PATH,
                        axum::routing::post(delivery_submit),
                    )
                    .with_state(fault_state)
                    .fallback_service(pollis_delivery::build_router_with_state(real_state));
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
    wipe_remote(
        &w.remote.conn().await.expect("main conn"),
        &w.log.conn().await.expect("log conn"),
    )
    .await
    .expect("wipe test turso");
    // The current-suite pin is process-global (see `set_current_suite`), so a
    // scenario that panics between pinning and clearing would otherwise hand the
    // next test a world running on a retired code point. Resetting here rather
    // than in each scenario's teardown means no test can forget.
    set_current_suite(None);
}

/// The main-DB handle — the very one the in-process DS reads and writes through.
///
/// Clients have no handle at all since #987: `pollis-core` does not link
/// `libsql`, so a stray client-side query is a compile error rather than
/// something a `query_only` PRAGMA has to catch at runtime. Tests that must seed
/// or mutate *server-side* state directly — backdating envelopes or clearing
/// watermarks to set up an envelope-GC scenario, tombstoning a revoked device's
/// `user_device` row — stand in for effects that happen server-side in
/// production (envelope GC, DS-driven device revocation), never for client
/// writes.
pub(crate) async fn writable_remote() -> Arc<Db> {
    world().await.remote.clone()
}

/// The commit-log handle, for the same reason and with the same caveat as
/// [`writable_remote`]. The three MLS control-plane tables live ONLY here.
pub(crate) async fn writable_log() -> Arc<Db> {
    world().await.log.clone()
}

/// Register a custom emoji in `group_id`, server-side (#917 / #848).
///
/// Emoji registration is a DS write backed by an R2 upload, and the flows world
/// has neither an R2 nor the re-encoder's input. Seeding the two rows the
/// upload would have produced — one `custom_emoji_object`, one `group_emoji` —
/// through the WRITABLE handle stands in for exactly that server-side effect,
/// the same way the envelope-GC scenarios seed backdated envelopes. It lets the
/// suite exercise the two READ paths, which is what #917 is about.
///
/// `r2_key` is deliberately stored WRONG (it does not match
/// `emoji/<hash>.<ext>`). `get_emoji_url` re-derives the key and refuses to
/// fetch on a mismatch, which gives the render path a deterministic terminal
/// error reached AFTER the object lookup and BEFORE any network call. That is
/// what makes a hermetic assertion about the render path possible at all: the
/// interesting question is never "did the bytes arrive", it is "was membership
/// consulted on the way", and this stops the test at the first point where the
/// answer is knowable.
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

/// Post-hoc commit-log GAP injection for the epoch-gap recovery scenario
/// (#430-P2 / invariant F1).
///
/// A live submit-path fault can't leave a durable *interior* gap: dropping the
/// head commit row merely lowers the head, and the next committer refills it. A
/// real gap — epochs `… N-1, [N missing], N+1 …` — persists only when a row is
/// removed AFTER a higher epoch has already been appended above it. So the gap
/// scenario lets every commit land normally, then deletes ONE interior row
/// directly on the LOG DB — the very handle the in-process DS writes through, so
/// the deletion is as authoritative as a DS write. A member catching up from
/// below then reads `N-1` immediately followed by `N+1`, trips the
/// append-only-log gap detector in `process_pending_commits`, drops its stale
/// local group, and must recover via external-join rather than wedge.
///
/// `conversation_id` is the MLS group id. For a group channel that is the
/// `group_id` (see `process_pending_commits`'s channel→group resolution in
/// `group_state.rs`), NOT the channel id.
///
/// Asserts loudly that exactly one row was removed — a silent no-op would mean
/// the scenario armed the wrong epoch and would prove nothing.
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

/// Post-hoc commit-BYTES corruption for the #680 deserialize-wedge scenario.
///
/// The analogue of [`drop_commit_row`], but instead of removing the row it
/// TRUNCATES its `commit_data` blob (a disk-bitrot / replication-fault shape) while
/// leaving the `epoch` column intact. A member replaying from below therefore
/// still classifies the row as the next commit to APPLY (the epoch-gap check
/// passes — the epoch is untouched), reaches `apply_one_commit`, and fails at
/// `MlsMessageIn::tls_deserialize`. On pre-#680 code that was a `Stop` that wedged
/// the member permanently (the group exists locally yet can never advance); after
/// #680 it is an explicit `Recover(MalformedCommit)` that external-joins onto the
/// head.
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
/// Asserts exactly one row was corrupted — a silent no-op would prove nothing.
///
/// IMPLEMENTATION NOTE — why this is a DELETE-then-INSERT, not a straight
/// `UPDATE ... SET commit_data`. Log-DB migration 000005 (#691) added
/// `trg_mls_commit_log_immutable`, a BEFORE UPDATE trigger that RAISE(ABORT)s any
/// UPDATE that changes `commit_data` — commit-payload immutability is the "rewrite"
/// half of invariant F2 and is the most valuable clause in that migration, so it
/// must NOT be weakened to accommodate this test. Instead we reproduce the exact
/// same observable end state — one INTERIOR row whose `commit_data` is a two-byte
/// stub, every other column (including `seq`, the catch-up order key, and the
/// untouched `epoch`) identical, the head unchanged — by deleting the row and
/// re-inserting it with the stub payload. That satisfies both new triggers as
/// written: the DELETE guard (`trg_..._keep_head`) only protects the live head, and
/// this row is provably interior (the caller asserts a higher epoch sits above it);
/// the INSERT guard (`trg_..._no_forward_gap`) only rejects `epoch > head + 1`, and
/// re-inserting a below-head epoch (the head row is never removed) is `epoch <= head`,
/// so it passes. Do NOT "simplify" this back into an UPDATE — you will hit a
/// confusing ABORT from the immutability trigger.
pub(crate) async fn corrupt_commit_row(conversation_id: &str, epoch: i64) {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for corrupt_commit_row");
    // Snapshot the full row first — we must re-insert it byte-for-byte except for
    // the payload, preserving `seq` so the row keeps its position in the seq-ordered
    // catch-up replay (a fresh AUTOINCREMENT seq would re-order the corrupt commit to
    // the tail and change what the replay observes).
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
    // ORDERING IS LOAD-BEARING — read every column out of `row` into a local BEFORE
    // the duplicate-row check below. A `libsql::Row` is a view over the statement's
    // current cursor position, not an owned snapshot of its values: calling
    // `rows.next()` again to look for a second row steps the statement and
    // invalidates the `Row` already handed out, after which every `row.get(i)` reads
    // back `NULL` (surfacing as a misleading `NullValue` on `seq`, an
    // `INTEGER PRIMARY KEY AUTOINCREMENT` that can never actually be NULL). Do NOT
    // "tidy" the assert back up next to its `next()` call — that reintroduces the
    // dead-cursor read and wedges `corrupt_commit_recovers_instead_of_wedging`.
    let seq: i64 = row.get(0).expect("seq");
    let generation: i64 = row.get(1).expect("generation");
    let sender_id: String = row.get(2).expect("sender_id");
    let created_at: String = row.get(3).expect("created_at");
    let added_user_id: Option<String> = row.get(4).expect("added_user_id");
    let added_device_ids: Option<String> = row.get(5).expect("added_device_ids");
    // Now safe to step the cursor: the duplicate-row guard catches a genuinely wrong
    // scenario setup (two commits at the same epoch/generation), but must run only
    // after the reads above.
    assert!(
        rows.next().await.expect("second row check").is_none(),
        "corrupt_commit_row: more than one commit at epoch {epoch} for {conversation_id} — \
         the scenario's setup is wrong (a duplicate row)"
    );

    // Truncate to a two-byte stub: too short to be a valid TLS-serialised
    // MlsMessageOut, so `tls_deserialize` fails outright. The row is interior, so the
    // DELETE never trips the head guard; the re-insert lands below the head, so it
    // never trips the forward-gap guard.
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

/// Prune the commit log below `floor` (EXCLUSIVE) via the REAL DS retention
/// DELETE (`pollis_delivery::commit::delete_commits_below`) — the exact statement
/// the event-driven prune runs (#539). Models a Tier-2 prune whose floor has
/// advanced PAST a straggler: a member below `floor` then reads an
/// earliest-available epoch above its own, trips the append-only gap detector in
/// `process_pending_commits`, and recovers via external-join. Returns the number
/// of commit rows pruned (asserted non-zero — a no-op would prove nothing).
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
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

/// Record a catch-up high-water at `since` for EVERY current member device of a
/// conversation, straight onto the DS DBs — a test stand-in for a forged/buggy
/// high-water that has already been recorded (the #681 attack's EFFECT; the
/// monotone store makes such a value sticky). Covering the whole roster is what
/// makes it dangerous: the floor then reads "fully reported" and Tier 1 is driven
/// off the injected value, so with `since` far above head only the retention
/// floor's head clamp stands between it and a full commit-log wipe. Membership is
/// read off MAIN exactly as `pollis_delivery::commit::prune_commit_log` does.
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
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

/// Run the REAL event-driven retention prune for a conversation
/// (`pollis_delivery::commit::prune_commit_log`) against the live DS DBs — the
/// exact call the DS makes on a commit append / catch-up report — and return its
/// report. Membership on MAIN, log on LOG, as production splits them.
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
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

/// The Delivery Service's head epoch for a conversation — `MAX(epoch) + 1` over
/// the commit log — read straight from the LOG DB via
/// `pollis_delivery::commit::head_epoch`. Convergence assertions use this to
/// confirm the group advanced (and past a dropped epoch) and that a returning
/// member reached the shared head.
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
pub(crate) async fn ds_head_epoch(conversation_id: &str) -> i64 {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for ds_head_epoch");
    pollis_delivery::commit::head_epoch(&conn, conversation_id)
        .await
        .expect("head_epoch")
}

/// The Delivery Service's head SUITE GENERATION for a conversation (#454 P4) —
/// `COALESCE(MAX(generation), 0)` over the commit log. 0 until the conversation
/// migrates off the classic suite, so every pre-P4 assertion reads the same.
///
/// `conversation_id` is the MLS group id (the `group_id` for a group channel).
pub(crate) async fn ds_head_generation(conversation_id: &str) -> i64 {
    let log = world().await.log.clone();
    let conn = log.conn().await.expect("log conn for ds_head_generation");
    pollis_delivery::commit::head_generation(&conn, conversation_id)
        .await
        .expect("head_generation")
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
    let timestamp = pollis_delivery::util::now_unix() as i64;
    let message = pollis_delivery::auth::canonical_message("POST", path, timestamp, body);

    let signature_b64 = {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("client local db open");
        let provider = pollis_lib::commands::mls::PollisProvider::new(db.conn());
        // DS request auth signs with the certified device identity's ML-DSA-44
        // half (#668 P5), mirroring `ds_client::ds_post`.
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

// ─── Client ─────────────────────────────────────────────────────────────────

/// One simulated device for one user. Holds a `MockRuntime` app with its own
/// isolated keystore + managed `AppState`. All clients in a given test share
/// the `Arc<Db>` on `TestWorld`, so they actually round-trip through
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
        Self::new_with_config(world().await.config.clone()).await
    }

    /// Shared constructor: build a client backed by an explicit Config. Sealed
    /// sender is UNCONDITIONAL (#607) — there is no per-client seal toggle to
    /// flip; every client seals every outbound envelope.
    async fn new_with_config(config: Config) -> Self {
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        // No database handle (#987). The `query_only_view` this used to pass was
        // the runtime gate that a client never wrote the shared DB directly;
        // `pollis-core` no longer links `libsql`, so the same guarantee — now
        // covering READS too — holds at compile time, and
        // `pollis-core/tests/no_client_side_remote_reads.rs` pins it.
        let state = Arc::new(AppState::new_with_parts(config, keystore));
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

    /// Run the cold-launch / post-reconnect MLS sweep for this client — catches
    /// every group + DM up to head and runs the eviction/remove reconcile
    /// backstop (issue #430 P1). Real clients fire this at AppShell mount.
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

    /// Create an additional text channel in a group and return its ID. All
    /// channels in a group share the group's single MLS group (`mls_group_id ==
    /// group_id`), so this is how a test builds the multi-channel topology needed
    /// to exercise cross-channel epoch interactions.
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

    /// Send a reply INTO a thread (#825). `thread_id` is the root message's
    /// ULID. Returns the new message so a caller can assert on its id.
    ///
    /// Takes `conversation_id` like every other send here: the local `message`
    /// table keys channels and DMs on the same column, which is precisely why
    /// threads are expected to behave identically in both.
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

    /// Replies this device holds for one thread, oldest first.
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

    /// Reply counts per thread for one conversation (channel id or DM id).
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

    /// Delete a message the caller sent — "delete for everyone". Sends the E2EE
    /// redaction and removes the original envelope; recipients soft-delete their
    /// local copy on next ingest.
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

    /// Fetch the FULL message history for a channel, following the cursor across
    /// every page. The production read (`get_channel_messages`) is paginated
    /// (default page 50, newest-first); reading only the first page would make an
    /// oracle over a long history (e.g. the 500-op marathon, which sends >50
    /// messages) falsely report the OLDEST messages as lost for a member who has
    /// been present since the start — they're simply below the first page. The
    /// model's bulletproof-delivery assertion needs the whole history, so page
    /// through to the end.
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

    /// Give this client a media-server port and token (#917).
    ///
    /// `get_emoji_url` bails immediately when the loopback media server has not
    /// started, which every flows client is — so without this the render path
    /// would fail before reaching any authorization-relevant code and a test
    /// asserting on it would prove nothing. The values are never connected to;
    /// they only have to exist for the command to proceed to the object lookup.
    pub(crate) async fn arm_media_server(&self) {
        *self.state.media_server_port.lock().await = Some(1);
        *self.state.media_server_token.lock().await = Some("test-token".to_string());
    }

    /// Return the (user_id, role) pairs for every current member of a group.
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

// ─── Attacker model: a stolen leaf (issue #666) ──────────────────────────────

/// A passive adversary holding an exfiltrated copy of one device's MLS state.
///
/// Everything openmls keeps for a device — leaf secret, epoch secrets, ratchet
/// state, the group itself — lives in the local `mls_kv` table, so copying that
/// table IS the compromise. The copy is planted in a *separate* client's local
/// DB, which is what makes the adversary strong enough to be worth testing: it
/// gets its own state that evolves independently of the victim's, so nothing it
/// can (or cannot) read is an artefact of sharing the victim's live database.
///
/// Crucially the attacker **follows the commit chain**. A snapshot that only ever
/// decrypted at its stolen epoch would be locked out by *any* epoch change, and a
/// lockout assertion over it would prove nothing about rotation. A real adversary
/// holding the victim's leaf key can decrypt the path secrets in every *other*
/// member's commit — those commits are addressed to the copath, which contains the
/// victim's leaf — and so rides the group forward indefinitely. Only a commit by
/// the victim itself cuts it off, because a committer's own leaf is the one
/// position an UpdatePath carries no ciphertext for. That gap is exactly what
/// issue #666's self-update closes, and `catch_up` is what forces the test to
/// prove it rather than assume it.
///
/// Passive is the honest scope. `mls_kv` also holds the device's *signing* key,
/// so an adversary willing to act could authenticate to the DS and external-join
/// as the victim — but that is a full device impersonation, and no amount of key
/// rotation defends against it. Post-compromise security is the claim that a
/// *listener* is evicted once the victim rotates, and that is what this models.
pub(crate) struct StolenLeaf {
    /// The attacker's own client. Its identity is its own (it never authenticates
    /// as the victim); only the MLS key material is the victim's.
    client: TestClient,
    victim: String,
}

/// Exfiltrate `victim`'s entire MLS state as of right now onto an attacker-owned
/// device.
///
/// The attacker signs up so it has an unlocked local DB to plant the stolen rows
/// in; that signup is a bystander account which never joins any group. Its own
/// `mls_kv` rows are dropped first — a stolen device holds the victim's key
/// material and nothing else.
pub(crate) async fn steal_leaf(victim: &TestClient) -> StolenLeaf {
    let mut client = TestClient::new().await;
    client.sign_up("attacker@test.local").await;

    // Columns are read as dynamically-typed `Value`s because `mls_kv` genuinely
    // holds a mix: openmls' own rows bind `scope` as the empty *blob*, while
    // sibling modules writing through `raw_conn` bind it as text, and SQLite
    // stores whatever it was given regardless of the declared affinity. Copying
    // `Value`s keeps every row byte- and type-identical to the victim's.
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

    // Adopt the victim's (user_id, device_id) as the identity this state signs
    // and reads AS.
    //
    // Not an escalation: `mls_kv` — which we just copied wholesale — is where the
    // device's ML-DSA-44 request-signing key lives, so a copy of it IS a copy of
    // the credential. What changed in #987 is where that matters. Before it, a
    // client could fetch the commit log with the whole-database read token every
    // binary carried, so an attacker holding only `mls_kv` caught up without ever
    // presenting a credential; the harness could model that by leaving the
    // attacker signed in as itself. There is no ambient read capability any more
    // — the DS answers `conversation-state` for the VERIFIED signer and nobody
    // else — so catching up now requires presenting the stolen device's identity,
    // which the stolen bytes already permit.
    //
    // The adversary stays PASSIVE: it reads and decrypts, and never commits,
    // writes, or external-joins. Device impersonation is a different attack that
    // no key rotation defends against, and the roster/eviction tests cover it.
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
    /// Ride the commit chain forward as far as the stolen keys allow.
    ///
    /// Runs the real client replay (`process_pending_commits_inner`) as the
    /// victim, which is what an adversary running the stolen device's own binary
    /// would do. It succeeds through other members' commits and stalls at the
    /// victim's own — the whole point of the model, so the result is deliberately
    /// ignored here and judged by what [`can_read`](Self::can_read) sees.
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

    /// Can the attacker still read `body` out of `conversation_id`?
    ///
    /// Catches up first, then runs the real production decrypt
    /// (`try_mls_decrypt`) against the stolen state over every envelope the DS
    /// holds for the conversation — the exact capability a wiretap plus a stolen
    /// device would have.
    ///
    /// Both ids are needed because they are not the same key: envelopes are
    /// stored per *conversation* (a channel id, for group traffic), while the MLS
    /// group every channel in a group shares is keyed by the group id. For a DM
    /// the two coincide. Mirrors `send_message`'s own `mls_group_id` lookup.
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
            // Plaintext is padded before sealing, so the body is a substring.
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

/// Every MLS ciphertext the DS is holding for `conversation_id`, decoded from
/// the `mls:<hex>` envelope framing. This is the wiretap half of the attacker.
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

// ─── Suite generations / suite migration (#454 P4, #669) ────────────────────

/// MLS code point `0x0001` (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`) —
/// the suite Pollis shipped before #454, retired by #669.
///
/// The migration scenarios need *a* suite that is not the current one, and this
/// is the honest choice: it is the only other suite this build's provider
/// implements, and it differs from `CS_PQ` in every dimension (KEM, AEAD, KDF,
/// signature scheme), so a group born on it exercises the boundary rather than a
/// relabelling.
pub(crate) const SUITE_LEGACY: u16 = 0x0001;

/// Pin the suite this process treats as current, or `None` for the build's own
/// (`CS_PQ`).
///
/// #669 left production with exactly one suite, which means the only way a group
/// can end up on a *non*-current suite is a change to the `CS_PQ` constant — a
/// renumber of the provisional `draft-ietf-mls-pq-ciphersuites` code point, which
/// has already happened once. A test cannot wait for that, so `pollis-core`
/// exposes the constant as a `test-harness`-only seam and this is the handle on
/// it: pin `SUITE_LEGACY` before the fleet signs up to get a world running on the
/// old number, then clear it and let each client's next login
/// ([`republish_key_packages`]) and sweep carry the world across, which is
/// exactly the two steps a real renumber would take.
///
/// Global, not per-client, because a code point is a property of the *build* —
/// there is no production world in which two clients disagree about it. Flows
/// tests are `#[serial]`, and [`wipe`] clears it, so a scenario that panics
/// mid-migration cannot leak its pin into the next one.
pub(crate) fn set_current_suite(code_point: Option<u16>) {
    pollis_lib::commands::mls::set_current_suite_override(code_point);
}

/// Republish this device's KeyPackage pool in whatever suite is current now.
///
/// This is production's own `ensure_mls_key_package`, the same call
/// `initialize_identity` makes at login — not a harness shortcut. It is what a
/// user does by updating their app and signing in, and it is the *first* of the
/// two steps that carry a fleet onto a new code point: until every roster device
/// has a KeyPackage in the target suite, `migrate_to_current_suite_if_due`
/// refuses to move the group at all rather than leave one behind.
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

/// This device's local suite generation for `conversation_id` (#454 P4) — which
/// lineage it believes it is on. 0 until it adopts a successor.
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

/// The ciphersuite of the `GroupInfo` the Delivery Service currently publishes
/// for `conversation_id`, as a raw RFC 9420 code point.
///
/// Read off the server-visible blob rather than asked of a client, so it proves
/// what actually travelled: `0x0001` = classic
/// (`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`), `0x0052` = the PQ suite
/// (`MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`).
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

/// Every undelivered-or-delivered `Welcome` blob the Delivery Service holds for
/// `conversation_id`, oldest first.
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

/// Ceiling for a commit that adds nobody, in bytes.
///
/// This is the number that must NOT grow with the roster. A self-update or a
/// remove carries an UpdatePath and nothing else, so its size is logarithmic in
/// the tree once every leaf is merged (#666) — `pollis-core`'s
/// `pq_payloads_stay_under_their_ceilings` pins the same 20 KiB for an 8-member
/// merged commit on the hybrid suite.
pub(crate) const COMMIT_CEILING_NO_ADDS: usize = 20_480;

/// Ceiling for ONE added member's inlined KeyPackage, in bytes.
///
/// `add_members` puts Add proposals in the commit **by value**, so a commit that
/// admits `k` members carries `k` KeyPackages. On the hybrid suite a KeyPackage
/// is the most expensive routine object there is — two X-Wing public keys, a
/// 1,312 B ML-DSA-44 signature key and a 2,420 B signature — and `pollis-core`
/// asserts it stays under 12 KiB. Measured today it is ~8.7 KiB, so this leaves
/// roughly 40% headroom without stopping being a bound.
pub(crate) const COMMIT_CEILING_PER_ADD: usize = 12_288;

/// Check every stored commit for `conversation_id` against the budget its own
/// contents justify, returning a description of the worst offender.
///
/// The distinction this draws is the whole point. Two commits obey completely
/// different growth laws, and collapsing them into one number is what let a
/// roster-linear commit hide:
///
/// - A commit that adds nobody must be **logarithmic** in the roster
///   ([`COMMIT_CEILING_NO_ADDS`]). This is the property #666 exists to defend,
///   and it is the one where a regression means "a ratchet tree or a Welcome got
///   inlined".
/// - A commit that adds `k` members is **linear in `k` by construction**
///   ([`COMMIT_CEILING_PER_ADD`] each), because it must convey each new member's
///   KeyPackage. The migration opening commit is the extreme case: `super::
///   migrate` builds the successor as a group of one and reconciles the whole
///   roster into it, so a single commit carries `roster - 1` KeyPackages.
///
/// Checked per row rather than against `MAX(LENGTH(...))` so the budget can
/// depend on what that specific commit admitted.
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
        // `added_metadata` writes the added device ids as a comma-separated list,
        // and NULL when the commit added nobody.
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
    /// Rotate this device's own leaf in `conversation_id` (issue #666).
    ///
    /// Calls production's [`self_update_group`] directly rather than through an
    /// `invoke` shim because there is no user-facing command for it: rotation is
    /// driven by the welcome poller on join and by the cold-launch sweep on a
    /// timer. Tests need to place it at an exact point in a sequence, which
    /// neither of those affords.
    ///
    /// Returns whether a commit actually landed (`false` = another member won
    /// the epoch race, which the caller must treat as "not yet rotated").
    pub(crate) async fn self_update(&self, conversation_id: &str) -> bool {
        self.try_self_update(conversation_id)
            .await
            .unwrap_or_else(|e| panic!("self_update_group({conversation_id}): {e}"))
    }

    /// Fallible [`self_update`](Self::self_update), for the fuzzer.
    ///
    /// The landing DS faults are supposed to recover *internally* (the command
    /// still returns `Ok`), so an `Err` here is a finding — and a generated case
    /// needs to report it with its op sequence attached rather than panic out of
    /// the proptest closure with no repro.
    pub(crate) async fn try_self_update(&self, conversation_id: &str) -> Result<bool, String> {
        let user_id = self.user_id().to_string();
        pollis_lib::commands::mls::self_update_group(&self.state, conversation_id, &user_id)
            .await
            .map_err(|e| e.to_string())
    }
}
