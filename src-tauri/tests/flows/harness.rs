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
use pollis_lib::test_harness::{bootstrap_schema, build_client_app, invoke, wipe_remote};
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
    pub(crate) remote: Arc<RemoteDb>,
    pub(crate) config: Config,
}

static WORLD: OnceCell<TestWorld> = OnceCell::const_new();

pub(crate) async fn world() -> &'static TestWorld {
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

            // Stand-in for "remote Turso" — a libsql file in the same temp
            // dir as the per-user SQLCipher DBs. No network round-trip.
            let remote_db_path = path.join("test_turso.db");
            let remote = Arc::new(
                RemoteDb::connect_local(&remote_db_path)
                    .await
                    .expect("connect local libsql"),
            );

            bootstrap_schema(&remote)
                .await
                .expect("bootstrap test turso schema");

            TestWorld { remote, config }
        })
        .await
}

pub(crate) async fn wipe() {
    let w = world().await;
    wipe_remote(&w.remote).await.expect("wipe test turso");
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

    pub(crate) async fn invoke_json(&self, cmd: &str, args: serde_json::Value) -> serde_json::Value {
        invoke(&self.webview, cmd, args)
            .await
            .unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }
}

// ─── Helpers for multi-client scenarios ─────────────────────────────────────

impl TestClient {
    /// Drain any pending MLS welcomes queued for this client on Turso. Real
    /// clients call this on login and when the livekit inbox pings them; the
    /// harness drives it explicitly. Scoped to welcomes only — commits are
    /// per-channel (see [`process_commits_for`]).
    pub(crate) async fn poll(&self) {
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
