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

use crate::harness::{delivery_url, raw_post_status, wipe, TestClient};
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
