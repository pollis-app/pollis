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
/// who they are; authorization is what they lack. This is the security core of
/// the slice — the membership / sender checks live on the DS, so a client cannot
/// write into a conversation it doesn't belong to, nor edit a message it didn't
/// send, even with a perfectly good device signature.
///
/// Two refusals, both at 403 (not 401 — the requests are correctly signed):
///   1. a NON-member signs a `messages/send` into someone else's channel;
///   2. a member-but-NON-sender signs a `messages/edit` of another user's
///      message.
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

    // (2) Member-but-non-sender edit → 403. Bob is a current member, so he
    //     passes the membership gate, but the target message's sender is alice,
    //     so the sender-only edit check refuses him.
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
        "a validly-signed edit of someone else's message must be FORBIDDEN, got {code}"
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
