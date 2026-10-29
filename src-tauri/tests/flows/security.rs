//! Sole-writer security acceptance test for #420.
//!
//! The DS is the only writer to the MLS control plane, so it must reject any unsigned or forged write.

use crate::harness::{
    delivery_url, raw_post_status, signed_post_status, wipe, writable_remote, TestClient,
};
use serde_json::json;
use serial_test::serial;

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_unsigned_or_invalid_writes() {
    wipe().await;

    // A known user, so the rejections are about the signature, not the user.
    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let base = delivery_url().await;

    let now = (pollis_delivery::util::now_unix() as i64).to_string();
    let empty_body = serde_json::to_vec(&serde_json::json!({})).expect("serialize body");

    // 1. No auth headers → 401.
    let code = raw_post_status(&base, "/v1/welcomes/purge", &[], &empty_body).await;
    assert_eq!(
        code, 401,
        "purge with NO signature must be rejected — the DS is the gated sole writer"
    );

    // 2. Bogus signature for a real user → 401 (fails verification, never open).
    let code = raw_post_status(
        &base,
        "/v1/welcomes/purge",
        &[
            ("X-Pollis-User", profile.id.as_str()),
            ("X-Pollis-Device", "01JBOGUSDEVICEIDXXXXXXXXXX"),
            ("X-Pollis-Timestamp", now.as_str()),
            ("X-Pollis-Signature", "AAAA"),
        ],
        &empty_body,
    )
    .await;
    assert_eq!(
        code, 401,
        "purge with an INVALID signature must be rejected"
    );

    // 3. An unsigned commit is refused too.
    let commit_body = serde_json::to_vec(&serde_json::json!({
        "conversation_id": "01JCONVERSATIONXXXXXXXXXXX",
        "based_on_epoch": 0,
        "sender_id": profile.id,
        "commit": "AA==",
        "welcomes": [],
    }))
    .expect("serialize commit body");
    let code = raw_post_status(&base, "/v1/commits", &[], &commit_body).await;
    assert_eq!(
        code, 401,
        "an unsigned commit must be rejected — clients cannot write the log around the DS"
    );

    drop(alice);
}

/// #419 domain A: membership gates signed sends and edits (403); authorship is a client check since #607.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_a_writes_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let _alice = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // Fully enrolled, so 403 means "not a member", never 401.
    let _mallory = mallory.sign_up("mallory@test.local").await;

    let group_id = alice.create_group("Private").await;
    let channel_id = alice.general_channel_id(&group_id).await;

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
        .send_channel_message_id(&channel_id, "alice's message")
        .await;

    // (1) Non-member send → 403.
    let send_body = serde_json::to_vec(&json!({
        "id": "01TESTNONMEMBERSENDXXXXXXX",
        "conversation_id": channel_id,
        "sender_id": mallory.user_id(),
        "ciphertext": "mls:00",
        "reply_to_id": null,
        "sent_at": "2026-01-01T00:00:00+00:00",
    }))
    .expect("serialize send body");
    let code = signed_post_status(&mallory, "/v1/messages/send", &send_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed send from a non-member must be FORBIDDEN, got {code}"
    );

    // (2) Non-member edit → 403.
    let nonmember_edit_body = serde_json::to_vec(&json!({
        "envelope_id": "01TESTNONMEMBEREDITXXXXXXX",
        "conversation_id": channel_id,
        "target_message_id": msg_id,
        "sender_id": mallory.user_id(),
        "ciphertext": "mls:00",
        "sent_at": "2026-01-01T00:00:00+00:00",
    }))
    .expect("serialize non-member edit body");
    let code = signed_post_status(&mallory, "/v1/messages/edit", &nonmember_edit_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed edit from a NON-member must be FORBIDDEN, got {code}"
    );

    // (3) Member-but-non-author edit → 403 since #1086: accepting it would clobber the author's pending edit.
    //
    //     The DS checks a per-envelope HMAC capability, not an identity, so nothing is de-anonymized.
    let edit_body = serde_json::to_vec(&json!({
        "envelope_id": "01TESTBOGUSEDITENVXXXXXXXX",
        "conversation_id": channel_id,
        "target_message_id": msg_id,
        "sender_id": bob.user_id(),
        "ciphertext": "mls:00",
        "sent_at": "2026-01-01T00:00:00+00:00",
    }))
    .expect("serialize edit body");
    let code = signed_post_status(&bob, "/v1/messages/edit", &edit_body).await;
    assert_eq!(
        code, 403,
        "a member who cannot open the target's deletion capability must not be able \
         to replace its pending edit (#1086), got {code}"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// #419 domain B: the admin role is re-derived server-side from `group_member`.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_b_admin_ops_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Private").await;
    let channel_id = alice.general_channel_id(&group_id).await;

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

    let remove_body = serde_json::to_vec(&json!({
        "group_id": group_id,
        "user_id": alice_profile.id,
        "requester_id": bob.user_id(),
    }))
    .expect("serialize remove body");
    let code = signed_post_status(&bob, "/v1/members/remove", &remove_body).await;
    assert_eq!(
        code, 403,
        "a non-admin removing another member must be FORBIDDEN, got {code}"
    );

    let delete_body = serde_json::to_vec(&json!({
        "channel_id": channel_id,
        "requester_id": bob.user_id(),
    }))
    .expect("serialize delete body");
    let code = signed_post_status(&bob, "/v1/channels/delete", &delete_body).await;
    assert_eq!(
        code, 403,
        "a non-admin deleting a channel must be FORBIDDEN, got {code}"
    );

    drop(alice);
    drop(bob);
}

/// #419 domain C: the DS binds the body's actor to the signer.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_c_writes_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let profile_body = serde_json::to_vec(&json!({
        "user_id": bob_profile.id,
        "username": "hacked-by-alice",
    }))
    .expect("serialize profile body");
    let code = signed_post_status(&alice, "/v1/profile/update", &profile_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed profile edit of someone else's row must be FORBIDDEN, got {code}"
    );

    let block_body = serde_json::to_vec(&json!({
        "blocker_id": bob_profile.id,
        "blocked_id": alice.user_id(),
    }))
    .expect("serialize block body");
    let code = signed_post_status(&alice, "/v1/blocks/add", &block_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed block on another user's behalf must be FORBIDDEN, got {code}"
    );

    drop(alice);
    drop(bob);
}

/// #419: an outsider can neither add to nor remove from a DM (403).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_dm_churn_by_non_member() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // Fully enrolled, so 403 means "not a member", never 401.
    let _mallory = mallory.sign_up("mallory@test.local").await;

    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    let add_body = serde_json::to_vec(&json!({
        "dm_channel_id": dm_id,
        "user_id": mallory.user_id(),
        "added_by": mallory.user_id(),
        "added_at": "2026-01-01T00:00:00+00:00",
    }))
    .expect("serialize dm/add body");
    let code = signed_post_status(&mallory, "/v1/dm/add", &add_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed dm/add by a non-member must be FORBIDDEN, got {code}"
    );

    // (2) Mallory is neither the removed user nor the creator → 403.
    let remove_body = serde_json::to_vec(&json!({
        "dm_channel_id": dm_id,
        "user_id": bob_profile.id,
        "requester_id": mallory.user_id(),
    }))
    .expect("serialize dm/remove body");
    let code = signed_post_status(&mallory, "/v1/dm/remove", &remove_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed dm/remove by a non-creator/non-self must be FORBIDDEN, got {code}"
    );

    let still_member = bob
        .list_dm_requests()
        .await
        .iter()
        .chain(bob.list_dms().await.iter())
        .any(|dm| dm["id"].as_str() == Some(dm_id.as_str()));
    assert!(
        still_member,
        "bob must remain a member after the forbidden removal"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// #419 domain D: key packages and push tokens are owner-scoped (403 for another user).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_d_writes_for_another_user() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    // Fully enrolled, so 403, never 401.
    let _mallory = mallory.sign_up("mallory@test.local").await;

    let mallory_device = mallory
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("mallory device_id");

    let kp_body = serde_json::to_vec(&json!({
        "device_id": mallory_device,
        "packages": [{ "ref_hash": "deadbeef", "key_package": "AA==" }],
        "user_id": alice_profile.id,
    }))
    .expect("serialize key-package body");
    let code = signed_post_status(&mallory, "/v1/key-packages", &kp_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed key-package publish naming ANOTHER user must be FORBIDDEN, got {code}"
    );

    let push_body = serde_json::to_vec(&json!({
        "token": "ExponentPushToken[mallory]",
        "platform": "ios",
        "updated_at": "2026-01-01T00:00:00+00:00",
        "user_id": alice_profile.id,
    }))
    .expect("serialize push-token body");
    let code = signed_post_status(&mallory, "/v1/push-tokens", &push_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed push-token register naming ANOTHER user must be FORBIDDEN, got {code}"
    );

    drop(alice);
    drop(mallory);
}

/// #419 domains E + G: account-lifecycle ops are self-scoped (403 for another user).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_eg_writes_for_another_user() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    // Fully enrolled, so 403, never 401.
    let _mallory = mallory.sign_up("mallory@test.local").await;

    // (1) Refused before any account_key_log touch.
    let rotate_body = serde_json::to_vec(&json!({
        "based_on_version": 1,
        "account_id_pub": "AA==",
        "salt": "AA==",
        "nonce": "AA==",
        "wrapped_key": "AA==",
        "user_id": alice_profile.id,
    }))
    .expect("serialize rotate body");
    let code = signed_post_status(&mallory, "/v1/account/rotate-identity", &rotate_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed identity rotation of ANOTHER user must be FORBIDDEN, got {code}"
    );

    let delete_body = serde_json::to_vec(&json!({ "user_id": alice_profile.id }))
        .expect("serialize delete body");
    let code = signed_post_status(&mallory, "/v1/account/delete", &delete_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed account deletion of ANOTHER user must be FORBIDDEN, got {code}"
    );

    let event_body = serde_json::to_vec(&json!({
        "kind": "identity_reset",
        "user_id": alice_profile.id,
    }))
    .expect("serialize event body");
    let code = signed_post_status(&mallory, "/v1/security-events", &event_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed security-event under ANOTHER user's name must be FORBIDDEN, got {code}"
    );

    let approve_body = serde_json::to_vec(&json!({
        "request_id": "01TESTENROLLREQXXXXXXXXXXX",
        "wrapped_account_key": "AA==",
        "approved_by_device_id": "01TESTDEVICEXXXXXXXXXXXXXX",
        "user_id": alice_profile.id,
    }))
    .expect("serialize approve body");
    let code = signed_post_status(&mallory, "/v1/enrollment/approve", &approve_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed enrollment approval onto ANOTHER user's account must be FORBIDDEN, got {code}"
    );

    drop(alice);
    drop(mallory);
}

/// #419 domain E: the `account_key_log` CAS lets exactly one rotation from a head win (409, no fork).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn account_key_log_cas_rejects_second_rotation_at_same_version() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let _alice = alice.sign_up("alice@test.local").await;

    // sign_up set version 1, so both rotations claim head 1.
    let rotate = |pubk: &str| {
        serde_json::to_vec(&json!({
            "based_on_version": 1,
            "account_id_pub": pubk,
            "salt": "AQ==",
            "nonce": "Ag==",
            "wrapped_key": "Aw==",
        }))
        .expect("serialize rotate body")
    };

    let first = signed_post_status(&alice, "/v1/account/rotate-identity", &rotate("EAAA")).await;
    assert_eq!(
        first, 200,
        "the first rotation from the current head must be ACCEPTED, got {first}"
    );

    let second = signed_post_status(&alice, "/v1/account/rotate-identity", &rotate("IAAA")).await;
    assert_eq!(
        second, 409,
        "a second rotation from an already-consumed head must CONFLICT (no fork), got {second}"
    );

    let db = crate::harness::writable_remote().await;
    let conn = db.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM account_key_log WHERE user_id = ?1 AND identity_version = 2",
            libsql::params![alice.user_id().to_string()],
        )
        .await
        .expect("account_key_log count query");
    let count: i64 = rows
        .next()
        .await
        .expect("row")
        .expect("some row")
        .get(0)
        .expect("count");
    assert_eq!(
        count, 1,
        "account_key_log must hold exactly one row at version 2 (no fork), got {count}"
    );

    drop(alice);
}

/// C4: device-signed logout is self-scoped.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_logout_only_removes_own_device() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let _mallory = mallory.sign_up("mallory@test.local").await;

    async fn device_id_of(_client: &TestClient, user_id: &str) -> String {
        let db = crate::harness::writable_remote().await;
        let conn = db.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT device_id FROM user_device WHERE user_id = ?1",
                libsql::params![user_id.to_string()],
            )
            .await
            .expect("user_device select");
        rows.next()
            .await
            .expect("rows")
            .expect("device row")
            .get::<String>(0)
            .expect("device_id")
    }
    async fn device_row_exists(_client: &TestClient, device_id: &str) -> bool {
        let db = crate::harness::writable_remote().await;
        let conn = db.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT 1 FROM user_device WHERE device_id = ?1",
                libsql::params![device_id.to_string()],
            )
            .await
            .expect("user_device exists query");
        rows.next().await.expect("rows").is_some()
    }

    let alice_device = device_id_of(&alice, &alice_profile.id).await;
    let mallory_device = device_id_of(&mallory, mallory.user_id()).await;

    let body = serde_json::to_vec(&json!({
        "device_id": alice_device,
        "user_id": alice_profile.id,
    }))
    .expect("serialize logout body");
    let code = signed_post_status(&mallory, "/v1/auth/logout", &body).await;
    assert_eq!(
        code, 403,
        "a validly-signed logout of ANOTHER user's device must be FORBIDDEN, got {code}"
    );
    assert!(
        device_row_exists(&alice, &alice_device).await,
        "alice's device row must survive a forbidden cross-user logout"
    );

    // (2) No body user_id → 200 but a no-op: the DELETE is bound to the signer.
    let body = serde_json::to_vec(&json!({ "device_id": alice_device }))
        .expect("serialize logout body");
    let code = signed_post_status(&mallory, "/v1/auth/logout", &body).await;
    assert_eq!(
        code, 200,
        "an own-account logout request is accepted (no-op when the device isn't the signer's), got {code}"
    );
    assert!(
        device_row_exists(&alice, &alice_device).await,
        "alice's device row must NOT be removable by mallory's self-scoped logout"
    );

    let body = serde_json::to_vec(&json!({ "device_id": mallory_device }))
        .expect("serialize logout body");
    let code = signed_post_status(&mallory, "/v1/auth/logout", &body).await;
    assert_eq!(
        code, 200,
        "a signer logging out their OWN device must succeed, got {code}"
    );
    assert!(
        !device_row_exists(&alice, &mallory_device).await,
        "mallory's own device row must be removed by her logout"
    );

    drop(alice);
    drop(mallory);
}

/// #875: `/v1/messages/delete` with auth OFF must take `actor_id` from the client-built body; #878 scoping still holds.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn message_delete_works_when_ds_auth_is_disabled() {
    use pollis_core::commands::messages::delete_message_body;
        use pollis_delivery::writes::WriteOutcome;

    wipe().await;
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("writable main-db conn");

    // Seeded server-side: a DM mallory is in, and an unrelated conversation.
    conn.execute(
        "INSERT INTO users (id, email, username) \
         VALUES ('mallory', 'mallory-875@test.local', 'mallory-875')",
        (),
    )
    .await
    .expect("seed user");
    // 000017's triggers need the registry claim first (#948).
    conn.execute_batch(
        "INSERT INTO conversation (id, kind) VALUES ('conv-mine', 'dm');
         INSERT INTO dm_channel (id, created_by) VALUES ('conv-mine', 'mallory');",
    )
    .await
    .expect("seed dm_channel");
    conn.execute(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
         VALUES ('conv-mine', 'mallory', 'mallory')",
        (),
    )
    .await
    .expect("seed dm_channel_member");
    for (id, conv) in [("env-mine", "conv-mine"), ("env-theirs", "conv-theirs")] {
        conn.execute(
            "INSERT INTO message_envelope \
                 (id, conversation_id, sender_id, ciphertext, sent_at, type) \
             VALUES (?1, ?2, 'sealed', 'mls:00', '2027-01-15T08:00:00.5+00:00', 'message')",
            libsql::params![id.to_string(), conv.to_string()],
        )
        .await
        .expect("seed envelope");
    }

    async fn envelope_exists(conn: &libsql::Connection, id: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT 1 FROM message_envelope WHERE id = ?1",
                libsql::params![id.to_string()],
            )
            .await
            .expect("envelope lookup");
        rows.next().await.expect("row").is_some()
    }

    // The exact body the shipped client builds.
    let parsed = delete_message_body("env-mine", "conv-mine", "mallory", "mallory", None);

    // Auth OFF: `actor_id` is the only thing naming the actor.
    let outcome = pollis_delivery::messages::apply_delete_message(&conn, None, &parsed)
        .await
        .expect("delete must not error");
    assert!(
        matches!(outcome, WriteOutcome::Ok),
        "the client's delete body must be accepted on a no-auth deployment, got {outcome:?}"
    );
    assert!(
        !envelope_exists(&conn, "env-mine").await,
        "the no-auth delete must actually remove the envelope, not merely return 200"
    );

    // #878: membership of 'conv-mine' cannot reach an envelope in 'conv-theirs'.
    let parsed = delete_message_body("env-theirs", "conv-mine", "mallory", "mallory", None);
    let outcome = pollis_delivery::messages::apply_delete_message(&conn, Some("mallory"), &parsed)
        .await
        .expect("delete must not error");
    assert!(
        envelope_exists(&conn, "env-theirs").await,
        "a member of 'conv-mine' must not be able to delete an envelope in another \
         conversation (#878); outcome was {outcome:?}"
    );

    // `actor_id` is never a grant on the signed path.
    let parsed = delete_message_body("env-theirs", "conv-theirs", "alice", "alice", None);
    let outcome = pollis_delivery::messages::apply_delete_message(&conn, Some("mallory"), &parsed)
        .await
        .expect("delete must not error");
    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "a signed request whose actor_id names a DIFFERENT user must be forbidden, got {outcome:?}"
    );
    assert!(
        envelope_exists(&conn, "env-theirs").await,
        "the impersonating delete must not have removed anything"
    );
}

/// The four path-taking commands refuse paths no human chose, and pass the gate once recorded.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn path_taking_commands_refuse_a_path_the_user_never_chose() {
    wipe().await;

    let client = TestClient::new().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let secret_path = dir.path().join("id_rsa");
    std::fs::write(&secret_path, b"-----BEGIN OPENSSH PRIVATE KEY-----").expect("write");
    let secret = secret_path.to_string_lossy().into_owned();
    let elsewhere = dir.path().join("anywhere.json").to_string_lossy().into_owned();
    let files_dir = dir.path().to_string_lossy().into_owned();

    let refusals = [
        (
            "upload_media",
            json!({
                "path": secret,
                "filename": "id_rsa",
                "contentType": "application/octet-stream"
            }),
        ),
        (
            "upload_group_emoji",
            json!({ "groupId": "g", "shortcode": "sneaky", "path": secret }),
        ),
        (
            "export_archive",
            json!({ "path": elsewhere, "conversationId": null }),
        ),
        (
            "fetch_export_attachments",
            json!({ "filesDir": files_dir, "attachments": [] }),
        ),
    ];

    for (cmd, args) in &refusals {
        let err = client
            .invoke_try(cmd, args.clone())
            .await
            .expect_err("must refuse a path nobody picked");
        assert!(
            err.contains("not chosen in a file dialog"),
            "{cmd} must be refused BY THE PATH SCOPE, not incidentally by something \
             else downstream; got: {err}"
        );
    }

    // Only the pickers and DragDrop can record one, hence the managed state directly.
    let scope = tauri::Manager::state::<pollis_lib::pathscope::PathScope>(&client.webview);
    scope.remember_file(&secret_path);

    let err = client
        .invoke_try(
            "upload_media",
            json!({
                "path": secret,
                "filename": "id_rsa",
                "contentType": "application/octet-stream"
            }),
        )
        .await
        .expect_err("this client is not signed in, so the upload still cannot complete");
    assert!(
        !err.contains("not chosen in a file dialog"),
        "a path the user DID choose must get past the gate; got: {err}"
    );
}
