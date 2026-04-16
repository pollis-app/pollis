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

// ─── Helpers for multi-client scenarios ─────────────────────────────────────

impl TestClient {
    /// Drain any pending MLS welcomes queued for this client on Turso. Real
    /// clients call this on login and when the livekit inbox pings them; the
    /// harness drives it explicitly. Scoped to welcomes only — commits are
    /// per-channel (see [`process_commits_for`]).
    async fn poll(&self) {
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
    async fn process_commits_for(&self, channel_id: &str) {
        let _: serde_json::Value = invoke(
            &self.webview,
            "process_pending_commits",
            json!({ "conversationId": channel_id, "userId": self.user_id() }),
        )
        .await
        .unwrap_or_else(|e| panic!("process_pending_commits({channel_id}): {e}"));
    }

    async fn create_group(&self, name: &str) -> String {
        let g: serde_json::Value = self
            .invoke_json(
                "create_group",
                json!({
                    "name": name,
                    "description": null,
                    "ownerId": self.user_id(),
                }),
            )
            .await;
        g["id"].as_str().expect("group id").to_string()
    }

    async fn invite(&self, group_id: &str, invitee_identifier: &str) {
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
    async fn first_pending_invite(&self) -> Option<serde_json::Value> {
        let invites: serde_json::Value = self
            .invoke_json("get_pending_invites", json!({ "userId": self.user_id() }))
            .await;
        invites
            .as_array()
            .and_then(|arr| arr.first().cloned())
    }

    async fn accept_invite(&self, invite_id: &str) {
        self.invoke_json(
            "accept_group_invite",
            json!({ "inviteId": invite_id, "userId": self.user_id() }),
        )
        .await;
    }

    async fn decline_invite(&self, invite_id: &str) {
        self.invoke_json(
            "decline_group_invite",
            json!({ "inviteId": invite_id, "userId": self.user_id() }),
        )
        .await;
    }

    async fn list_group_ids(&self) -> Vec<String> {
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

    async fn group_member_ids(&self, group_id: &str) -> Vec<String> {
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

    async fn list_group_channels(&self, group_id: &str) -> Vec<serde_json::Value> {
        let channels: serde_json::Value = self
            .invoke_json("list_group_channels", json!({ "groupId": group_id }))
            .await;
        channels.as_array().expect("channels array").clone()
    }

    /// Return the #General text channel ID for a group.
    async fn general_channel_id(&self, group_id: &str) -> String {
        self.list_group_channels(group_id)
            .await
            .into_iter()
            .find(|c| c["channel_type"] == "text")
            .expect("a text channel")["id"]
            .as_str()
            .expect("channel id")
            .to_string()
    }

    async fn request_group_access(&self, group_id: &str) {
        self.invoke_json(
            "request_group_access",
            json!({ "groupId": group_id, "requesterId": self.user_id() }),
        )
        .await;
    }

    async fn list_join_requests(&self, group_id: &str) -> Vec<serde_json::Value> {
        let reqs: serde_json::Value = self
            .invoke_json(
                "get_group_join_requests",
                json!({ "groupId": group_id, "requesterId": self.user_id() }),
            )
            .await;
        reqs.as_array().expect("requests array").clone()
    }

    async fn approve_join_request(&self, request_id: &str) {
        self.invoke_json(
            "approve_join_request",
            json!({ "requestId": request_id, "approverId": self.user_id() }),
        )
        .await;
    }

    async fn reject_join_request(&self, request_id: &str) {
        self.invoke_json(
            "reject_join_request",
            json!({ "requestId": request_id, "approverId": self.user_id() }),
        )
        .await;
    }

    async fn remove_member(&self, group_id: &str, target_user_id: &str) {
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

    async fn create_dm(&self, other_user_ids: &[&str]) -> String {
        let members: Vec<&str> = other_user_ids.to_vec();
        let dm: serde_json::Value = self
            .invoke_json(
                "create_dm_channel",
                json!({ "creatorId": self.user_id(), "memberIds": members }),
            )
            .await;
        dm["id"].as_str().expect("dm id").to_string()
    }

    async fn list_dm_requests(&self) -> Vec<serde_json::Value> {
        let dms: serde_json::Value = self
            .invoke_json("list_dm_requests", json!({ "userId": self.user_id() }))
            .await;
        dms.as_array().expect("dm requests array").clone()
    }

    async fn list_dms(&self) -> Vec<serde_json::Value> {
        let dms: serde_json::Value = self
            .invoke_json("list_dm_channels", json!({ "userId": self.user_id() }))
            .await;
        dms.as_array().expect("dm channels array").clone()
    }

    async fn accept_dm_request(&self, dm_channel_id: &str) {
        self.invoke_json(
            "accept_dm_request",
            json!({ "dmChannelId": dm_channel_id, "userId": self.user_id() }),
        )
        .await;
    }

    async fn block(&self, blocked_user_id: &str) {
        self.invoke_json(
            "block_user",
            json!({ "blockerId": self.user_id(), "blockedId": blocked_user_id }),
        )
        .await;
    }

    /// Try to invoke `send_message`, returning the error string if it failed.
    async fn try_send_message(
        &self,
        conversation_id: &str,
        content: &str,
    ) -> Result<serde_json::Value, String> {
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

    async fn send_channel_message(&self, conversation_id: &str, content: &str) {
        self.try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
    }

    async fn fetch_channel_messages(&self, channel_id: &str) -> Vec<serde_json::Value> {
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
}

// ─── Scenarios ──────────────────────────────────────────────────────────────

/// Alice invites Bob by username. Bob sees the pending invite, accepts it,
/// and appears in the group member list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_invite_accept_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Invite Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob
        .first_pending_invite()
        .await
        .expect("bob should see one pending invite");
    assert_eq!(invite["group_id"], group_id);
    assert_eq!(invite["inviter_id"], alice_profile.id);

    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    // Bob should no longer see a pending invite.
    assert!(bob.first_pending_invite().await.is_none());

    // Both appear in the member list.
    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&alice_profile.id));
    assert!(ids.contains(&bob_profile.id));
    assert_eq!(ids.len(), 2);

    // Bob's own group list now includes the group.
    assert!(bob.list_group_ids().await.contains(&group_id));

    drop(alice);
    drop(bob);
}

/// Bob declines Alice's invite. The invite row goes away and Bob does not
/// become a member.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_invite_decline_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Decline Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob.first_pending_invite().await.expect("one pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();

    bob.decline_invite(&invite_id).await;

    assert!(bob.first_pending_invite().await.is_none());

    // Membership unchanged — only Alice.
    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id]);
    assert!(!bob.list_group_ids().await.contains(&group_id));
    let _ = bob_profile;

    drop(alice);
    drop(bob);
}

/// Carol finds Alice's group by slug, requests access, and Alice (admin)
/// approves. Carol's request becomes non-pending and Carol gains membership.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_join_request_approve_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Joinable").await;

    carol.request_group_access(&group_id).await;

    // Admin sees one pending request for this group.
    let requests = alice.list_join_requests(&group_id).await;
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["requester_id"], carol_profile.id);
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    // Non-admins get an empty list (role gate in get_group_join_requests).
    assert!(carol.list_join_requests(&group_id).await.is_empty());

    alice.approve_join_request(&request_id).await;
    carol.poll().await;

    // Request no longer pending for the admin.
    assert!(alice.list_join_requests(&group_id).await.is_empty());

    // Carol is a member; Alice still is.
    let ids = alice.group_member_ids(&group_id).await;
    assert!(ids.contains(&alice_profile.id));
    assert!(ids.contains(&carol_profile.id));
    assert_eq!(ids.len(), 2);

    drop(alice);
    drop(carol);
}

/// Carol's request is rejected. She does not become a member and the pending
/// list clears.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_join_request_reject_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Not For You").await;

    carol.request_group_access(&group_id).await;
    let requests = alice.list_join_requests(&group_id).await;
    let request_id = requests[0]["id"].as_str().expect("request id").to_string();

    alice.reject_join_request(&request_id).await;

    assert!(alice.list_join_requests(&group_id).await.is_empty());
    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id]);
    assert!(!carol.list_group_ids().await.contains(&group_id));
    let _ = carol_profile;

    drop(alice);
    drop(carol);
}

/// After Alice removes Bob from the group, Bob no longer appears in the
/// member list and the group drops off his own list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn removed_member_loses_access() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Kick Test").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite = bob.first_pending_invite().await.expect("pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    assert!(bob.list_group_ids().await.contains(&group_id));

    alice.remove_member(&group_id, &bob_profile.id).await;

    let ids = alice.group_member_ids(&group_id).await;
    assert_eq!(ids, vec![alice_profile.id.clone()]);
    assert!(!bob.list_group_ids().await.contains(&group_id));

    drop(alice);
    drop(bob);
}

/// Alice creates a DM to Bob. Bob sees it as a pending request (not in his
/// accepted DM list). After accepting, it moves to his DM channel list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_request_accept_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()]).await;

    // Alice created — it's accepted on her side (she sees it as a channel).
    let alice_channels = alice.list_dms().await;
    assert_eq!(alice_channels.len(), 1);
    assert_eq!(alice_channels[0]["id"], dm_id);

    // Bob sees it as a pending request, not an accepted channel.
    let bob_requests = bob.list_dm_requests().await;
    assert_eq!(bob_requests.len(), 1);
    assert_eq!(bob_requests[0]["id"], dm_id);
    assert!(bob.list_dms().await.is_empty());

    bob.accept_dm_request(&dm_id).await;

    // After accept, it's in Bob's channels and gone from his requests.
    assert!(bob.list_dm_requests().await.is_empty());
    let bob_channels = bob.list_dms().await;
    assert_eq!(bob_channels.len(), 1);
    assert_eq!(bob_channels[0]["id"], dm_id);

    drop(alice);
    drop(bob);
}

/// If Bob blocks Alice before she creates a DM, Alice's create_dm_channel
/// should fail with the generic BLOCK_ERR message — the block is
/// indistinguishable from any other "pending" state to the sender.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn block_prevents_dm_creation() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    bob.block(&alice_profile.id).await;

    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "create_dm_channel",
        json!({
            "creatorId": alice_profile.id,
            "memberIds": [alice_profile.id.clone(), bob_profile.id.clone()],
        }),
    )
    .await
    .err()
    .expect("create_dm_channel should fail when blocked");
    assert!(
        err.contains("message request pending"),
        "expected BLOCK_ERR, got: {err}"
    );

    // And no DM rows on either side.
    assert!(alice.list_dms().await.is_empty());
    assert!(alice.list_dm_requests().await.is_empty());
    assert!(bob.list_dms().await.is_empty());
    assert!(bob.list_dm_requests().await.is_empty());

    drop(alice);
    drop(bob);
}

/// End-to-end crypto round-trip: after Bob accepts Alice's invite, Alice
/// sends a channel message. Bob fetches the channel via `get_channel_messages`
/// and sees the decrypted plaintext — proving the MLS handshake + encrypt +
/// decrypt path works through the real command pipeline.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn channel_message_round_trip() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Crypto").await;
    alice.invite(&group_id, &bob_profile.username).await;

    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;

    alice.send_channel_message(&channel_id, "hello bob").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        contents.contains(&"hello bob"),
        "bob should decrypt alice's message, got: {bob_msgs:#?}"
    );

    drop(alice);
    drop(bob);
}
