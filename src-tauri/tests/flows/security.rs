//! Sole-writer security acceptance test for #420.
//!
//! Goal A makes the Delivery Service the *only* writer to the MLS control-plane
//! tables: clients hold a read-only token on the log DB and can only write via
//! the DS, which gates every write behind a device-signature. The local test DB
//! has no real read-only token to enforce that physically, so this asserts the
//! equivalent SECURITY property the token buys: with auth ON, the DS REJECTS any
//! write that isn't a valid signed request. A client therefore cannot write the
//! commit log around the DS — gaps/forks stay structurally impossible.
//!
//! The whole flows suite is the positive companion: every commit / GroupInfo /
//! Welcome-ack it drives is a *correctly signed* DS write, and the suite only
//! passes because those succeed. This file proves the negative.

use crate::harness::{delivery_url, raw_post_status, signed_post_status, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_unsigned_or_invalid_writes() {
    wipe().await;

    // A fully signed-up client so a real (user_id, device_id, mls_signature_pub)
    // exists in the shared DB — the rejections below are NOT "unknown user", they
    // are "no / bad signature for a known user".
    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let base = delivery_url().await;

    let now = pollis_delivery::auth::now_unix().to_string();
    let empty_body = serde_json::to_vec(&serde_json::json!({})).expect("serialize body");

    // 1. W8 purge with NO auth headers → 401. Without a signature the DS cannot
    //    attribute the write to any device, so it refuses.
    let code = raw_post_status(&base, "/v1/welcomes/purge", &[], &empty_body).await;
    assert_eq!(
        code, 401,
        "purge with NO signature must be rejected — the DS is the gated sole writer"
    );

    // 2. W8 purge with bogus signature headers for a real user → 401. Proves the
    //    gate rejects a forged request, not just one missing headers. ("AAAA" is
    //    valid base64 but doesn't decode to a 64-byte Ed25519 signature, so it
    //    fails verification — the never-fail-open path resolves to 401.)
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

    // 3. The canonical sole-writer path: an UNSIGNED commit must be refused too,
    //    so a client physically cannot append to the commit log without the DS.
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

/// Domain-A (#419) server-side AUTHORIZATION: a *validly signed* message write
/// must still be refused when the signer lacks permission. Authentication proves
/// who they are; authorization is what they lack. The DS's remaining domain-A
/// gate is MEMBERSHIP — a client cannot write into a conversation it doesn't
/// belong to, even with a perfect device signature.
///
/// Authorship is NOT a DS gate anymore (Solution A, #607): under unconditional
/// sealed sender the stored `sender_id` is a blinded sentinel, so the DS cannot
/// prove who authored a message and deliberately does not try. A member-but-
/// non-author edit is therefore ACCEPTED by the DS (200) and rejected instead on
/// the recipient's ingest, where the edit's MLS-authenticated author must equal
/// the target's author (proven by `messages::sealed_non_author_edit_is_ignored`).
///
/// Refusals here (403, not 401 — the requests are correctly signed):
///   1. a NON-member signs a `messages/send` into someone else's channel;
///   2. a NON-member signs a `messages/edit` — the membership gate still applies
///      to edits;
/// plus the post-#607 acceptance:
///   3. a member-but-non-author edit is accepted by the DS (200), authorship
///      being enforced client-side.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_a_writes_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let _alice = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // Mallory is a fully enrolled user (real registered device key) but never
    // joins alice's group — so a 403 below is "not a member", never "unknown
    // device" (which would be 401).
    let _mallory = mallory.sign_up("mallory@test.local").await;

    let group_id = alice.create_group("Private").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // Bob joins so he is a current member (but not the sender of alice's msg).
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

    // Alice sends a real message; its envelope (sender_id = alice) now exists
    // remotely for the edit-authz path to resolve against.
    let msg_id = alice
        .send_channel_message_id(&channel_id, "alice's message")
        .await;

    // (1) Non-member send → 403. Mallory signs the exact body the client seam
    //     would, but she is not a member of `channel_id`.
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

    // (2) NON-member edit → 403. Mallory is not a member of `channel_id`, so the
    //     membership gate refuses her edit — the DS still gates edits on
    //     membership (Solution A only dropped the AUTHOR check, #607).
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

    // (3) Member-but-non-author edit → 200 (Solution A, #607). Bob is a member,
    //     so he passes the membership gate; the DS no longer checks authorship
    //     (the sealed sender_id can't prove it), so it ACCEPTS the envelope. The
    //     forged edit is rejected instead on ingest by every recipient — see
    //     `messages::sealed_non_author_edit_is_ignored`.
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
        code, 200,
        "post-#607 the DS accepts a member's edit regardless of authorship \
         (enforced client-side on ingest), got {code}"
    );

    drop(alice);
    drop(bob);
    drop(mallory);
}

/// Domain-B (#419) server-side AUTHORIZATION: a *validly signed* group/channel
/// admin op must still be refused when the signer is only a plain member. The
/// admin role is re-derived server-side from `group_member`, so a non-admin
/// cannot remove another member or delete a channel even with a perfect device
/// signature.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_b_admin_ops_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Alice owns the group (admin). Bob joins as a plain member.
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

    // (1) Non-admin tries to remove the admin -> 403 (role re-derived as member).
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

    // (2) Non-admin tries to delete a channel -> 403 (destructive ops are admin-only).
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

/// Domain-C (#419) server-side AUTHORIZATION: a *validly signed* profile / block
/// write must still be refused when the signer targets ANOTHER user's row. You
/// may only edit your OWN profile and manage your OWN block list — the DS binds
/// the actor in the body to the authenticated signer.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_c_writes_lacking_authorization() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // (1) alice signs a profile edit for BOB's row -> 403 (actor binding refuses).
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

    // (2) alice signs a block AS BOB -> 403 (you manage only your own block list).
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

/// Goal-B STRAGGLERS (#419) server-side AUTHORIZATION: DM membership churn. A
/// *validly signed* `dm/add` or `dm/remove` from a user who is NOT in the DM must
/// be refused at 403 (not 401 — the signature is real). DM membership is
/// re-derived server-side, so an outsider can neither pull a third party into a
/// conversation it isn't part of, nor evict an existing member.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_dm_churn_by_non_member() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    // Mallory is fully enrolled (real device key) so the rejections below are
    // "not a member" (403), never "unknown device" (401).
    let _mallory = mallory.sign_up("mallory@test.local").await;

    // Alice and Bob share a DM; Mallory is an outsider.
    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    // (1) Mallory signs an add into alice+bob's DM → 403 (she isn't a member, so
    //     she cannot add anyone — the actor membership check refuses it).
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

    // (2) Mallory signs a removal of Bob from alice+bob's DM → 403 (she is neither
    //     the removed user nor the channel creator).
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

    // Bob is still a member — the rejected removal changed nothing.
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

/// Domain-D (#419) server-side AUTHORIZATION: every domain-D write is
/// owner-scoped, so a *validly signed* request that names another user as the
/// owner must be refused at 403 (not 401 — the signature is real). This proves a
/// device cannot publish key packages or register a push token *under another
/// user's identity*, which would let it impersonate that user in MLS adds or
/// hijack their push delivery.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_d_writes_for_another_user() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    // Mallory is fully enrolled (real device key) so the rejections below are
    // "not your account" (403), never "unknown device" (401).
    let _mallory = mallory.sign_up("mallory@test.local").await;

    let mallory_device = mallory
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("mallory device_id");

    // (1) Mallory signs a key-package publish that attributes the rows to ALICE.
    //     resolve_actor refuses the body's user_id ≠ signer → 403.
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

    // (2) Mallory signs a push-token registration under ALICE's user_id → 403,
    //     so she can't redirect alice's background notifications to her device.
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

/// Domains E + G (#419) server-side AUTHORIZATION: every account-lifecycle op is
/// SELF-scoped — a user rotates / deletes / recovers / audits / approves only
/// THEIR OWN account. A *validly signed* request that names ANOTHER user as the
/// target must be refused at 403 (not 401 — the signature is real). This proves a
/// device cannot rotate another user's identity key (which would let it take over
/// the account-key transparency log), delete their account, forge an audit event
/// under their name, or approve a device onto their account.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_rejects_domain_eg_writes_for_another_user() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    // Mallory is fully enrolled (real device key) so the rejections below are
    // "not your account" (403), never "unknown device" (401).
    let _mallory = mallory.sign_up("mallory@test.local").await;

    // (1) Mallory signs an identity rotation that names ALICE → 403. resolve_actor
    //     refuses the body's user_id ≠ signer BEFORE any account_key_log touch, so
    //     the transparency log is never even reached.
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

    // (2) Mallory signs an account deletion naming ALICE → 403.
    let delete_body = serde_json::to_vec(&json!({ "user_id": alice_profile.id }))
        .expect("serialize delete body");
    let code = signed_post_status(&mallory, "/v1/account/delete", &delete_body).await;
    assert_eq!(
        code, 403,
        "a validly-signed account deletion of ANOTHER user must be FORBIDDEN, got {code}"
    );

    // (3) Mallory signs a security-event forging ALICE's audit log → 403.
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

    // (4) Mallory signs an enrollment approval naming ALICE → 403 (she may only
    //     approve enrollments onto her OWN account).
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

/// Domain E (#419) `account_key_log` CAS — the transparency-log analogue of the
/// commit-log fork test (`concurrent_commits_at_same_epoch_must_not_fork_a_member`).
///
/// `account_key_log` is an append-only, transparency-backed log keyed
/// `UNIQUE (user_id, identity_version)`. If two rotations from the same head both
/// landed, the published account-key tree would FORK. The CAS in
/// `apply_rotate_identity` (`INSERT … WHERE based_on == current head … ON CONFLICT
/// DO NOTHING`) must let exactly one win: the second rotation that names the SAME
/// `based_on_version` after the first has advanced the head is rejected with 409,
/// and the log keeps exactly ONE row at the new version.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn account_key_log_cas_rejects_second_rotation_at_same_version() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let _alice = alice.sign_up("alice@test.local").await;

    // sign_up established alice's account identity at version 1 (the direct
    // bootstrap write), so both rotations below claim the same head: version 1.
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

    // First rotation from head=1 wins → 200, advancing the head to version 2.
    let first = signed_post_status(&alice, "/v1/account/rotate-identity", &rotate("EAAA")).await;
    assert_eq!(
        first, 200,
        "the first rotation from the current head must be ACCEPTED, got {first}"
    );

    // Second rotation ALSO claims head=1 (a stale/duplicate append, exactly the
    // concurrent-fork shape). The CAS sees the head is now 2 ≠ 1 → 409. No fork.
    let second = signed_post_status(&alice, "/v1/account/rotate-identity", &rotate("IAAA")).await;
    assert_eq!(
        second, 409,
        "a second rotation from an already-consumed head must CONFLICT (no fork), got {second}"
    );

    // The append-only log holds exactly ONE row at version 2 — the winner's. A
    // fork would show two distinct rows at the same (user_id, identity_version).
    let conn = alice
        .state
        .remote_db
        .conn()
        .await
        .expect("remote conn");
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

/// Bucket-C C4 — the DEVICE-SIGNED logout endpoint (`POST /v1/auth/logout`) is
/// self-scoped: a signer may only remove a device on THEIR OWN account, never
/// another user's. This is the negative companion to the logout flow (which the
/// `logout(delete_data=true)` command drives through this endpoint when a DS is
/// configured).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ds_logout_only_removes_own_device() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut mallory = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let _mallory = mallory.sign_up("mallory@test.local").await;

    // Resolve each client's stable device_id from the shared remote.
    async fn device_id_of(client: &TestClient, user_id: &str) -> String {
        let conn = client.state.remote_db.conn().await.expect("remote conn");
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
    async fn device_row_exists(client: &TestClient, device_id: &str) -> bool {
        let conn = client.state.remote_db.conn().await.expect("remote conn");
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

    // (1) Mallory signs a logout NAMING alice's account → 403. resolve_actor
    //     refuses the body's user_id ≠ signer before any DELETE runs.
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

    // (2) Mallory signs a logout of alice's device WITHOUT a body user_id → 200,
    //     but it is a no-op: the DELETE is bound `WHERE user_id = <signer>`, so it
    //     can never match alice's device. Self-scope holds even with no body id.
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

    // (3) Mallory signs a logout of her OWN device → 200, and her row is GONE.
    //     The happy path: you can remove your own device.
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
