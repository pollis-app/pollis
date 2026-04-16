//! End-to-end integration harness for Pollis.
//!
//! Drives the real `#[tauri::command]` functions through the tauri IPC
//! pipeline — no `_inner` shims, no mocked DB layer. Each [`TestClient`]
//! owns its own `App<MockRuntime>` backed by its own `InMemoryKeystore`,
//! while all clients share a single [`TestWorld`] pointed at a disposable
//! test Turso instance (`.env.test`).
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
use pollis_lib::test_harness::{bootstrap_schema, build_client_app, invoke, wipe_remote};
use serde_json::json;
use serial_test::serial;
use tauri::test::MockRuntime;
use tauri::{App, WebviewWindow};
use tokio::sync::OnceCell;

const DEV_OTP: &str = "000000";

// ─── World ──────────────────────────────────────────────────────────────────

/// Shared across all clients in a single test. Owns the connection to the
/// disposable test Turso and a temp dir that backs per-user SQLCipher files.
///
/// Construction is lazy + process-wide so integration tests share one
/// connection pool but still run serially (the wipe would race otherwise).
struct TestWorld {
    remote: Arc<RemoteDb>,
    config: Config,
}

static WORLD: OnceCell<TestWorld> = OnceCell::const_new();

async fn world() -> &'static TestWorld {
    WORLD
        .get_or_init(|| async {
            // Loads .env.test and bypasses R2/LiveKit/Resend with placeholders.
            let config = Config::for_test().expect("Config::for_test");

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

            let remote = Arc::new(
                RemoteDb::connect(&config.turso_url, &config.turso_token)
                    .await
                    .expect("connect test turso"),
            );

            bootstrap_schema(&remote)
                .await
                .expect("bootstrap test turso schema");

            TestWorld { remote, config }
        })
        .await
}

async fn wipe() {
    let w = world().await;
    wipe_remote(&w.remote).await.expect("wipe test turso");
}

// ─── Client ─────────────────────────────────────────────────────────────────

/// One simulated device for one user. Holds a `MockRuntime` app with its own
/// isolated keystore + managed `AppState`. All clients in a given test share
/// the `Arc<RemoteDb>` on `TestWorld`, so they actually round-trip through
/// the same test Turso DB the way real clients round-trip through production
/// Turso.
struct TestClient {
    /// App must outlive the webview — keep it alive for the lifetime of the
    /// client.
    _app: App<MockRuntime>,
    webview: WebviewWindow<MockRuntime>,
    #[allow(dead_code)]
    state: Arc<AppState>,
    /// Populated after `sign_up` / `sign_in`. Commands like `create_group`
    /// need this to identify the caller.
    profile: Option<UserProfile>,
}

impl TestClient {
    /// Build a fresh client. Does NOT sign in — call [`sign_up`] after
    /// construction.
    async fn new() -> Self {
        let w = world().await;
        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(
            w.config.clone(),
            w.remote.clone(),
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
    /// Populates `self.profile` and warms the local DB so downstream commands
    /// work. Returns the final profile.
    async fn sign_up(&mut self, email: &str) -> UserProfile {
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

    fn user_id(&self) -> &str {
        &self.profile.as_ref().expect("not signed in").id
    }

    async fn invoke_json(&self, cmd: &str, args: serde_json::Value) -> serde_json::Value {
        invoke(&self.webview, cmd, args)
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }
}

// ─── Smoke test ─────────────────────────────────────────────────────────────

/// Minimal end-to-end path: one client signs up, creates a group, and can
/// list it back. Validates the whole stack — Config → AppState → RemoteDb →
/// keystore → MLS init → Turso round-trip — on real code paths.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn single_client_signup_and_create_group() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    assert_eq!(profile.email, "alice@test.local");
    assert!(!profile.id.is_empty());
    assert!(!profile.enrollment_required, "new user should not need enrollment");

    let group: serde_json::Value = alice
        .invoke_json(
            "create_group",
            json!({ "name": "Test Group", "description": null, "ownerId": alice.user_id() }),
        )
        .await;
    assert_eq!(group["name"], "Test Group");
    assert_eq!(group["owner_id"], alice.user_id());

    let groups: serde_json::Value = alice
        .invoke_json("list_user_groups", json!({ "userId": alice.user_id() }))
        .await;
    let groups = groups.as_array().expect("groups array");
    assert_eq!(groups.len(), 1, "should see the group we just created");
    assert_eq!(groups[0]["name"], "Test Group");

    // Force the borrow checker to keep the client alive past the final
    // assertion (otherwise it may be dropped early in some builds, closing
    // the local DB handle mid-assertion).
    drop(alice);
}

/// Two distinct clients in the same test process. Proves:
///   - Per-client `InMemoryKeystore`s keep sessions isolated.
///   - Both clients round-trip through the shared test Turso.
///   - `search_user_by_username` from one client resolves a user registered
///     by another — the basic read-after-write contract the invite / DM
///     flows are built on.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn two_clients_see_each_other_via_turso() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Distinct user IDs — keystores are independent.
    assert_ne!(alice_profile.id, bob_profile.id);

    // Bob can find Alice via the real search command.
    let hit: serde_json::Value = bob
        .invoke_json(
            "search_user_by_username",
            json!({ "username": alice_profile.username.clone() }),
        )
        .await;
    assert_eq!(hit["id"], alice_profile.id);
    assert_eq!(hit["username"], alice_profile.username);

    // And vice versa — proves the shared remote is symmetric, not just
    // single-writer.
    let hit: serde_json::Value = alice
        .invoke_json(
            "search_user_by_username",
            json!({ "username": bob_profile.username.clone() }),
        )
        .await;
    assert_eq!(hit["id"], bob_profile.id);

    drop(alice);
    drop(bob);
}
