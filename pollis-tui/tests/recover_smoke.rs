//! SECRET-KEY RECOVERY gate (M4, spec §7 + §11).
//!
//! Recovering an account onto a FRESH terminal with no sibling device online —
//! the Secret Key path. The key comes from first-device `verify_otp`; the DS stores
//! `account_recovery` wrapped under it.
//!
//! Call order for the recovering device (through `pollis_tui::{auth,enroll}`):
//!
//!   new device:  request_otp -> verify_otp  (enrollment_required=true; registers
//!                                             the device, mints the session)
//!   new device:  recover(user_id, secret_key)   (unwraps account_recovery -> unlock)
//!   new device:  set_pin -> finalize(user_id) -> initialize_identity
//!
//! A and B are the same user on separate data dirs; Carol is the decryptor. B inherits no history.

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

    // The only place the Secret Key exists in the clear.
    let secret_key = alice_profile
        .new_secret_key
        .clone()
        .expect("first-device signup must surface a Secret Key");

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

    let mut alice_b = TestClient::new_persistent_in(&world, "alice-dev-b");
    let b_profile = alice_b.begin_enrollment(alice_email).await;
    assert_eq!(
        b_profile.id, alice_profile.id,
        "the recovering device must resolve to alice's existing user_id",
    );

    // No sibling: B unwraps the account key with the saved Secret Key, then finalizes.
    alice_b.recover(&secret_key).await;

    alice_b.sync_rounds(4).await;
    assert!(
        alice_b.conversation_ids().await.contains(&dm_id),
        "the recovered device must see alice's existing DM",
    );

    alice_b.send_text(&dm_id, "hello after recovery").await;

    carol.sync_rounds(4).await;
    let carol_seen = contents(&carol.read_dm(&dm_id).await);
    assert!(
        carol_seen.contains(&"hello after recovery".to_string()),
        "Carol must decrypt the message B sent from its recovered leaf; got {carol_seen:?}",
    );

    alice_a.sync_rounds(4).await;
    let a_seen = contents(&alice_a.read_dm(&dm_id).await);
    assert!(
        a_seen.contains(&"hello after recovery".to_string()),
        "device A must decrypt the message B sent; got {a_seen:?}",
    );
}
