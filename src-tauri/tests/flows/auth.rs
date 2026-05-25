use crate::harness::{wipe, world, TestClient, TEST_PIN};
use pollis_lib::test_harness::invoke;
use serde_json::json;
use serial_test::serial;

// ─── PIN lifecycle ──────────────────────────────────────────────────────────

/// set_pin → lock → unlock roundtrip against the real command pipeline.
///
/// `TestClient::sign_up` already calls `set_pin(TEST_PIN)` so the local
/// DB opens. This test focuses on the lock/unlock half of the cycle:
/// asserts the wrapped blobs and pin_meta are present, the legacy
/// session blob is gone, lock drops unlock state, wrong PIN fails,
/// correct PIN restores access.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn pin_set_lock_unlock_roundtrip() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    // sign_up's set_pin populated AppState.unlock and persisted the
    // wrapped blobs. Legacy session blob never written; legacy
    // unwrapped slots deleted (load-bearing post-#194).
    let snap: serde_json::Value = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["is_unlocked"], true);
    assert_eq!(snap["pin_set"], true);
    assert_eq!(snap["last_active_user"], uid);

    let ks = alice.state.keystore.as_ref();
    assert!(ks.load_for_user("session", &uid).await.unwrap().is_none());
    assert!(ks.load_for_user("db_key", &uid).await.unwrap().is_none(),
        "legacy db_key slot must be deleted post-set_pin");
    assert!(ks.load_for_user("account_id_key", &uid).await.unwrap().is_none(),
        "legacy account_id_key slot must be deleted post-set_pin");
    assert!(ks.load_for_user("pin_meta", &uid).await.unwrap().is_some());
    assert!(ks.load_for_user("db_key_wrapped", &uid).await.unwrap().is_some());
    assert!(ks.load_for_user("account_id_key_wrapped", &uid).await.unwrap().is_some());

    // Lock → not unlocked, pin_set still true.
    invoke::<()>(&alice.webview, "lock", json!({})).await.expect("lock");
    let snap: serde_json::Value = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["is_unlocked"], false);
    assert_eq!(snap["pin_set"], true);

    // Wrong PIN is rejected.
    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": "9999" }),
    )
    .await
    .expect_err("wrong PIN must fail");
    assert!(
        err.to_lowercase().contains("pin"),
        "expected pin-related error, got: {err}"
    );

    // Correct PIN re-unlocks.
    let outcome: serde_json::Value = invoke(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect("unlock with correct PIN");
    assert_eq!(outcome["user_id"], uid);

    let snap: serde_json::Value = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["is_unlocked"], true);

    drop(alice);
}

/// Changing the PIN: old fails after change, new succeeds.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn pin_change_roundtrip() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    // sign_up set TEST_PIN. Change to 2222.
    invoke::<()>(
        &alice.webview,
        "set_pin",
        json!({ "oldPin": TEST_PIN, "newPin": "2222" }),
    )
    .await
    .expect("change set_pin");

    invoke::<()>(&alice.webview, "lock", json!({})).await.unwrap();

    // Old PIN must now fail.
    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect_err("old PIN must fail after change");
    assert!(err.to_lowercase().contains("pin"), "unexpected error: {err}");

    // New PIN succeeds.
    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": "2222" }),
    )
    .await
    .expect("new PIN must unlock");

    drop(alice);
}

/// Lock closes the local DB. With the DB closed, DB-touching commands
/// fail until unlock re-opens it. This is the load-bearing property:
/// the PIN isn't merely a UI gate, it gates SQLCipher decryption.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn pin_locks_db_access() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    // Sanity: signed in, DB open, list_messages returns Ok (empty
    // is fine — we just need a command that touches local_db).
    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect("list_messages before lock");

    invoke::<()>(&alice.webview, "lock", json!({})).await.expect("lock");

    // After lock, the DB handle is dropped — list_messages must fail.
    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect_err("list_messages must fail while locked");
    let err_lower = err.to_lowercase();
    assert!(
        err_lower.contains("not signed in")
            || err_lower.contains("locked")
            || err_lower.contains("database"),
        "expected DB-closed error, got: {err}"
    );

    // Correct PIN reopens.
    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect("unlock with correct PIN");

    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect("list_messages after unlock");

    drop(alice);
}

/// Wrong PIN must NOT open the local DB. The wrapped blobs stay
/// untouched, AppState.unlock stays empty, and DB-touching commands
/// continue to fail.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn wrong_pin_keeps_db_locked() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    invoke::<()>(&alice.webview, "lock", json!({})).await.expect("lock");

    // Capture the wrapped blobs before any failed unlock attempts.
    let ks = alice.state.keystore.clone();
    let wrapped_db_before = ks
        .load_for_user("db_key_wrapped", &uid)
        .await
        .unwrap()
        .expect("wrapped db_key present after sign_up");
    let wrapped_aik_before = ks
        .load_for_user("account_id_key_wrapped", &uid)
        .await
        .unwrap()
        .expect("wrapped account_id_key present after sign_up");

    // Wrong PIN.
    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": "9999" }),
    )
    .await
    .expect_err("wrong PIN must fail");

    // DB still closed — list_messages must still fail.
    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect_err("list_messages must fail after wrong PIN");

    // Wrapped blobs untouched (only the failed_attempts counter inside
    // pin_meta should have changed; the key blobs themselves are
    // immutable until set_pin or lockout-nuke).
    let wrapped_db_after = ks
        .load_for_user("db_key_wrapped", &uid)
        .await
        .unwrap()
        .expect("wrapped db_key still present");
    let wrapped_aik_after = ks
        .load_for_user("account_id_key_wrapped", &uid)
        .await
        .unwrap()
        .expect("wrapped account_id_key still present");
    assert_eq!(wrapped_db_before, wrapped_db_after);
    assert_eq!(wrapped_aik_before, wrapped_aik_after);

    // Correct PIN still opens it.
    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect("correct PIN must succeed after a wrong attempt");

    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect("list_messages after unlock");

    drop(alice);
}

/// Reset the account identity and confirm every existing `user_device`
/// row's cross-signing cert is re-signed against the new
/// `account_id_pub` in lock-step with the rotation. Without the
/// re-sign, the old cert lingers and fails verification on every other
/// client (advisory `process_pending_commits` then logs a warning per
/// commit forever, and the cross-signing defense is effectively off).
///
/// Drives the pure rotation path (`account_identity::reset_identity`)
/// rather than the full `reset_identity_and_recover` Tauri command so
/// the assertion is focused on the cert-resign behavior and not on the
/// downstream group / DM / device-row cleanup.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn reset_identity_resigns_device_cert() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");

    let (old_pub, old_version): (Vec<u8>, i64) = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub, identity_version FROM users WHERE id = ?1",
                libsql::params![user_id.clone()],
            )
            .await
            .expect("users select");
        let row = rows.next().await.expect("rows").expect("user row");
        (
            row.get::<Option<Vec<u8>>>(0).unwrap().expect("account_id_pub"),
            row.get(1).expect("identity_version"),
        )
    };

    let (device_id, old_cert, old_cert_version, old_issued_at_str, mls_sig_pub): (
        String,
        Vec<u8>,
        i64,
        String,
        Vec<u8>,
    ) = {
        let mut rows = conn
            .query(
                "SELECT device_id, device_cert, cert_identity_version, cert_issued_at, mls_signature_pub \
                 FROM user_device WHERE user_id = ?1 AND device_cert IS NOT NULL",
                libsql::params![user_id.clone()],
            )
            .await
            .expect("user_device select");
        let row = rows.next().await.expect("rows").expect("device with cert");
        (
            row.get::<String>(0).expect("device_id"),
            row.get::<Option<Vec<u8>>>(1).unwrap().expect("device_cert"),
            row.get::<i64>(2).expect("cert_identity_version"),
            row.get::<Option<String>>(3).unwrap().expect("cert_issued_at"),
            row.get::<Option<Vec<u8>>>(4).unwrap().expect("mls_signature_pub"),
        )
    };
    let old_issued_at: u64 = old_issued_at_str.parse().expect("parse issued_at");

    pollis_lib::commands::account_identity::verify_device_cert(
        &old_pub,
        &device_id,
        &mls_sig_pub,
        old_cert_version as u32,
        old_issued_at,
        &old_cert,
    )
    .expect("pre-rotation cert must verify against pre-rotation account_id_pub");

    let _new_secret_key = pollis_lib::commands::account_identity::reset_identity(
        &alice.state,
        &user_id,
    )
    .await
    .expect("reset_identity");

    let (new_pub, new_version): (Vec<u8>, i64) = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub, identity_version FROM users WHERE id = ?1",
                libsql::params![user_id.clone()],
            )
            .await
            .expect("users re-select");
        let row = rows.next().await.expect("rows").expect("user row");
        (
            row.get::<Option<Vec<u8>>>(0).unwrap().expect("new account_id_pub"),
            row.get(1).expect("new identity_version"),
        )
    };

    assert_ne!(old_pub, new_pub, "account_id_pub must change");
    assert_eq!(
        new_version,
        old_version + 1,
        "identity_version must bump by exactly 1"
    );

    let (new_cert, new_cert_version, new_issued_at_str): (Vec<u8>, i64, String) = {
        let mut rows = conn
            .query(
                "SELECT device_cert, cert_identity_version, cert_issued_at \
                 FROM user_device WHERE user_id = ?1 AND device_id = ?2",
                libsql::params![user_id.clone(), device_id.clone()],
            )
            .await
            .expect("user_device re-select");
        let row = rows
            .next()
            .await
            .expect("rows")
            .expect("device row still present after reset_identity");
        (
            row.get::<Option<Vec<u8>>>(0)
                .unwrap()
                .expect("device_cert re-signed"),
            row.get::<i64>(1).expect("cert_identity_version"),
            row.get::<Option<String>>(2)
                .unwrap()
                .expect("cert_issued_at"),
        )
    };
    let new_issued_at: u64 = new_issued_at_str.parse().expect("parse new issued_at");

    assert_eq!(
        new_cert_version, new_version,
        "cert_identity_version must match users.identity_version after rotation"
    );
    assert_ne!(
        new_cert, old_cert,
        "device_cert bytes must change after re-sign"
    );

    pollis_lib::commands::account_identity::verify_device_cert(
        &new_pub,
        &device_id,
        &mls_sig_pub,
        new_cert_version as u32,
        new_issued_at,
        &new_cert,
    )
    .expect("post-rotation cert must verify against new account_id_pub");

    let stale = pollis_lib::commands::account_identity::verify_device_cert(
        &new_pub,
        &device_id,
        &mls_sig_pub,
        old_cert_version as u32,
        old_issued_at,
        &old_cert,
    );
    assert!(
        stale.is_err(),
        "old cert must NOT verify against new account_id_pub"
    );

    drop(alice);
}

/// Boot-time self-heal: a sibling device's `user_device` row whose
/// `cert_identity_version` is behind `users.identity_version` (e.g.
/// because another device rotated the account identity while this one
/// was offline) gets re-signed during `unlock`, without that sibling
/// device having to come online itself.
///
/// Drives the path by inserting a synthetic stale row that does NOT
/// belong to the test client's current device (so `ensure_device_cert`
/// — which only touches the calling device — leaves it alone) and
/// asserts `unlock` re-signs it via `resign_stale_device_certs`.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn unlock_resigns_stale_sibling_device_cert() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");

    // Synthetic sibling device row at cert_identity_version = 0 (stale
    // relative to the just-signed-up user's identity_version = 1). The
    // mls_signature_pub bytes are arbitrary — the cross-signing cert
    // attests to whatever bytes are stored there, so verification
    // works on whatever we wrote.
    let phantom_device_id = "phantom-sibling-device";
    let phantom_sig_pub = vec![0xABu8; 32];
    let phantom_cert_placeholder = vec![0u8; 64];
    conn.execute(
        "INSERT INTO user_device \
           (device_id, user_id, device_cert, cert_issued_at, cert_identity_version, mls_signature_pub) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![
            phantom_device_id,
            user_id.clone(),
            phantom_cert_placeholder.clone(),
            "0".to_string(),
            0i64,
            phantom_sig_pub.clone(),
        ],
    )
    .await
    .expect("insert phantom user_device row");

    // Sanity: the phantom row's placeholder cert is bogus and would
    // never verify against the user's real account_id_pub.
    let (account_id_pub, identity_version): (Vec<u8>, i64) = {
        let mut rows = conn
            .query(
                "SELECT account_id_pub, identity_version FROM users WHERE id = ?1",
                libsql::params![user_id.clone()],
            )
            .await
            .expect("users select");
        let row = rows.next().await.expect("rows").expect("user row");
        (
            row.get::<Option<Vec<u8>>>(0).unwrap().expect("account_id_pub"),
            row.get(1).expect("identity_version"),
        )
    };
    assert!(
        pollis_lib::commands::account_identity::verify_device_cert(
            &account_id_pub,
            phantom_device_id,
            &phantom_sig_pub,
            0,
            0,
            &phantom_cert_placeholder,
        )
        .is_err(),
        "placeholder cert must not verify (sanity check)"
    );

    // Lock + unlock to trigger the boot-time sweep.
    invoke::<()>(&alice.webview, "lock", json!({}))
        .await
        .expect("lock");
    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": user_id, "pin": TEST_PIN }),
    )
    .await
    .expect("unlock");

    // The phantom row should now be re-signed at the current
    // identity_version with bytes that verify against
    // account_id_pub.
    let (new_cert, new_cert_version, new_issued_at_str): (Vec<u8>, i64, String) = {
        let mut rows = conn
            .query(
                "SELECT device_cert, cert_identity_version, cert_issued_at \
                 FROM user_device WHERE device_id = ?1 AND user_id = ?2",
                libsql::params![phantom_device_id, user_id.clone()],
            )
            .await
            .expect("phantom re-select");
        let row = rows.next().await.expect("rows").expect("phantom row");
        (
            row.get::<Option<Vec<u8>>>(0).unwrap().expect("device_cert"),
            row.get::<i64>(1).expect("cert_identity_version"),
            row.get::<Option<String>>(2)
                .unwrap()
                .expect("cert_issued_at"),
        )
    };
    let new_issued_at: u64 = new_issued_at_str.parse().expect("parse issued_at");

    assert_eq!(
        new_cert_version, identity_version,
        "phantom cert_identity_version must be bumped to current identity_version"
    );
    assert_ne!(
        new_cert, phantom_cert_placeholder,
        "phantom cert bytes must change (was placeholder, now real signature)"
    );
    pollis_lib::commands::account_identity::verify_device_cert(
        &account_id_pub,
        phantom_device_id,
        &phantom_sig_pub,
        new_cert_version as u32,
        new_issued_at,
        &new_cert,
    )
    .expect("re-signed phantom cert must verify against account_id_pub");

    drop(alice);
}

// ─── Safety numbers / contact verification ───────────────────────────────────

/// Full lifecycle of the Signal-style safety number:
///   1. Creating a DM TOFU-pins the peer → status "unverified", 60 digits.
///   2. Both sides compute the *same* number regardless of who asks.
///   3. `set_contact_verified` flips status → "verified".
///   4. A Turso-side `account_id_pub` swap (the exact attack this defends
///      against) is detected → status "changed".
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn safety_number_lifecycle_and_key_change_detection() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    bob.sign_up("bob@test.local").await;
    let alice_id = alice.user_id().to_string();
    let bob_id = bob.user_id().to_string();

    // Creating the DM runs check_and_pin_account_key for bob on alice's side.
    alice.create_dm(&[&bob_id]).await;

    let a_view = alice
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": alice_id, "peerUserId": bob_id }),
        )
        .await;
    assert_eq!(a_view["status"], "unverified");
    let a_num = a_view["safety_number"].as_str().unwrap().to_string();
    assert_eq!(
        a_num.chars().filter(|c| c.is_ascii_digit()).count(),
        60,
        "safety number must be 60 digits"
    );

    // Order independence: bob asking about alice yields the identical number.
    let b_view = bob
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": bob_id, "peerUserId": alice_id }),
        )
        .await;
    assert_eq!(
        b_view["safety_number"].as_str().unwrap(),
        a_num,
        "both parties must derive the same safety number"
    );

    // Explicit verification.
    alice
        .invoke_json(
            "set_contact_verified",
            json!({ "peerUserId": bob_id, "verified": true }),
        )
        .await;
    let verified = alice
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": alice_id, "peerUserId": bob_id }),
        )
        .await;
    assert_eq!(verified["status"], "verified");

    // Simulate the attack: someone with Turso write access swaps bob's
    // account_id_pub. alice's locally-pinned key no longer matches.
    let conn = world().await.remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE users SET account_id_pub = ?1, identity_version = identity_version + 1 \
         WHERE id = ?2",
        libsql::params![vec![9u8; 32], bob_id.clone()],
    )
    .await
    .expect("swap bob account_id_pub");

    let changed = alice
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": alice_id, "peerUserId": bob_id }),
        )
        .await;
    assert_eq!(
        changed["status"], "changed",
        "a Turso-side key swap must be detected as 'changed'"
    );

    drop(alice);
    drop(bob);
}
