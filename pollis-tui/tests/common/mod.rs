//! In-box, Tauri-free test rig for the pollis-tui data/sync core.
//!
//! The flows harness distilled to a two-client scenario, calling `pollis_core::commands::*`
//! directly.
//!
//! Separate main and commit-log libsql files, so a misrouted query fails, behind one in-process
//! DS that every client goes through (#987).

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

/// verify-otp accepts a known code.
pub const DEV_OTP: &str = "000000";
/// Fixed PIN so every client's local SQLCipher DB is open after signup.
pub const TEST_PIN: &str = "0000";

/// The three MLS control-plane tables that live ONLY on the commit-log DB.
const LOG_TABLES: [&str; 3] = ["mls_commit_log", "mls_welcome", "mls_group_info"];

async fn bootstrap_schema(conn: &libsql::Connection) -> anyhow::Result<()> {
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
        run_sql_script(conn, sql).await?;
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
    // Post-baseline log-DB migrations; without them the welcome upsert fails (#487).
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

/// `CREATE TRIGGER ... BEGIN <stmt>; END` bodies. Copied from the flows harness.
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
                // A trigger body's inner `;` is not a statement boundary; it ends at the `;`
                // after `END`.
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

/// The shared backend for one test: two libsql files (main + log), the config
/// pointing at the in-process DS, and the temp dir backing per-user DBs.
pub struct World {
    pub main: Arc<Db>,
    pub log: Arc<Db>,
    pub config: Config,
    // Kept alive so the temp dir (per-user DBs + libsql files) survives the test.
    _tmp: std::path::PathBuf,
}

impl World {
    /// Carve out a per-device data dir under the world's temp dir. Two
    /// devices of the SAME user must not share one: their DB and keystore key off it.
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
    // Per-user DB paths, so clients share one dir. `set_data_dir`, never `std::env::set_var`,
    // which races the rig's threads (#923).
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

    // Only the DS URL is real; it is the only backend (#987).
    let config = Config {
        r2_endpoint: String::new(),
        r2_public_url: String::new(),
        livekit_url: String::new(),
        pollis_delivery_url: Some(delivery_url),
        overlay_mode: pollis_core::config::OverlayMode::Off,
        overlay_relay_url: None,
        overlay_relay_cert: None,
        overlay_directory_url: None,
        overlay_directory_key: None,
        dev_access_key: None,
    };

    World {
        main,
        log,
        config,
        _tmp: tmp,
    }
}

/// Boot the REAL `pollis-delivery` router on a dedicated OS thread + runtime, so
/// the server outlives the per-test runtime.
///
/// The real router, as the flows harness mounts it, rather than a hand-rolled test double (#987).
async fn spawn_in_process_delivery(main: Arc<Db>, log: Arc<Db>) -> String {
    use std::sync::mpsc;

    // Auth enforced, like production, so a signing or authz regression fails here.
    let state = pollis_delivery::AppState::new_with_log_db(main, log, true)
        .with_otp_config(pollis_delivery::otp::OtpConfig {
            resend_api_key: None,
            dev_otp: Some(DEV_OTP.to_string()),
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 0,
            max_attempts: 5,
            // Fixed mailboxes would trip the per-mailbox budget; raised, not disabled.
            max_sends_per_window: u32::MAX,
            ..pollis_delivery::otp::OtpConfig::default()
        })
        // Every request comes from 127.0.0.1; raised, not disabled.
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

/// One simulated device: its own `AppState` and `InMemoryKeystore` on the shared world.
pub struct TestClient {
    pub state: Arc<AppState>,
    pub profile: Option<auth::UserProfile>,
    /// When set, this device's own data dir — repointed (via [`use_dir`])
    /// before any on-disk touch; `None` uses the shared dir. Two devices of one user need their
    /// own.
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

    /// Point the process's data dir at this client's own, if any, via the locked `set_data_dir`
    /// (#923).
    fn use_dir(&self) {
        if let Some(dir) = &self.data_dir {
            pollis_core::db::local::set_data_dir(dir);
        }
    }

    /// Build a fresh, signed-out client whose keystore is the **file-backed**
    /// `default_os_keystore`, so identity and session survive dropping the `AppState`.
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
    /// `name`), for two clients of the SAME user.
    pub fn new_persistent_in(world: &World, name: &str) -> Self {
        let dir = world.device_dir(name);
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
    /// one on the same data dir with a fresh `default_os_keystore`, re-activating this user first.
    pub fn restart(&mut self, world: &World) {
        self.activate();
        // The old AppState drops; the rebuilt one reads the same on-disk state.
        self.state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            default_os_keystore(),
        ));
    }

    pub fn user_id(&self) -> &str {
        &self.profile.as_ref().expect("not signed in").id
    }

    /// Point the process-global active user (accounts.json `last_active_user`) at
    /// THIS client.
    pub fn activate(&self) {
        // Repoint the data dir first so the accounts-index write lands here.
        self.use_dir();
        if let Some(p) = &self.profile {
            let _ = accounts::upsert_account(&p.id, &p.username, None);
        }
    }

    /// First-device signup in the §7 order via the pollis-tui auth wrappers.
    pub async fn sign_up(&mut self, email: &str) -> auth::UserProfile {
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

    /// Create a 2-person DM to `other`, returning its id.
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
            None,
            username,
            &self.state,
        )
        .await
        .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
    }

    /// Send through the TUI's own write layer (`pollis_tui::send::send_text`).
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

    /// The conversation ids this device currently sees.
    pub async fn conversation_ids(&self) -> Vec<String> {
        self.activate();
        pollis_tui::data::load_conversations(&self.state, self.user_id())
            .await
            .expect("load_conversations")
            .conversation_ids()
    }

    /// A fresh device signing in against an existing account (`enrollment_required = true`).
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

    /// New device: kick off an enrollment request.
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

    /// Existing device: approve a pending request with the code the approver
    /// TYPED (#1096); in a test, the value the new device displays.
    pub async fn approve_enrollment(&self, request_id: &str, verification_code: &str) {
        self.try_approve_enrollment(request_id, verification_code)
            .await
            .expect("approve_enrollment");
    }

    /// [`Self::approve_enrollment`] without the `expect`, for the negative case:
    /// a wrong code must be refused.
    pub async fn try_approve_enrollment(
        &self,
        request_id: &str,
        verification_code: &str,
    ) -> anyhow::Result<()> {
        self.activate();
        pollis_tui::enroll::approve(
            &self.state,
            request_id.to_string(),
            verification_code.to_string(),
        )
        .await
    }

    /// New device: complete an enrollment/recovery after the account key is in
    /// `AppState.unlock`: `set_pin` → `finalize` → `initialize_identity`, like desktop.
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

    /// Poll to approval, then finish; resolves on the first poll.
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
    /// (into `AppState.unlock`), then runs the enrollment tail.
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
