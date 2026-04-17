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

    /// Like `send_channel_message` but returns the new message's ID so the
    /// caller can later edit or delete it.
    async fn send_channel_message_id(&self, conversation_id: &str, content: &str) -> String {
        let msg = self
            .try_send_message(conversation_id, content)
            .await
            .unwrap_or_else(|e| panic!("send_message({conversation_id}): {e}"));
        msg["id"].as_str().expect("message id").to_string()
    }

    /// Edit a previously-sent message. Republishes the ciphertext at the
    /// current MLS epoch and replaces any prior edit envelope.
    async fn edit_message(&self, conversation_id: &str, message_id: &str, new_content: &str) {
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

    /// Change a member's role in a group (`"admin"` or `"member"`).
    async fn set_member_role(&self, group_id: &str, target_user_id: &str, role: &str) {
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
    async fn group_member_roles(&self, group_id: &str) -> Vec<(String, String)> {
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

/// Scales the group up from 1 → 9 members and back down to 1, sending a
/// labeled message after every membership change. Verifies:
///   - After each add, every current member (including the newcomer) can
///     decrypt the subsequently-sent message. Newcomers cannot decrypt
///     messages that were sent before they joined — MLS Welcomes do not
///     carry history, so `content` comes back as null for pre-join envelopes.
///   - After each remove, the removed member can no longer decrypt any
///     subsequent message (their epoch is stale; their keys don't open the
///     new commit).
///   - Every surviving member's epoch ratchet stays consistent: they keep
///     decrypting every post-change message, proving that commit processing
///     (add and remove) advances their local MLS state correctly.
///   - The creator can finish the test alone and still send + decrypt —
///     the group state survives the full shrink.
///
/// This is the main stress test for MLS epoch ratcheting under heavy
/// membership churn.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn epoch_ratchet_nine_users_add_remove() {
    wipe().await;

    // Nine fully-signed-up clients. Index 0 is the creator and stays for
    // the whole test; indices 1..9 are added in order and then removed in
    // reverse order.
    let mut clients: Vec<TestClient> = Vec::with_capacity(9);
    let mut profiles: Vec<UserProfile> = Vec::with_capacity(9);
    for i in 0..9 {
        let mut c = TestClient::new().await;
        let p = c.sign_up(&format!("user{}@test.local", i + 1)).await;
        profiles.push(p);
        clients.push(c);
    }

    let group_id = clients[0].create_group("Ratchet").await;
    let channel_id = clients[0].general_channel_id(&group_id).await;

    // ── Growth phase: 2 → 9 members ──
    for n in 1..9 {
        // Creator invites user n+1.
        let invitee_username = profiles[n].username.clone();
        clients[0].invite(&group_id, &invitee_username).await;

        // Invitee accepts + applies their Welcome.
        let invite = clients[n]
            .first_pending_invite()
            .await
            .unwrap_or_else(|| panic!("user{} should have pending invite", n + 1));
        let invite_id = invite["id"].as_str().expect("invite id").to_string();
        clients[n].accept_invite(&invite_id).await;
        clients[n].poll().await;

        // Already-members apply the add commit so their local epoch advances.
        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        // Creator sends a step-labeled message.
        let label = format!("msg-{n}");
        clients[0].send_channel_message(&channel_id, &label).await;

        // Every current member decrypts the latest message.
        for k in 0..=n {
            let msgs = clients[k].fetch_channel_messages(&channel_id).await;
            let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&label.as_str()),
                "user{} should decrypt '{}' at growth step {}, got: {:?}",
                k + 1,
                label,
                n,
                contents
            );
        }

        // Newcomer cannot decrypt any of the prior messages — they joined
        // after those were encrypted to older epochs.
        if n >= 2 {
            let newcomer_msgs = clients[n].fetch_channel_messages(&channel_id).await;
            for prior in 1..n {
                let prior_label = format!("msg-{prior}");
                let hit = newcomer_msgs
                    .iter()
                    .any(|m| m["content"].as_str() == Some(prior_label.as_str()));
                assert!(
                    !hit,
                    "user{} should NOT decrypt pre-join message '{}'",
                    n + 1,
                    prior_label
                );
            }
        }
    }

    // ── Shrink phase: 9 → 1 members ──
    // Remove users 9, 8, ..., 2 (user 0 is creator and stays). After each
    // removal, send a new message and assert (a) every remaining member
    // decrypts it, (b) the just-removed user cannot.
    for n in (1..9).rev() {
        let target_id = profiles[n].id.clone();
        clients[0].remove_member(&group_id, &target_id).await;

        // Remaining members apply the remove commit.
        for k in 0..n {
            clients[k].process_commits_for(&channel_id).await;
        }

        let label = format!("post-remove-{}", n + 1);
        clients[0].send_channel_message(&channel_id, &label).await;

        // Remaining members decrypt.
        for k in 0..n {
            let msgs = clients[k].fetch_channel_messages(&channel_id).await;
            let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&label.as_str()),
                "user{} should decrypt '{}' after removing user{}, got: {:?}",
                k + 1,
                label,
                n + 1,
                contents
            );
        }

        // Removed user cannot decrypt the post-removal message.
        let removed_msgs = clients[n].fetch_channel_messages(&channel_id).await;
        let removed_contents: Vec<&str> = removed_msgs
            .iter()
            .filter_map(|m| m["content"].as_str())
            .collect();
        assert!(
            !removed_contents.contains(&label.as_str()),
            "removed user{} should NOT decrypt '{}', got: {:?}",
            n + 1,
            label,
            removed_contents
        );
    }

    // Creator alone at epoch N can still send + decrypt.
    clients[0]
        .send_channel_message(&channel_id, "alone-again")
        .await;
    let msgs = clients[0].fetch_channel_messages(&channel_id).await;
    let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        contents.contains(&"alone-again"),
        "creator alone should decrypt 'alone-again', got: {contents:?}"
    );

    drop(clients);
}

/// Seeded xorshift64 — deterministic and dependency-free. We intentionally
/// avoid pulling `StdRng` in from `rand` here so changes to its default
/// feature set can't silently re-seed this scenario.
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // 0 is a fixed point for xorshift; nudge it if the caller passes 0.
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Uniform `[0, n)`.
    fn gen_range(&mut self, n: usize) -> usize {
        assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

/// Seeded random churn across membership and role changes. At every step the
/// harness verifies that:
///   - `get_group_members` on the creator exactly matches the scenario's
///     expected membership and per-member admin flag.
///   - A message sent by the current admin or a current non-admin member
///     is decryptable by every present member and opaque to every
///     non-present client (including members removed earlier in the run).
///
/// This catches subtle regressions the linear growth test can't hit:
///   - An admin's epoch becoming stale across role toggles.
///   - An add/remove/add sequence failing to advance a long-present member's
///     ratchet.
///   - `set_member_role` silently mutating membership state.
///
/// Uses a fixed xorshift seed; failure logs print the full op sequence via
/// `eprintln!` so a red run is reproducible.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn random_admin_and_membership_churn() {
    wipe().await;

    const NUM_OTHERS: usize = 5;
    const MAX_GROUP_SIZE: usize = 5;
    const NUM_OPS: usize = 30;
    const SEED: u64 = 0xC0FFEE_BEEF_F00D;

    // Index 0 is the creator; 1..=5 are other pre-signed-up clients.
    let mut clients: Vec<TestClient> = Vec::with_capacity(NUM_OTHERS + 1);
    let mut profiles: Vec<UserProfile> = Vec::with_capacity(NUM_OTHERS + 1);
    for i in 0..=NUM_OTHERS {
        let mut c = TestClient::new().await;
        let p = c.sign_up(&format!("churn{}@test.local", i)).await;
        profiles.push(p);
        clients.push(c);
    }

    let group_id = clients[0].create_group("Churn").await;
    let channel_id = clients[0].general_channel_id(&group_id).await;

    // Tracked expected state: who is a member, who is an admin. The creator
    // is always both and stays in the group.
    use std::collections::BTreeSet;
    let mut members: BTreeSet<usize> = BTreeSet::from([0]);
    let mut admins: BTreeSet<usize> = BTreeSet::from([0]);

    // Ordered log of ops executed, printed on failure for reproducibility.
    let mut op_log: Vec<String> = Vec::new();
    let mut msg_counter: u32 = 0;

    let mut rng = XorShift64::new(SEED);

    for op_idx in 0..NUM_OPS {
        // Pick a candidate op; skip if it can't apply.
        let pick = rng.gen_range(6);
        let op_name: &'static str = match pick {
            0 => "add",
            1 => "remove",
            2 => "promote",
            3 => "demote",
            4 => "send_from_admin",
            _ => "send_from_member",
        };

        // Precompute the population each op needs so we can skip cleanly.
        let non_members: Vec<usize> = (1..=NUM_OTHERS).filter(|i| !members.contains(i)).collect();
        let removable: Vec<usize> = members.iter().copied().filter(|&i| i != 0).collect();
        let promotable: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !admins.contains(i))
            .collect();
        let demotable: Vec<usize> = admins.iter().copied().filter(|&i| i != 0).collect();
        let current_admins: Vec<usize> = admins.iter().copied().collect();
        let current_non_admin_members: Vec<usize> = members
            .iter()
            .copied()
            .filter(|i| !admins.contains(i))
            .collect();

        match op_name {
            "add" => {
                if members.len() >= MAX_GROUP_SIZE || non_members.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip add (size={})", members.len()));
                    continue;
                }
                let idx = non_members[rng.gen_range(non_members.len())];
                let invitee_username = profiles[idx].username.clone();
                clients[0].invite(&group_id, &invitee_username).await;
                let invite = clients[idx]
                    .first_pending_invite()
                    .await
                    .unwrap_or_else(|| {
                        eprintln!("op_log so far: {op_log:#?}");
                        panic!("op {op_idx}: user{idx} should have a pending invite")
                    });
                let invite_id = invite["id"].as_str().expect("invite id").to_string();
                clients[idx].accept_invite(&invite_id).await;
                clients[idx].poll().await;

                // Every already-member (including creator) applies the add commit.
                for &k in members.iter() {
                    clients[k].process_commits_for(&channel_id).await;
                }
                // Newcomer also processes commits — `poll` applied the Welcome but
                // later commits could have landed before this point in a real run.
                clients[idx].process_commits_for(&channel_id).await;

                members.insert(idx);
                op_log.push(format!("{op_idx:02}: add user{idx}"));
            }
            "remove" => {
                if removable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip remove (no removable)"));
                    continue;
                }
                let idx = removable[rng.gen_range(removable.len())];
                clients[0].remove_member(&group_id, profiles[idx].id.as_str()).await;

                members.remove(&idx);
                admins.remove(&idx);

                for &k in members.iter() {
                    clients[k].process_commits_for(&channel_id).await;
                }
                op_log.push(format!("{op_idx:02}: remove user{idx}"));
            }
            "promote" => {
                if promotable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip promote (no promotable)"));
                    continue;
                }
                let idx = promotable[rng.gen_range(promotable.len())];
                clients[0]
                    .set_member_role(&group_id, profiles[idx].id.as_str(), "admin")
                    .await;
                admins.insert(idx);
                op_log.push(format!("{op_idx:02}: promote user{idx}"));
            }
            "demote" => {
                if demotable.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip demote (no demotable)"));
                    continue;
                }
                let idx = demotable[rng.gen_range(demotable.len())];
                clients[0]
                    .set_member_role(&group_id, profiles[idx].id.as_str(), "member")
                    .await;
                admins.remove(&idx);
                op_log.push(format!("{op_idx:02}: demote user{idx}"));
            }
            "send_from_admin" => {
                if current_admins.is_empty() {
                    op_log.push(format!("{op_idx:02}: skip send_from_admin (impossible)"));
                    continue;
                }
                let idx = current_admins[rng.gen_range(current_admins.len())];
                msg_counter += 1;
                let label = format!("op{op_idx:02}-admin{idx}-m{msg_counter}");
                clients[idx].send_channel_message(&channel_id, &label).await;
                verify_message_visibility(
                    &clients, &members, &label, op_idx, &op_log, &channel_id,
                )
                .await;
                op_log.push(format!("{op_idx:02}: send_from_admin user{idx} -> {label}"));
            }
            "send_from_member" => {
                if current_non_admin_members.is_empty() {
                    op_log.push(format!(
                        "{op_idx:02}: skip send_from_member (no non-admin members)"
                    ));
                    continue;
                }
                let idx = current_non_admin_members
                    [rng.gen_range(current_non_admin_members.len())];
                msg_counter += 1;
                let label = format!("op{op_idx:02}-member{idx}-m{msg_counter}");
                clients[idx].send_channel_message(&channel_id, &label).await;
                verify_message_visibility(
                    &clients, &members, &label, op_idx, &op_log, &channel_id,
                )
                .await;
                op_log.push(format!("{op_idx:02}: send_from_member user{idx} -> {label}"));
            }
            _ => unreachable!(),
        }

        // After every op, verify membership + role state matches expectations.
        let observed = clients[0].group_member_roles(&group_id).await;
        let observed_ids: BTreeSet<String> =
            observed.iter().map(|(id, _)| id.clone()).collect();
        let observed_admin_ids: BTreeSet<String> = observed
            .iter()
            .filter(|(_, r)| r == "admin")
            .map(|(id, _)| id.clone())
            .collect();

        let expected_ids: BTreeSet<String> =
            members.iter().map(|&i| profiles[i].id.clone()).collect();
        let expected_admin_ids: BTreeSet<String> =
            admins.iter().map(|&i| profiles[i].id.clone()).collect();

        if observed_ids != expected_ids {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx} ({op_name}): group membership mismatch\n  expected: {expected_ids:?}\n  observed: {observed_ids:?}"
            );
        }
        if observed_admin_ids != expected_admin_ids {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx} ({op_name}): admin set mismatch\n  expected: {expected_admin_ids:?}\n  observed: {observed_admin_ids:?}"
            );
        }
    }

    eprintln!("churn op log ({} ops):", op_log.len());
    for line in &op_log {
        eprintln!("  {line}");
    }

    drop(clients);
}

/// Shared assertion for send-from-admin / send-from-member ops in the churn
/// scenario: every current member must decrypt the message; no non-member
/// may see its plaintext.
async fn verify_message_visibility(
    clients: &[TestClient],
    members: &std::collections::BTreeSet<usize>,
    label: &str,
    op_idx: usize,
    op_log: &[String],
    channel_id: &str,
) {
    for k in 0..clients.len() {
        let msgs = clients[k].fetch_channel_messages(channel_id).await;
        let contents: Vec<&str> = msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        let has = contents.contains(&label);
        if members.contains(&k) {
            if !has {
                eprintln!("op log: {op_log:#?}");
                panic!(
                    "op {op_idx}: member user{k} should decrypt '{label}', got: {contents:?}"
                );
            }
        } else if has {
            eprintln!("op log: {op_log:#?}");
            panic!(
                "op {op_idx}: non-member user{k} should NOT decrypt '{label}', got: {contents:?}"
            );
        }
    }
}

/// Exercises `edit_message` across MLS membership changes. Three phases in
/// one scenario:
///
/// 1. **Add-then-edit**: after alice+bob are in the group, alice sends
///    "hello"; bob fetches and sees it. Carol is invited and joins; alice
///    then edits the message. Bob and carol both see the edited content on
///    their next fetch. Carol never saw "hello" (Welcomes don't carry
///    history) — that's fine; we only assert she can decrypt the edit once
///    she's a member because `edit_message` re-encrypts at the post-add
///    epoch.
/// 2. **Remove-then-edit**: alice removes carol, then edits again. Bob
///    picks up the new edit; carol does not — the edit envelope is
///    encrypted to an epoch she's no longer in, so her local cache is
///    frozen at whatever she last decrypted.
/// 3. **Edit → add → edit**: final sanity check that edits written across
///    a second add advance too. Alice edits ("v2"), adds a new user, then
///    edits again ("v3"). All current members converge on "v3", proving
///    the second edit's ciphertext targets the correct (post-add) epoch
///    and that a stale first edit cannot linger and win — `edit_message`
///    does a DELETE+INSERT so only the latest envelope survives.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn edit_message_across_membership_changes() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Edit Churn").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // ── Phase 1: add-then-edit ──
    // alice + bob are in the group; alice sends "hello".
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;

    let msg_id = alice
        .send_channel_message_id(&channel_id, "hello")
        .await;

    // Bob can decrypt the original pre-add.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"hello"),
        "bob should decrypt 'hello' before carol is added, got: {bob_contents:?}"
    );

    // Now add carol and advance everyone's epoch.
    alice.invite(&group_id, &carol_profile.username).await;
    let invite_id = carol
        .first_pending_invite()
        .await
        .expect("carol invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    carol.accept_invite(&invite_id).await;
    carol.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Alice edits; the edit envelope is encrypted at the 3-member epoch.
    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited")
        .await;

    // Bob's local cache flips to the edited content.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob should still see the original row");
    assert_eq!(
        bob_msg["content"].as_str(),
        Some("HELLO edited"),
        "bob should see alice's edit after a member was added"
    );
    assert!(
        bob_msg["edited_at"].as_str().is_some(),
        "bob's view should carry an edited_at timestamp"
    );

    // Carol fetches too. She never had a local row for `msg_id` (the
    // original envelope was encrypted at the pre-join epoch she can't
    // decrypt), and the current edit-apply path only UPDATEs an existing
    // row — so carol's view of this specific message is null content.
    // The important property here is that she must NOT have recovered the
    // pre-join plaintext "hello".
    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents: Vec<&str> = carol_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents.contains(&"hello"),
        "carol should not see the pre-join plaintext 'hello', got: {carol_contents:?}"
    );

    // ── Phase 2: remove-then-edit ──
    // Snapshot carol's view BEFORE removal so we can confirm her cache
    // freezes after she loses access.
    let carol_view_before_remove: Vec<(String, Option<String>)> = carol
        .fetch_channel_messages(&channel_id)
        .await
        .iter()
        .map(|m| {
            (
                m["id"].as_str().expect("id").to_string(),
                m["content"].as_str().map(str::to_owned),
            )
        })
        .collect();

    alice.remove_member(&group_id, &carol_profile.id).await;
    bob.process_commits_for(&channel_id).await;

    // Alice publishes a second edit at the post-remove epoch.
    alice
        .edit_message(&channel_id, &msg_id, "HELLO edited again")
        .await;

    // Bob follows the edit.
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_msg = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id)
        .expect("bob still sees the row");
    assert_eq!(
        bob_msg["content"].as_str(),
        Some("HELLO edited again"),
        "bob should follow the post-remove edit"
    );

    // Carol cannot decrypt the new envelope; her local cache is frozen at
    // whatever she last saw. She definitely must not see the new content.
    let carol_msgs_after = carol.fetch_channel_messages(&channel_id).await;
    let carol_contents_after: Vec<&str> = carol_msgs_after
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        !carol_contents_after.contains(&"HELLO edited again"),
        "removed carol should NOT see the post-remove edit, got: {carol_contents_after:?}"
    );
    // Her view of msg_id either retains the prior cached value or is None,
    // but never the new content.
    let prior = carol_view_before_remove
        .iter()
        .find(|(id, _)| id == &msg_id)
        .and_then(|(_, c)| c.clone());
    let now = carol_msgs_after
        .iter()
        .find(|m| m["id"] == msg_id)
        .and_then(|m| m["content"].as_str().map(str::to_owned));
    assert_ne!(
        now.as_deref(),
        Some("HELLO edited again"),
        "carol's row for the edited message must not show the post-remove edit"
    );
    // If carol had cached the first edit, she should still have it — removal
    // shouldn't retroactively wipe what's already decrypted locally.
    if prior.as_deref() == Some("HELLO edited") {
        assert_eq!(
            now.as_deref(),
            Some("HELLO edited"),
            "carol's cached plaintext should not regress after removal"
        );
    }

    // ── Phase 3: edit → add → edit ──
    // Fresh message on the current (post-remove) epoch.
    let msg_id_v = alice
        .send_channel_message_id(&channel_id, "v1")
        .await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.edit_message(&channel_id, &msg_id_v, "v2").await;

    // Add a brand-new user dave; advance everyone's epoch.
    let mut dave = TestClient::new().await;
    let dave_profile = dave.sign_up("dave@test.local").await;
    alice.invite(&group_id, &dave_profile.username).await;
    let invite_id = dave
        .first_pending_invite()
        .await
        .expect("dave invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    dave.accept_invite(&invite_id).await;
    dave.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    // Second edit at the post-add epoch — must overwrite the first edit,
    // which is still sitting in `message_envelope`. DELETE+INSERT inside
    // edit_message guarantees only "v3" survives remotely, so a member
    // whose local cache holds v2 must converge on v3 (never see "v2" or
    // the original "v1" after fetching).
    alice.edit_message(&channel_id, &msg_id_v, "v3").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_row = bob_msgs
        .iter()
        .find(|m| m["id"] == msg_id_v)
        .expect("bob row exists");
    assert_eq!(
        bob_row["content"].as_str(),
        Some("v3"),
        "post-add edit must overwrite the prior edit for a member that had the original"
    );

    // Dave — like carol earlier — has no local row for the pre-join
    // message, so his fetch returns content=None and the prior edits
    // can't "leak through" as cached plaintext. The invariant we do
    // care about is that he never observes a stale prior edit as
    // plaintext.
    let dave_msgs = dave.fetch_channel_messages(&channel_id).await;
    let dave_contents: Vec<&str> = dave_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    for stale in ["v1", "v2"] {
        assert!(
            !dave_contents.contains(&stale),
            "dave must never see stale pre-join plaintext '{stale}', got: {dave_contents:?}"
        );
    }

    drop(alice);
    drop(bob);
    drop(carol);
    drop(dave);
}

/// Count remaining envelopes for a conversation via a direct libsql query.
/// Bypasses any Tauri command so we observe the raw row state.
async fn envelope_count(remote: &Arc<pollis_lib::db::remote::RemoteDb>, conversation_id: &str) -> i64 {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .expect("count query");
    let row = rows.next().await.expect("row").expect("some row");
    row.get::<i64>(0).expect("count")
}

/// Backdate every envelope in a conversation to a known timestamp. Used to
/// simulate old envelopes without waiting 30 days. Applies to envelopes of
/// any type.
async fn backdate_envelopes(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
    conversation_id: &str,
    sent_at: &str,
) {
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE message_envelope SET sent_at = ?1 WHERE conversation_id = ?2",
        libsql::params![sent_at.to_string(), conversation_id.to_string()],
    )
    .await
    .expect("backdate envelopes");
}

/// Wipe conversation watermarks for a specific conversation so the test can
/// reconstruct a known lag pattern. Add-member seeding would otherwise leave
/// recent watermarks that mask the behavior we want to exercise.
async fn clear_watermarks(
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
    conversation_id: &str,
) {
    let conn = remote.conn().await.expect("remote conn");
    conn.execute(
        "DELETE FROM conversation_watermark WHERE conversation_id = ?1",
        libsql::params![conversation_id.to_string()],
    )
    .await
    .expect("clear watermarks");
}

/// Exercises the two independent gates in `get_channel_messages`' envelope
/// cleanup: the 30-day TTL and the all-members-caught-up watermark. They're
/// OR'd, so either alone is sufficient to delete. The scenario drives
/// three cases, each time triggering cleanup by having a member fetch:
///
/// - **Negative**: young envelope, only the sender has fetched. Neither
///   gate fires — envelope stays.
/// - **Watermark-only**: young envelope, both members have fetched past it.
///   Watermark gate fires even though TTL is far from expired.
/// - **TTL-only**: envelope backdated past 30 days while watermarks are
///   deliberately left in a state where the watermark gate cannot fire
///   (one member's row is absent, so the CASE returns NULL). TTL gate
///   fires alone and the envelope is deleted.
///
/// The backdating + watermark hacks poke the remote DB directly — there's
/// no production command that lets a test manipulate `sent_at` or
/// `conversation_watermark`, and we intentionally don't add one.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn envelope_cleanup_ttl_or_watermark() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Cleanup").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let channel_id = alice.general_channel_id(&group_id).await;
    alice.process_commits_for(&channel_id).await;

    let remote = alice.state.remote_db.clone();

    // ── Negative: young envelope, only sender fetched ──
    // Wipe watermarks first so add-member's seeded "now" rows don't satisfy
    // the watermark gate on their own.
    clear_watermarks(&remote, &channel_id).await;

    alice.send_channel_message(&channel_id, "neg-hello").await;
    // Alice fetches — triggers cleanup. Bob has not fetched. The cleanup
    // subquery requires every current member to have a watermark row; bob
    // does not, so the watermark gate returns NULL. TTL is fresh (< 30
    // days). Neither gate fires — envelope stays.
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        1,
        "young envelope with a lagging member should not be cleaned up"
    );

    // ── Watermark-only: young envelope, both watermarks strictly past it ──
    // The cleanup query uses `sent_at < MIN(cw)`, and the watermark upsert
    // uses the latest returned message's `sent_at`. So to delete "neg-hello"
    // via the watermark gate we need a STRICTLY later message that both
    // members have fetched — that later message's sent_at becomes the new
    // watermark, and neg-hello's sent_at becomes strictly less than it.
    bob.fetch_channel_messages(&channel_id).await;
    alice.send_channel_message(&channel_id, "neg-hello-2").await;
    alice.fetch_channel_messages(&channel_id).await;
    bob.fetch_channel_messages(&channel_id).await;
    // Both watermarks now sit at sent_at("neg-hello-2"), strictly greater
    // than sent_at("neg-hello"). The next cleanup trigger evicts the older
    // envelope but leaves the newer one (whose sent_at equals MIN).
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        1,
        "older envelope should be cleaned once every watermark passes it, while the latest envelope remains"
    );

    // ── TTL-only: old envelope, watermark gate deliberately broken ──
    // Send a fresh envelope, then backdate it past the 30-day TTL.
    alice.send_channel_message(&channel_id, "very-old").await;
    // Rewind sent_at into the past — well beyond the 30-day threshold.
    backdate_envelopes(&remote, &channel_id, "2020-01-01T00:00:00+00:00").await;
    // Wipe watermarks again so the gate cannot accidentally fire: alice's
    // upsert during her fetch will re-create her row (set to the backdated
    // sent_at), but bob will be missing until he fetches.  `COUNT(gm) !=
    // COUNT(cw)` → CASE returns NULL → watermark gate stays false.
    clear_watermarks(&remote, &channel_id).await;
    alice.fetch_channel_messages(&channel_id).await;
    assert_eq!(
        envelope_count(&remote, &channel_id).await,
        0,
        "old envelope should be cleaned by the TTL gate even when the watermark gate cannot fire"
    );

    drop(alice);
    drop(bob);
}

// ─── Multi-device helpers ───────────────────────────────────────────────────

impl TestClient {
    /// Fetch a DM channel page through the real `get_dm_messages` command.
    /// Mirrors `fetch_channel_messages` but drives the DM code path, which
    /// polls welcomes and processes commits before decrypting.
    async fn fetch_dm_messages(&self, dm_channel_id: &str) -> Vec<serde_json::Value> {
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
    async fn leave_dm(&self, dm_channel_id: &str) {
        self.invoke_json(
            "leave_dm_channel",
            json!({ "dmChannelId": dm_channel_id, "userId": self.user_id() }),
        )
        .await;
    }

    async fn unblock(&self, blocked_user_id: &str) {
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
async fn enroll_second_device(primary: &TestClient, email: &str) -> TestClient {
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

    // 5. Sanity: remote now lists two devices for this user.
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

/// Multi-device DM: alice and bob each run two enrolled devices. A DM
/// between them must reach all four leaves of the MLS tree, because
/// `dm_channel_member` is keyed per user but the tree expands to every
/// enrolled device of every member during reconcile. A non-member
/// (carol) cannot decrypt any of the four messages.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_multi_device_round_trip() {
    wipe().await;

    // ── Setup: alice_d1 + alice_d2, bob_d1 + bob_d2 ──
    let mut alice_d1 = TestClient::new().await;
    let alice_profile = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // ── Create DM: alice_d1 → bob, bob_d1 accepts ──
    let dm_id = alice_d1
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    // Bob sees the pending request on d1 and accepts.
    assert_eq!(bob_d1.list_dm_requests().await.len(), 1);
    bob_d1.accept_dm_request(&dm_id).await;

    // ── Warm MLS on all four devices ──
    // `get_dm_messages` polls welcomes and processes pending commits, so
    // each call advances that device's local MLS state to the current
    // epoch for this conversation.
    alice_d1.fetch_dm_messages(&dm_id).await;
    alice_d2.fetch_dm_messages(&dm_id).await;
    bob_d1.fetch_dm_messages(&dm_id).await;
    bob_d2.fetch_dm_messages(&dm_id).await;

    // ── Carol: signs up, does NOT join the DM. Baseline for non-member. ──
    let mut carol = TestClient::new().await;
    let _carol_profile = carol.sign_up("carol@test.local").await;

    // ── Helper: assert that exactly the three "other" devices decrypt `content`. ──
    async fn assert_other_three_decrypt(
        sender_label: &str,
        content: &str,
        dm_id: &str,
        others: [(&TestClient, &str); 3],
        carol: &TestClient,
    ) {
        for (client, label) in others.iter() {
            let msgs = client.fetch_dm_messages(dm_id).await;
            let contents: Vec<&str> =
                msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&content),
                "{label} should decrypt '{content}' sent by {sender_label}, got: {contents:?}"
            );
        }
        let carol_msgs = carol.fetch_dm_messages(dm_id).await;
        let carol_contents: Vec<&str> =
            carol_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            !carol_contents.contains(&content),
            "non-member carol must not decrypt '{content}', got: {carol_contents:?}"
        );
    }

    // ── Round trip #1: alice_d1 → the other three ──
    alice_d1
        .send_channel_message(&dm_id, "hi from alice-d1")
        .await;
    assert_other_three_decrypt(
        "alice_d1",
        "hi from alice-d1",
        &dm_id,
        [
            (&alice_d2, "alice_d2"),
            (&bob_d1, "bob_d1"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #2: alice_d2 → the other three ──
    alice_d2
        .send_channel_message(&dm_id, "hi from alice-d2")
        .await;
    assert_other_three_decrypt(
        "alice_d2",
        "hi from alice-d2",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&bob_d1, "bob_d1"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #3: bob_d1 → the other three ──
    bob_d1
        .send_channel_message(&dm_id, "hi from bob-d1")
        .await;
    assert_other_three_decrypt(
        "bob_d1",
        "hi from bob-d1",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&alice_d2, "alice_d2"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #4: bob_d2 → the other three ──
    bob_d2
        .send_channel_message(&dm_id, "hi from bob-d2")
        .await;
    assert_other_three_decrypt(
        "bob_d2",
        "hi from bob-d2",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&alice_d2, "alice_d2"),
            (&bob_d1, "bob_d1"),
        ],
        &carol,
    )
    .await;

    drop(alice_d1);
    drop(alice_d2);
    drop(bob_d1);
    drop(bob_d2);
    drop(carol);
}

