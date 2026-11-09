//! Biometric unlock on top of the PIN (#1685), through the real command pipeline. The "biometric
//! store" is the test itself holding the device key the app would keep behind Face ID or a
//! fingerprint; everything core decides is exercised as on a phone.

use crate::harness::{wipe, TestClient, TEST_PIN};
use pollis_core::notify::escrow::{test_support, ESCROW_SLOT};
use serde_json::json;
use serial_test::serial;

const SLOTS: [&str; 3] = ["duk_meta", "db_key_duk", "account_id_key_duk"];

/// The escrow is mobile-only; these tests stand in for a phone.
struct MobileBuild;

impl MobileBuild {
    fn on() -> Self {
        test_support::force_escrow_supported_globally(true);
        Self
    }
}

impl Drop for MobileBuild {
    fn drop(&mut self) {
        test_support::force_escrow_supported_globally(false);
    }
}

fn other_pin() -> &'static str {
    if TEST_PIN == "1111" {
        "2222"
    } else {
        "1111"
    }
}

/// A well-formed device key that is not the one core handed out.
fn a_wrong_key() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode([0x5au8; 32])
}

async fn slot(c: &TestClient, name: &str) -> Option<Vec<u8>> {
    c.state.keystore.load_for_user(name, c.user_id()).await.expect("keystore")
}

async fn slots_present(c: &TestClient) -> Vec<&'static str> {
    let mut out = Vec::new();
    for s in SLOTS {
        if slot(c, s).await.is_some() {
            out.push(s);
        }
    }
    out
}

async fn device_unlock_on(c: &TestClient) -> bool {
    let snap = c.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["pin_set"], true, "{snap}");
    snap["device_unlock"].as_bool().expect("device_unlock")
}

async fn try_unlock(c: &TestClient, key: &str) -> Result<serde_json::Value, String> {
    c.invoke_try(
        "unlock_with_device_key",
        json!({ "userId": c.user_id(), "deviceKey": key }),
    )
    .await
}

async fn signed_up(email: &str) -> TestClient {
    let mut c = TestClient::new().await;
    c.sign_up(email).await;
    c
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_biometric_unlock_opens_the_same_data_and_refreshes_the_escrow() {
    wipe().await;
    let _mobile = MobileBuild::on();
    let alice = signed_up("duk-same@test.local").await;
    let group = alice.create_group("Biometrics").await;
    let channel = alice.general_channel_id(&group).await;
    let sent = alice.send_channel_message_id(&channel, "before the lock").await;
    let escrow_before = slot(&alice, ESCROW_SLOT).await.expect("a PIN unlock escrows the key");

    assert!(!device_unlock_on(&alice).await, "off by default");
    let key = alice.enable_biometric_unlock().await;
    assert!(device_unlock_on(&alice).await);

    alice.invoke_json("lock", json!({})).await;
    // Drop the escrow so the unlock has to write it again.
    alice
        .state
        .keystore
        .delete_for_user(ESCROW_SLOT, alice.user_id())
        .await
        .unwrap();
    let outcome = try_unlock(&alice, &key).await.expect("biometric unlock");
    assert_eq!(outcome["user_id"], alice.user_id());
    let snap = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["is_unlocked"], true);

    assert_eq!(
        slot(&alice, ESCROW_SLOT).await,
        Some(escrow_before),
        "the biometric unlock refreshes the escrow with the same key, as the PIN does"
    );
    let page = alice
        .invoke_json("list_messages", json!({ "conversationId": channel }))
        .await;
    assert!(
        page.to_string().contains(&sent),
        "the same encrypted DB opens: {page}"
    );
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_biometric_unlock_never_touches_the_pin_attempt_counter() {
    wipe().await;
    let alice = signed_up("duk-counter@test.local").await;
    let key = alice.enable_biometric_unlock().await;
    alice.invoke_json("lock", json!({})).await;

    for _ in 0..2 {
        alice
            .invoke_try("unlock", json!({ "userId": alice.user_id(), "pin": other_pin() }))
            .await
            .expect_err("wrong PIN");
    }
    let counted = slot(&alice, "pin_meta").await.expect("pin_meta");

    for bad in [a_wrong_key(), "garbage".to_string(), String::new()] {
        try_unlock(&alice, &bad).await.expect_err("a wrong device key never unlocks");
        assert_eq!(
            slot(&alice, "pin_meta").await.as_ref(),
            Some(&counted),
            "a failed biometric unlock must not move the PIN counter"
        );
    }
    try_unlock(&alice, &key).await.expect("the right key unlocks");
    assert_eq!(
        slot(&alice, "pin_meta").await.as_ref(),
        Some(&counted),
        "nor does a successful one forgive the wrong PINs"
    );
    assert_eq!(slots_present(&alice).await, SLOTS.to_vec(), "a failure disables nothing");

    // Eight more wrong PINs make ten: the lockout takes the biometric slots too.
    alice.invoke_json("lock", json!({})).await;
    for _ in 0..8 {
        let _ = alice
            .invoke_try("unlock", json!({ "userId": alice.user_id(), "pin": other_pin() }))
            .await;
    }
    assert!(slots_present(&alice).await.is_empty(), "the 10th wrong PIN removes them");
    assert!(slot(&alice, "pin_meta").await.is_none());
    try_unlock(&alice, &key).await.expect_err("no way around a lockout");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn turning_it_on_needs_the_pin_and_biometrics_never_re_arm_themselves() {
    wipe().await;
    let alice = signed_up("duk-proof@test.local").await;
    let key = alice.enable_biometric_unlock().await;
    alice.lock_then_biometric_unlock(&key).await;

    // Unlocked by biometrics: turning it on again (re-keying) wants the PIN.
    let err = alice
        .invoke_try("device_unlock_begin", json!({ "pin": null }))
        .await
        .expect_err("a biometric unlock is no PIN proof");
    assert!(err.contains("PIN"), "{err}");
    let err = alice
        .invoke_try("device_unlock_begin", json!({ "pin": key }))
        .await
        .expect_err("the device key is not a PIN");
    assert!(err.contains("PIN"), "{err}");
    let before = slot(&alice, "pin_meta").await.expect("pin_meta");
    alice
        .invoke_try("device_unlock_begin", json!({ "pin": other_pin() }))
        .await
        .expect_err("a wrong PIN");
    assert_ne!(
        slot(&alice, "pin_meta").await,
        Some(before),
        "a wrong PIN typed to turn it on counts like any other"
    );
    alice.invoke_json("lock", json!({})).await;

    // Unlocked by the PIN: the offer may turn it on once without asking again.
    alice
        .invoke_try("unlock", json!({ "userId": alice.user_id(), "pin": TEST_PIN }))
        .await
        .expect("PIN unlock");
    let fresh = alice
        .invoke_json("device_unlock_begin", json!({ "pin": null }))
        .await
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        !device_unlock_on(&alice).await,
        "begun but not confirmed is off: the old key already stopped working"
    );
    try_unlock(&alice, &key).await.expect_err("unconfirmed slots never unlock");
    alice
        .invoke_try("device_unlock_begin", json!({ "pin": null }))
        .await
        .expect_err("the proof is single use");

    alice
        .invoke_try("device_unlock_confirm", json!({ "deviceKey": a_wrong_key() }))
        .await
        .expect_err("confirming with a key that is not the stored one");
    assert!(!device_unlock_on(&alice).await);
    alice
        .invoke_json("device_unlock_confirm", json!({ "deviceKey": fresh }))
        .await;
    assert!(device_unlock_on(&alice).await);
    try_unlock(&alice, &key).await.expect_err("the replaced key is dead");
    alice.lock_then_biometric_unlock(&fresh).await;

    // Off needs nothing and leaves nothing.
    alice.invoke_json("disable_device_unlock", json!({})).await;
    assert!(!device_unlock_on(&alice).await);
    assert!(slots_present(&alice).await.is_empty());
    alice.invoke_json("lock", json!({})).await;
    try_unlock(&alice, &fresh).await.expect_err("off is off");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_pin_change_keeps_biometric_unlock_working() {
    wipe().await;
    let alice = signed_up("duk-change@test.local").await;
    let key = alice.enable_biometric_unlock().await;
    alice
        .invoke_json("set_pin", json!({ "oldPin": TEST_PIN, "newPin": other_pin() }))
        .await;
    assert!(device_unlock_on(&alice).await, "a PIN change re-wraps nothing");
    alice.lock_then_biometric_unlock(&key).await;
    // And the new PIN is still the PIN.
    alice.invoke_json("lock", json!({})).await;
    alice
        .invoke_try("unlock", json!({ "userId": alice.user_id(), "pin": other_pin() }))
        .await
        .expect("new PIN");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn one_users_key_never_unlocks_another() {
    wipe().await;
    let alice = signed_up("duk-a@test.local").await;
    let bob = signed_up("duk-b@test.local").await;
    let a_key = alice.enable_biometric_unlock().await;
    let b_key = bob.enable_biometric_unlock().await;
    bob.invoke_json("lock", json!({})).await;
    try_unlock(&bob, &a_key).await.expect_err("Alice's key on Bob's slots");
    try_unlock(&bob, &b_key).await.expect("Bob's own key");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn every_wipe_path_clears_the_slots_and_a_plain_logout_keeps_them() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("duk-wipe@test.local").await;
    let key = alice.enable_biometric_unlock().await;

    // A plain logout keeps them, like the PIN slots.
    alice.invoke_json("logout", json!({ "deleteData": false })).await;
    assert_eq!(slots_present(&alice).await, SLOTS.to_vec());
    assert!(slot(&alice, "pin_meta").await.is_some());

    // logout(delete_data) removes them.
    pollis_lib::accounts::upsert_account(&profile.id, &profile.username, None).unwrap();
    try_unlock(&alice, &key).await.expect("unlock again after a plain logout");
    alice.invoke_json("logout", json!({ "deleteData": true })).await;
    assert!(slots_present(&alice).await.is_empty(), "logout(delete_data)");

    // delete_account removes them.
    let bob = signed_up("duk-wipe-b@test.local").await;
    bob.enable_biometric_unlock().await;
    bob.invoke_json("delete_account", json!({ "userId": bob.user_id() })).await;
    assert!(slots_present(&bob).await.is_empty(), "delete_account");

    // Wiping the local account identity (recovery) removes them.
    let dave = signed_up("duk-wipe-d@test.local").await;
    dave.enable_biometric_unlock().await;
    pollis_core::commands::account_identity::wipe_local_account_identity(&dave.state, dave.user_id())
        .await
        .unwrap();
    assert!(slots_present(&dave).await.is_empty(), "wipe_local_account_identity");

    // A first-time set_pin (e.g. after recovery) may install new keys: stale slots go.
    let erin = signed_up("duk-wipe-e@test.local").await;
    erin.enable_biometric_unlock().await;
    erin.invoke_json("set_pin", json!({ "oldPin": null, "newPin": TEST_PIN }))
        .await;
    assert!(slots_present(&erin).await.is_empty(), "set_pin without the old PIN");
    // `wipe_local_data` deletes the whole data root, which here also holds the harness's own
    // databases, so it is not run in this suite. It deletes exactly `WIPED_USER_SLOTS`, the same
    // list the two logout paths above use, and pollis-core's
    // `device_unlock::tests::every_wipe_path_list_holds_the_slots` holds that list to the slots.
}
