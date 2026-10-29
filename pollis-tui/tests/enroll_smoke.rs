//! MULTI-DEVICE ENROLLMENT gate (M4, spec §7 "Additional-device enrollment").
//!
//! Adding a second terminal to an account that already exists on another device,
//! via sibling approval, proving the new device got a working MLS leaf. Call order:
//!
//!   new device:  request_otp -> verify_otp  (enrollment_required=true, mints the
//!                                             enrollment_session, registers device)
//!   new device:  request_enrollment(user_id) -> EnrollmentHandle
//!   old device:  pending_requests(user_id) -> match request_id, confirm code
//!   old device:  approve(request_id, code)
//!   new device:  enrollment_status(request_id) -> Approved   (installs account key)
//!   new device:  set_pin -> finalize(user_id) -> initialize_identity
//!
//! A and B are the same user on separate data dirs; Carol is the independent decryptor.
//!
//! B does not inherit history, but once enrolled must see the conversation and send.

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

    let mut alice_a = TestClient::new_persistent_in(&world, "alice-dev-a");
    let mut carol = TestClient::new_persistent_in(&world, "carol");

    let alice_email = "alice@test.local";
    let alice_profile = alice_a.sign_up(alice_email).await;
    let carol_profile = carol.sign_up("carol@test.local").await;

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

    let mut alice_b = TestClient::new_persistent_in(&world, "alice-dev-b");
    let b_profile = alice_b.begin_enrollment(alice_email).await;
    assert_eq!(
        b_profile.id, alice_profile.id,
        "the second device must resolve to alice's existing user_id",
    );

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
    // A gets no verification code (#1096); the code has to come off B's screen.
    let _ = matching;

    // A wrong code is refused: the typed code is checked against the one A derives.
    let wrong = wrong_code(&handle.verification_code);
    let refused = alice_a
        .try_approve_enrollment(&handle.request_id, &wrong)
        .await;
    assert!(
        refused.is_err(),
        "approving with a code that is not the derived one must fail, got {refused:?}",
    );

    // The right code, as a human reads it off B's screen.
    alice_a
        .approve_enrollment(&handle.request_id, &handle.verification_code)
        .await;

    alice_b.await_approval_and_finish(&handle.request_id).await;

    alice_b.sync_rounds(4).await;
    assert!(
        alice_b.conversation_ids().await.contains(&dm_id),
        "the enrolled device must see alice's existing DM",
    );

    alice_b.send_text(&dm_id, "hello from device B").await;

    carol.sync_rounds(4).await;
    let carol_seen = contents(&carol.read_dm(&dm_id).await);
    assert!(
        carol_seen.contains(&"hello from device B".to_string()),
        "Carol must decrypt the message B sent from its new leaf; got {carol_seen:?}",
    );

    alice_a.sync_rounds(4).await;
    let a_seen = contents(&alice_a.read_dm(&dm_id).await);
    assert!(
        a_seen.contains(&"hello from device B".to_string()),
        "device A must decrypt the message B sent; got {a_seen:?}",
    );
}

/// alphabet, the shape a mis-read or substituted code takes.
fn wrong_code(code: &str) -> String {
    use pollis_core::commands::device_enrollment::ENROLLMENT_SAS_ALPHABET;
    let mut chars: Vec<char> = code.chars().collect();
    let alphabet: Vec<char> = ENROLLMENT_SAS_ALPHABET.chars().collect();
    let last = chars.len() - 1;
    chars[last] = *alphabet
        .iter()
        .find(|c| **c != chars[last])
        .expect("the alphabet has more than one character");
    chars.into_iter().collect()
}
