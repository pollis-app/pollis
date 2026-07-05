//! MULTI-DEVICE ENROLLMENT gate (M4, spec §7 "Additional-device enrollment").
//!
//! Adding a second terminal to an account that already exists on another device,
//! via sibling approval, and proving the new device got a WORKING MLS LEAF — not
//! just an auth session. The canonical `device_enrollment` call order (derived
//! from `src-tauri/tests/flows/harness.rs::enroll_second_device`), driven through
//! `pollis_tui::{auth,enroll}`:
//!
//!   new device:  request_otp -> verify_otp  (enrollment_required=true, mints the
//!                                             enrollment_session, registers device)
//!   new device:  request_enrollment(user_id) -> EnrollmentHandle
//!   old device:  pending_requests(user_id) -> match request_id, confirm code
//!   old device:  approve(request_id, code)
//!   new device:  enrollment_status(request_id) -> Approved   (installs account key)
//!   new device:  set_pin -> finalize(user_id) -> initialize_identity
//!
//! Devices A and B are the SAME user, so each gets its OWN `POLLIS_DATA_DIR`
//! (separate local SQLCipher DB + keystore + accounts index) but shares the
//! world's DS + libsql. Carol is a third user — the independent decryptor that
//! proves B's leaf works.
//!
//! Bounded-history holds (spec): B, a new device, does NOT inherit the message A
//! sent before B enrolled. What MUST work — and what we assert — is that B, once
//! enrolled, sees the conversation and can SEND a message the other members
//! receive and decrypt.

mod common;

use common::{spawn_world, TestClient};

fn contents(page: &pollis_core::commands::messages::MessagePage) -> Vec<String> {
    page.messages
        .iter()
        .filter_map(|m| m.content.clone())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn second_device_enrolls_via_approval_and_gets_a_working_mls_leaf() {
    let world = spawn_world().await;

    // Device A (first device of alice) + Carol (a third user, the decryptor).
    let mut alice_a = TestClient::new_persistent_in(&world, "alice-dev-a");
    let mut carol = TestClient::new_persistent_in(&world, "carol");

    let alice_email = "alice@test.local";
    let alice_profile = alice_a.sign_up(alice_email).await;
    let carol_profile = carol.sign_up("carol@test.local").await;

    // ── A opens a DM to Carol; Carol accepts; A sends a first message ──
    let dm_id = alice_a.create_dm(&carol_profile.id).await;
    let requests = carol.dm_requests().await;
    assert_eq!(requests.len(), 1, "Carol should see A's pending DM request");
    carol.accept_dm(&dm_id).await;
    alice_a.send_text(&dm_id, "hello from device A").await;

    carol.sync_rounds(4).await;
    assert_eq!(
        contents(&carol.read_dm(&dm_id).await),
        vec!["hello from device A"],
        "baseline: Carol receives A's first message",
    );

    // ── Device B: a fresh SECOND device for alice, on its own data dir ──
    let mut alice_b = TestClient::new_persistent_in(&world, "alice-dev-b");
    let b_profile = alice_b.begin_enrollment(alice_email).await;
    assert_eq!(
        b_profile.id, alice_profile.id,
        "the second device must resolve to alice's existing user_id",
    );

    // ── B requests enrollment; A sees it, confirms the code, approves ──
    let handle = alice_b.request_enrollment().await;
    let pending = alice_a.pending_enrollment_requests().await;
    let matching = pending
        .iter()
        .find(|r| r.request_id == handle.request_id)
        .unwrap_or_else(|| {
            panic!(
                "device A did not see B's pending request {}; got {pending:#?}",
                handle.request_id
            )
        });
    assert_eq!(
        matching.verification_code, handle.verification_code,
        "the verification code must match between the two devices",
    );
    alice_a
        .approve_enrollment(&handle.request_id, &handle.verification_code)
        .await;

    // ── B polls to Approved, then finishes (set_pin → finalize → init) ──
    alice_b.await_approval_and_finish(&handle.request_id).await;

    // ── B syncs; it now sees the account's conversation ──
    alice_b.sync_rounds(4).await;
    assert!(
        alice_b.conversation_ids().await.contains(&dm_id),
        "the enrolled device must see alice's existing DM",
    );

    // ── The gate: B SENDS; Carol (a different user) receives + decrypts it ──
    alice_b.send_text(&dm_id, "hello from device B").await;

    carol.sync_rounds(4).await;
    let carol_seen = contents(&carol.read_dm(&dm_id).await);
    assert!(
        carol_seen.contains(&"hello from device B".to_string()),
        "Carol must decrypt the message B sent from its new leaf; got {carol_seen:?}",
    );

    // ── And A (alice's first device) also receives B's message ──
    alice_a.sync_rounds(4).await;
    let a_seen = contents(&alice_a.read_dm(&dm_id).await);
    assert!(
        a_seen.contains(&"hello from device B".to_string()),
        "device A must decrypt the message B sent; got {a_seen:?}",
    );
}
