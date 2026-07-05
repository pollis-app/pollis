//! SECRET-KEY RECOVERY gate (M4, spec §7 + §11).
//!
//! Recovering an account onto a FRESH terminal with no sibling device online —
//! the Secret Key path. Where the Secret Key comes from (derived from
//! `pollis-core`): first-device signup's `verify_otp` returns
//! `UserProfile.new_secret_key = Some(..)` (the "Emergency Kit" the user saves);
//! at the same time the DS's `establish-identity` stores the server-side
//! `account_recovery` blob (salt/nonce/wrapped_key), wrapped under that Secret
//! Key. `recover_with_secret_key` later fetches that blob and unwraps it.
//!
//! Call order for the recovering device (through `pollis_tui::{auth,enroll}`):
//!
//!   new device:  request_otp -> verify_otp  (enrollment_required=true; registers
//!                                             the device, mints the session)
//!   new device:  recover(user_id, secret_key)   (unwraps account_recovery -> unlock)
//!   new device:  set_pin -> finalize(user_id) -> initialize_identity
//!
//! Device A and the recovering device B are the SAME user → separate
//! `POLLIS_DATA_DIR`s. Carol (a third user) is the independent decryptor proving
//! B's recovered leaf works. Bounded-history: B does not inherit the message A
//! sent before recovery, but once recovered B can send + the members decrypt.

mod common;

use common::{spawn_world, TestClient};

fn contents(page: &pollis_core::commands::messages::MessagePage) -> Vec<String> {
    page.messages
        .iter()
        .filter_map(|m| m.content.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn fresh_device_recovers_with_secret_key_and_gets_a_working_mls_leaf() {
    let world = spawn_world().await;

    let mut alice_a = TestClient::new_persistent_in(&world, "alice-dev-a");
    let mut carol = TestClient::new_persistent_in(&world, "carol");

    let alice_email = "alice@test.local";
    let alice_profile = alice_a.sign_up(alice_email).await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    // The Emergency Kit: first-device signup surfaces the Secret Key exactly once,
    // on the returned profile. This is the ONLY place it exists in the clear.
    let secret_key = alice_profile
        .new_secret_key
        .clone()
        .expect("first-device signup must surface a Secret Key");

    // ── A opens a DM to Carol, who accepts; A sends a first message ──
    let dm_id = alice_a.create_dm(&carol_profile.id).await;
    assert_eq!(carol.dm_requests().await.len(), 1, "Carol sees A's DM request");
    carol.accept_dm(&dm_id).await;
    alice_a.send_text(&dm_id, "hello before recovery").await;

    carol.sync_rounds(4).await;
    assert_eq!(
        contents(&carol.read_dm(&dm_id).await),
        vec!["hello before recovery"],
        "baseline: Carol receives A's first message",
    );

    // ── Device B: a fresh device for alice, recovering via the Secret Key ──
    let mut alice_b = TestClient::new_persistent_in(&world, "alice-dev-b");
    let b_profile = alice_b.begin_enrollment(alice_email).await;
    assert_eq!(
        b_profile.id, alice_profile.id,
        "the recovering device must resolve to alice's existing user_id",
    );

    // No sibling approval — B unwraps the account key from `account_recovery`
    // with the saved Secret Key, then finalizes (cert / KPs / external-join).
    alice_b.recover(&secret_key).await;

    // ── B syncs; it now sees the account's conversation ──
    alice_b.sync_rounds(4).await;
    assert!(
        alice_b.conversation_ids().await.contains(&dm_id),
        "the recovered device must see alice's existing DM",
    );

    // ── The gate: B SENDS; Carol (a different user) receives + decrypts it ──
    alice_b.send_text(&dm_id, "hello after recovery").await;

    carol.sync_rounds(4).await;
    let carol_seen = contents(&carol.read_dm(&dm_id).await);
    assert!(
        carol_seen.contains(&"hello after recovery".to_string()),
        "Carol must decrypt the message B sent from its recovered leaf; got {carol_seen:?}",
    );

    // ── And A (alice's original device) also receives B's message ──
    alice_a.sync_rounds(4).await;
    let a_seen = contents(&alice_a.read_dm(&dm_id).await);
    assert!(
        a_seen.contains(&"hello after recovery".to_string()),
        "device A must decrypt the message B sent; got {a_seen:?}",
    );
}
