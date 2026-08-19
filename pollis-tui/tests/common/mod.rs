//! In-box, Tauri-free test rig for the pollis-tui data/sync core.
//!
//! This is the flows harness (`src-tauri/tests/flows/harness.rs`) distilled to
//! exactly what a two-client "A sends, B receives" scenario needs, and with the
//! Tauri layer removed: clients call `pollis_core::commands::*` **directly** (the
//! way the TUI does), not through a `MockRuntime` webview + `invoke`.
//!
//! Shared world, mirroring production `pollis_delivery::AppState { db, log_db }`:
//! - ONE main `Db` (users / groups / DMs / envelopes / auth) and ONE for the
//!   commit log (`mls_commit_log` / `mls_welcome` / `mls_group_info`) — two
//!   genuinely separate libsql files, so a misrouted query fails loudly (the
//!   #420 split). Both belong to the DS.
//! - ONE in-process `pollis-delivery` server, bound on loopback, that every
//!   client read AND write routes through. Since #987 a client has no database
//!   handle at all — `pollis-core` does not link `libsql` — so "everything went
//!   through the DS" is a compile-time fact rather than something a `query_only`
//!   PRAGMA had to catch at runtime.
//!
//! Each `TestClient` gets its OWN `AppState` + `InMemoryKeystore`, exactly like
//! the flows `TestClient`.

#![allow(dead_code)]

mod driver;
// Each `*_smoke.rs` binary includes this `common` module but only `ui_e2e` uses
// the driver, so the re-export is dead in the others — allow it (mirrors the
// module-wide `dead_code` allowance for the same reason).
#[allow(unused_imports)]
pub use driver::Driver;

use std::sync::Arc;

use pollis_core::accounts;
use pollis_core::commands::{auth, dm, messages, pin};
use pollis_core::config::Config;
use pollis_delivery::db::Db;
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

async fn bootstrap_schema(conn: &libsql::Connection) -> anyhow::Result<()> {
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
        run_sql_script(conn, BASELINE_SQL).await?;
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

async fn bootstrap_log_schema(conn: &libsql::Connection) -> anyhow::Result<()> {
    run_sql_script(conn, LOG_DB_SCHEMA).await?;
    // Post-baseline log-DB migrations (mirrors the flows harness + db-apply.sh's
    // second apply). Without these the log DB is missing e.g. migration 000002's
    // `mls_welcome` UNIQUE index, so the DS's idempotent welcome upsert
    // (`ON CONFLICT`) errors and the recipient never gets welcomed (#487). The
    // log DB is fresh per test and each migration is idempotent, so apply
    // unconditionally.
    for (_version, _description, sql) in POST_BASELINE_LOG_MIGRATIONS {
        run_sql_script(conn, sql).await?;
    }
    Ok(())
}

/// Drop the three MLS control-plane tables from MAIN so a misrouted main-side
/// read of them fails loudly (they live only on the log DB now).
async fn drop_log_tables_from_main(conn: &libsql::Connection) -> anyhow::Result<()> {
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

/// Split on `;`, honoring `--` line comments, `'...'` string literals, and
/// `CREATE TRIGGER ... BEGIN <stmt>; END` bodies (whose inner `;` are not
/// statement boundaries). All appear in the migrations. Copied from the flows
/// harness.
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
                // A `CREATE TRIGGER` body carries its own statement-terminating
                // `;` (`BEGIN <stmt>; END`). Those inner `;` are NOT statement
                // boundaries — the trigger ends only at the `;` after its `END`.
                let upper = current.trim().to_ascii_uppercase();
                if upper.starts_with("CREATE TRIGGER") && !upper.ends_with("END") {
                    current.push(c);
                } else {
                    let stmt = current.trim().to_string();
                    if !stmt.is_empty() {
                        statements.push(stmt);
                    }
                    current.clear();
                }
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
    pub main: Arc<Db>,
    pub log: Arc<Db>,
    pub config: Config,
    // Kept alive so the temp dir (per-user DBs + libsql files) survives the test.
    _tmp: std::path::PathBuf,
}

impl World {
    /// Carve out a per-device data dir under the world's temp dir. Two
    /// devices of the SAME user MUST NOT share a data dir — their local SQLCipher
    /// DB (`pollis_{user_id}.db`), file keystore, and `accounts.json` all key off
    /// data dir, so a shared dir would have the second device clobber the
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
    // SQLCipher DB path derives from the data dir — keyed per user, so all
    // clients can share one dir.
    //
    // `set_data_dir`, never `std::env::set_var`: this rig runs background sync
    // tasks and an in-process DS on other threads, and mutating the process
    // environment while any of them might be reading it is a data race that
    // shows up as a rare load-dependent failure and nothing else (#923).
    //
    // There is no `DEV_OTP` here either. The DS half takes the code as an
    // explicit `OtpConfig { dev_otp }` (see `spawn_in_process_delivery`) and the
    // client half is handed `DEV_OTP` literally at every `verify_otp` call, so
    // the environment was never actually in the loop — only in the race.
    pollis_core::db::local::set_data_dir(&tmp);

    let main = Arc::new(
        Db::connect_local(tmp.join("test_turso.db").to_str().expect("utf-8 path"))
            .await
            .expect("connect main libsql"),
    );
    bootstrap_schema(&main.conn().await.expect("main conn"))
        .await
        .expect("bootstrap main schema");

    let log = Arc::new(
        Db::connect_local(tmp.join("test_log.db").to_str().expect("utf-8 path"))
            .await
            .expect("connect log libsql"),
    );
    bootstrap_log_schema(&log.conn().await.expect("log conn"))
        .await
        .expect("bootstrap log schema");
    drop_log_tables_from_main(&main.conn().await.expect("main conn"))
        .await
        .expect("drop log tables from main");

    let delivery_url = spawn_in_process_delivery(main.clone(), log.clone()).await;

    // Config literal: R2/livekit fields are placeholders (nothing here touches
    // them); only the DS URL is real — since #987 it is the only backend there
    // is.
    let config = Config {
        r2_endpoint: String::new(),
        r2_public_url: String::new(),
        livekit_url: String::new(),
        pollis_delivery_url: Some(delivery_url),
        // Overlay off: the smoke rig talks to the in-process DS directly.
        overlay_mode: pollis_core::config::OverlayMode::Off,
        overlay_relay_url: None,
        overlay_relay_cert: None,
        overlay_directory_url: None,
        overlay_directory_key: None,
    };

    World {
        main,
        log,
        config,
        _tmp: tmp,
    }
}

// ─── In-process Delivery Service ──────────────────────────────────────────────

/// Boot the REAL `pollis-delivery` router on a dedicated OS thread + runtime, so
/// the server outlives the per-test runtime.
///
/// # Why this is the real router now (#987)
///
/// This used to be ~900 lines of hand-rolled handlers: a second implementation
/// of every endpoint the DM path touched, wired to the same `apply_*` functions
/// but with its own routing table, its own auth glue and its own response
/// bodies. It was already the wrong shape — a test double of a server is exactly
/// where a hand-written literal drifts from the server without anything noticing
/// — and #987 made it untenable: with every client READ now a `POST /v1/read/…`,
/// the double would have had to grow twenty more handlers whose only job was to
/// agree with the originals.
///
/// So it is gone, and this rig mounts `pollis_delivery::build_router_with_state`
/// exactly as the flows harness does. Anything the TUI exercises is served by
/// the code that serves it in production.
async fn spawn_in_process_delivery(main: Arc<Db>, log: Arc<Db>) -> String {
    use std::sync::mpsc;

    // Auth ENFORCED, like production and like the flows harness: the smokes
    // drive the signed path end to end, so a regression in request signing or
    // server-side authz fails here rather than in a release.
    let state = pollis_delivery::AppState::new_with_log_db(main, log, true)
        .with_otp_config(pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
        })
        // Every request arrives from 127.0.0.1, so the production per-IP limits
        // would read one test run as a single abusive client. Raised, not
        // disabled — `ratelimit.rs`'s own unit tests pin the real numbers.
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

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::Builder::new()
        .name("in-process-delivery".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .expect("delivery: build runtime");
            rt.block_on(async move {
                let router = pollis_delivery::build_router_with_state(state);
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
    /// When set, this device's own data dir — repointed (via [`use_dir`])
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
            keystore,
        ));
        Self {
            state,
            profile: None,
            data_dir: None,
        }
    }

    /// Point the process's data dir at this client's own, if it has one. Called at
    /// the start of every keystore/DB-touching path so a second device of the same
    /// user reads/writes its OWN on-disk state. No-op for shared-dir clients.
    ///
    /// Which device is current is still one process-wide value — two devices of
    /// one user genuinely cannot share a directory — but it is swapped through
    /// [`pollis_core::db::local::set_data_dir`], which is a locked write, rather
    /// than through `std::env::set_var`, which is a data race against every other
    /// thread's `getenv` and was the suspected cause of #923.
    fn use_dir(&self) {
        if let Some(dir) = &self.data_dir {
            pollis_core::db::local::set_data_dir(dir);
        }
    }

    /// Build a fresh, signed-out client whose keystore is the **file-backed**
    /// `default_os_keystore` (persistent under the data dir) rather than the
    /// in-memory one. Needed by the restart/resync gate: the identity + session
    /// must survive a `drop` of the `AppState`, which only a file keystore does.
    /// Media/os-keystore are off under pollis-tui's build, so `default_os_keystore`
    /// resolves to the file JSON store (spec §5) — no dbus, zero extra deps.
    pub fn new_persistent(world: &World) -> Self {
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            default_os_keystore(),
        ));
        Self {
            state,
            profile: None,
            data_dir: None,
        }
    }

    /// Like [`new_persistent`], but pinned to its OWN data dir (a subdir
    /// `name` under the world's temp dir). Required whenever two clients belong to
    /// the SAME user (the multi-device enrollment + Secret-Key recovery smokes):
    /// each device needs an isolated local DB + keystore + accounts index. The dir
    /// is repointed just-in-time by [`use_dir`] before every on-disk touch.
    pub fn new_persistent_in(world: &World, name: &str) -> Self {
        let dir = world.device_dir(name);
        // Build the keystore under THIS device's dir (the file keystore resolves
        // the data dir on every operation).
        pollis_core::db::local::set_data_dir(&dir);
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            default_os_keystore(),
        ));
        Self {
            state,
            profile: None,
            data_dir: Some(dir),
        }
    }

    /// Simulate a quit→relaunch: drop the current `AppState` and rebuild a NEW
    /// one on the SAME data dir + same libsql handles, with a FRESH
    /// `default_os_keystore`. The profile is retained in-memory only as the
    /// test's record of "who this device is" — the rebuilt state knows nothing
    /// until `auth::boot` rehydrates it from the persisted accounts index +
    /// keystore. Re-activates this user first so the accounts index's
    /// `last_active_user` points back at them (a prior client's `activate` may
    /// have moved it), which is what `get_session` keys off.
    pub fn restart(&mut self, world: &World) {
        self.activate();
        // Rebuild the keystore against THIS device's dir; `activate` already
        // repointed the process at it via `use_dir`.
        // Replace the Arc: the old AppState (and its file keystore handle) drops
        // when the last reference goes. The rebuilt state reads the same on-disk
        // keystore + local SQLCipher DB the previous instance wrote.
        self.state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
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
        // Repoint the data dir FIRST so the accounts-index write below (and
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
        // Repoint the data dir before verify_otp generates identity material +
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
            // reply_to_id / thread_id: an ordinary channel message.
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
