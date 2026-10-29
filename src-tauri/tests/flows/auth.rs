use crate::harness::{
    expire_ds_device_keys, expire_ds_last_seen_throttle, wipe, world, writable_remote, TestClient,
    TEST_PIN,
};
use pollis_lib::test_harness::invoke;
use serde_json::json;
use serial_test::serial;

/// set_pin → lock → unlock roundtrip, then a PIN change.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn pin_set_lock_unlock_roundtrip() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    // Legacy session blob never written; unwrapped slots deleted (post-#194).
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

    invoke::<()>(&alice.webview, "lock", json!({})).await.expect("lock");
    let snap: serde_json::Value = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["is_unlocked"], false);
    assert_eq!(snap["pin_set"], true);

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

    invoke::<()>(
        &alice.webview,
        "set_pin",
        json!({ "oldPin": TEST_PIN, "newPin": "2222" }),
    )
    .await
    .expect("change set_pin");

    invoke::<()>(&alice.webview, "lock", json!({})).await.unwrap();

    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": TEST_PIN }),
    )
    .await
    .expect_err("old PIN must fail after change");
    assert!(err.to_lowercase().contains("pin"), "unexpected error: {err}");

    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": "2222" }),
    )
    .await
    .expect("new PIN must unlock");

    drop(alice);
}

/// Lock closes the local DB, so the PIN gates SQLCipher decryption; a wrong PIN keeps it closed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn pin_locks_db_access_and_wrong_pin_keeps_it_locked() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect("list_messages before lock");

    invoke::<()>(&alice.webview, "lock", json!({})).await.expect("lock");

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

    invoke::<serde_json::Value>(
        &alice.webview,
        "unlock",
        json!({ "userId": uid, "pin": "9999" }),
    )
    .await
    .expect_err("wrong PIN must fail");

    invoke::<serde_json::Value>(
        &alice.webview,
        "list_messages",
        json!({ "conversationId": "nonexistent" }),
    )
    .await
    .expect_err("list_messages must fail after wrong PIN");

    // Key blobs are immutable until set_pin or a lockout nuke.
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

/// Identity reset re-signs every `user_device` cert against the new `account_id_pub`.
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

    let (device_id, old_cert, old_cert_version, old_issued_at_str, mls_sig_pub, mls_sig_pub_pq): (
        String,
        Vec<u8>,
        i64,
        String,
        Vec<u8>,
        Vec<u8>,
    ) = {
        let mut rows = conn
            .query(
                "SELECT device_id, device_cert, cert_identity_version, cert_issued_at, \
                        mls_signature_pub, mls_signature_pub_pq \
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
            row.get::<Option<Vec<u8>>>(5).unwrap().expect("mls_signature_pub_pq"),
        )
    };
    let old_issued_at: u64 = old_issued_at_str.parse().expect("parse issued_at");

    pollis_lib::commands::account_identity::verify_device_cert(
        &old_pub,
        &device_id,
        &mls_sig_pub,
        &mls_sig_pub_pq,
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
        &mls_sig_pub_pq,
        new_cert_version as u32,
        new_issued_at,
        &new_cert,
    )
    .expect("post-rotation cert must verify against new account_id_pub");

    let stale = pollis_lib::commands::account_identity::verify_device_cert(
        &new_pub,
        &device_id,
        &mls_sig_pub,
        &mls_sig_pub_pq,
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

/// #685 E: `is_current_device_registered` is `true` while live and `false` once tombstoned.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoked_device_reports_itself_unregistered() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");

    let registered = || async {
        invoke::<bool>(
            &alice.webview,
            "is_current_device_registered",
            json!({ "userId": user_id }),
        )
        .await
        .expect("is_current_device_registered must be a reachable command")
    };

    assert!(
        registered().await,
        "control: a live, enrolled device must report itself registered — if this \
         fails the command is missing or broken, and the false case below would \
         pass for the wrong reason (#685)"
    );

    // Tombstone as `POST /v1/devices/revoke` does.
    conn.execute(
        "UPDATE user_device SET revoked_at = datetime('now') WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    )
    .await
    .expect("tombstone the device");

    assert!(
        !registered().await,
        "a REVOKED device must report itself unregistered so the device_revoked \
         nudge signs it out — revocation is a tombstone, not a row delete, so a \
         bare existence check can never fail (#685)"
    );

    drop(alice);
}

/// A revoked device that missed the nudge still signs itself out (#1338).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_revoked_device_that_missed_the_nudge_is_told_and_wiped() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();
    alice.create_group("Before revocation").await;

    let groups = || async {
        invoke::<serde_json::Value>(&alice.webview, "list_user_groups", json!({ "userId": user_id }))
            .await
    };
    groups().await.expect("control: a live device lists its groups");

    // Tombstone and evict the DS key cache, with no nudge.
    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE user_device SET revoked_at = datetime('now') WHERE user_id = ?1",
        libsql::params![user_id.clone()],
    )
    .await
    .expect("tombstone the device");
    expire_ds_device_keys(&user_id);

    let err = groups().await.expect_err("a revoked device's signed call must fail");
    assert!(err.contains("device_revoked"), "the failure must say why: {err}");
    assert!(
        invoke::<bool>(&alice.webview, "is_current_device_revoked", json!({})).await.unwrap(),
        "the client must remember that THIS device was revoked"
    );

    let registered = invoke::<bool>(
        &alice.webview,
        "is_current_device_registered",
        json!({ "userId": user_id }),
    )
    .await
    .expect("the registration check must answer, not error, for a revoked device");
    assert!(!registered, "a revoked device must report itself unregistered");

    let err = groups().await.expect_err("still revoked");
    assert!(err.contains("device_revoked"), "{err}");

    invoke::<()>(&alice.webview, "logout", json!({ "deleteData": false }))
        .await
        .expect("logout");
    let ks = alice.state.keystore.as_ref();
    assert!(
        ks.load_for_user("device_id", &user_id).await.unwrap().is_none(),
        "the revoked device id must be forgotten, or the next sign-in is the same revoked device"
    );
    assert!(
        ks.load_for_user("account_id_key_wrapped", &user_id).await.unwrap().is_none(),
        "a device the account removed must not keep the account's keys"
    );
    assert!(
        !invoke::<bool>(&alice.webview, "is_current_device_revoked", json!({})).await.unwrap(),
        "the next sign-in is a new device and must be allowed to reach the DS"
    );

    drop(alice);
}

/// A late `device_revoked` answer is bound to the device that signed it, never its successor (#1338).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_late_revocation_answer_never_touches_the_next_account() {
    wipe().await;

    let mut client = TestClient::new().await;
    let alice = client.sign_up("alice@test.local").await;
    let alice_device = client.state.device_id.lock().await.clone().expect("alice's device");

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE user_device SET revoked_at = datetime('now') WHERE user_id = ?1",
        libsql::params![alice.id.clone()],
    )
    .await
    .expect("tombstone alice's device");
    expire_ds_device_keys(&alice.id);
    invoke::<serde_json::Value>(&client.webview, "list_user_groups", json!({ "userId": alice.id }))
        .await
        .expect_err("alice's device is revoked");
    invoke::<()>(&client.webview, "logout", json!({ "deleteData": false }))
        .await
        .expect("logout alice");

    let bob = client.sign_up("bob@test.local").await;
    client.create_group("Bob's group").await;

    let recorded = pollis_lib::commands::auth::note_revoked_response(
        &client.state,
        &alice.id,
        &alice_device,
    )
    .await;
    assert!(!recorded, "a refusal for a device that is no longer this one must mark nobody");
    assert!(
        !invoke::<bool>(&client.webview, "is_current_device_revoked", json!({})).await.unwrap(),
        "bob's device is not revoked"
    );
    invoke::<serde_json::Value>(&client.webview, "list_user_groups", json!({ "userId": bob.id }))
        .await
        .expect("bob's signed calls still work");

    invoke::<()>(&client.webview, "logout", json!({ "deleteData": false }))
        .await
        .expect("logout bob");
    let ks = client.state.keystore.as_ref();
    assert!(
        ks.load_for_user("account_id_key_wrapped", &bob.id).await.unwrap().is_some(),
        "a keep-data sign-out of a healthy account must keep its keys"
    );

    drop(client);
}

/// Revoking a device logs a security event via the real signed path (#947).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn revoking_a_device_records_a_security_event() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");

    // The current device cannot revoke itself.
    let sibling = "sibling-device-947";
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3)",
        libsql::params![sibling, user_id.clone(), "Alice's old laptop"],
    )
    .await
    .expect("insert sibling user_device row");

    let revocations = || async {
        let page: serde_json::Value = invoke(
            &alice.webview,
            "list_security_events",
            json!({ "userId": user_id, "limit": 100 }),
        )
        .await
        .expect("list_security_events must be a reachable command");
        page["events"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|e| e["kind"] == "device_revoked")
            .collect::<Vec<_>>()
    };

    assert!(
        revocations().await.is_empty(),
        "control: nothing has been revoked yet, so the log must hold no \
         device_revoked entry — otherwise the assertion below proves nothing"
    );

    invoke::<()>(
        &alice.webview,
        "revoke_device",
        json!({ "userId": user_id, "deviceId": sibling }),
    )
    .await
    .expect("revoke_device");

    let logged = revocations().await;
    assert_eq!(
        logged.len(),
        1,
        "revoking a device must append exactly one device_revoked entry to the \
         account's security log (#947), got {logged:?}"
    );
    assert_eq!(
        logged[0]["device_id"], sibling,
        "the entry must name the device that was revoked — an audit line that \
         cannot say WHICH device does not answer the question it exists for"
    );
    assert_eq!(
        logged[0]["metadata"], "name=Alice's old laptop",
        "the device's name is carried into the event because the `user_device` \
         row it came from is tombstoned by the revocation"
    );
}

/// Revoke returns once tombstoned; the background leaf removal coalesces a second revoke.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_revoked_device_leaves_the_tree_after_revoke_returns() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();
    let laptop = crate::harness::enroll_second_device(&alice, "alice@test.local").await;
    let laptop_id = laptop.state.device_id.lock().await.clone().expect("laptop device id");
    let alice_id = alice.state.device_id.lock().await.clone().expect("alice device id");

    // A leafless sibling, so its revoke lands mid-pass.
    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");
    let stale = "stale-sibling-revoke";
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3)",
        libsql::params![stale, user_id.clone(), "old phone"],
    )
    .await
    .expect("insert stale sibling row");

    let group_id = alice.create_group("RevokeFast").await;
    let in_tree = |tree: &[(String, String)], device: &str| tree.iter().any(|(_, d)| d == device);
    let tree = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
    assert!(
        in_tree(&tree, &laptop_id),
        "precondition: the laptop is a leaf of the group, got {tree:?}"
    );

    for device in [laptop_id.as_str(), stale] {
        invoke::<()>(
            &alice.webview,
            "revoke_device",
            json!({ "userId": user_id, "deviceId": device }),
        )
        .await
        .expect("revoke_device");
    }

    // Bounded wait, with nothing from the test that could do the removal.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let tree = loop {
        let tree = pollis_lib::commands::mls::local_tree_members(&alice.state, &group_id).await;
        if !in_tree(&tree, &laptop_id) {
            break tree;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the revoked laptop must be committed out of the tree after revoke_device \
             returns, with no reconcile from the test; still there after 60 s: {tree:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    assert!(in_tree(&tree, &alice_id), "the revoking device stays in the tree, got {tree:?}");

    drop(laptop);
}

/// #685 C: unlock re-signs a stale LIVE sibling cert but never a REVOKED one (bumping `identity_version` is the only relay-cert kill switch).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn unlock_never_resigns_a_revoked_sibling_device_cert() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");

    let sig_pub = vec![0xABu8; 32];
    let sig_pub_pq = vec![0xCDu8; pollis_lib::commands::account_identity::MLDSA44_PUB_LEN];
    let placeholder = vec![0u8; pollis_lib::commands::account_identity::MLDSA44_SIG_LEN];

    // Identical except for the tombstone.
    for (device_id, revoked) in [("live-sibling", false), ("revoked-sibling", true)] {
        conn.execute(
            "INSERT INTO user_device \
               (device_id, user_id, device_cert, cert_issued_at, cert_identity_version, \
                mls_signature_pub, mls_signature_pub_pq, revoked_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, \
                     CASE WHEN ?8 THEN datetime('now') ELSE NULL END)",
            libsql::params![
                device_id,
                user_id.clone(),
                placeholder.clone(),
                "0".to_string(),
                0i64,
                sig_pub.clone(),
                sig_pub_pq.clone(),
                revoked as i64,
            ],
        )
        .await
        .expect("insert phantom user_device row");
    }

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
            "live-sibling",
            &sig_pub,
            &sig_pub_pq,
            0,
            0,
            &placeholder,
        )
        .is_err(),
        "placeholder cert must not verify (sanity check)"
    );

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

    let cert_state = |device_id: &'static str| {
        let conn = conn.clone();
        let user_id = user_id.clone();
        async move {
            let mut rows = conn
                .query(
                    "SELECT device_cert, cert_identity_version, cert_issued_at \
                     FROM user_device WHERE device_id = ?1 AND user_id = ?2",
                    libsql::params![device_id, user_id],
                )
                .await
                .expect("phantom re-select");
            let row = rows.next().await.expect("rows").expect("phantom row");
            (
                row.get::<Option<Vec<u8>>>(0).unwrap().expect("device_cert"),
                row.get::<i64>(1).expect("cert_identity_version"),
                row.get::<Option<String>>(2).unwrap().expect("cert_issued_at"),
            )
        }
    };

    let (live_cert, live_version, live_issued_at) = cert_state("live-sibling").await;
    assert!(
        live_version > 0 && live_cert != placeholder,
        "control failed: the LIVE stale sibling must be re-signed, otherwise this \
         test proves nothing about the revoked one (#685)"
    );
    assert_eq!(
        live_version, identity_version,
        "the live sibling's cert_identity_version must be bumped to the current identity_version"
    );
    pollis_lib::commands::account_identity::verify_device_cert(
        &account_id_pub,
        "live-sibling",
        &sig_pub,
        &sig_pub_pq,
        live_version as u32,
        live_issued_at.parse().expect("parse issued_at"),
        &live_cert,
    )
    .expect("re-signed live sibling cert must verify against account_id_pub");

    let (revoked_cert, revoked_version, _) = cert_state("revoked-sibling").await;
    assert_eq!(
        revoked_version, 0,
        "a REVOKED device must never be re-signed to the current identity_version — \
         device-cert verification is offline and consults no revocation list, so the \
         version bump is the only thing that invalidates its relay cert (#685)"
    );
    assert_eq!(
        revoked_cert, placeholder,
        "a REVOKED device's cert bytes must be left exactly as they were (#685)"
    );

    drop(alice);
}

/// Device-signed email change; the OTP only proves the new mailbox, bound to the requester.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn email_change_through_ds_happy_and_security() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let alice_id = alice_profile.id.clone();
    let bob_id = bob_profile.id.clone();

    let new_email = "alice-new@test.local";

    alice
        .invoke_json(
            "request_email_change_otp",
            json!({ "userId": alice_id, "newEmail": new_email }),
        )
        .await;

    // 0. A wrong current-address code is refused (#1161), without spending the new-address attempt.
    let err = alice
        .invoke_try(
            "verify_email_change",
            json!({ "userId": alice_id, "newEmail": new_email, "code": crate::harness::DEV_OTP, "currentCode": "999999" }),
        )
        .await
        .expect_err("a change that cannot re-prove the current address must be refused");
    assert!(
        err.to_lowercase().contains("current email address"),
        "expected a current-address error, got: {err}"
    );

    let err = alice
        .invoke_try(
            "verify_email_change",
            json!({ "userId": alice_id, "newEmail": new_email, "code": "999999", "currentCode": crate::harness::DEV_OTP }),
        )
        .await
        .expect_err("a wrong code must be refused");
    assert!(
        err.to_lowercase().contains("invalid"),
        "expected an invalid-code error, got: {err}"
    );

    // 2. Bob can't consume alice's change; refused before the OTP check, so her code stays valid.
    let cross = bob
        .invoke_try(
            "verify_email_change",
            json!({ "userId": bob_id, "newEmail": new_email, "code": crate::harness::DEV_OTP, "currentCode": crate::harness::DEV_OTP }),
        )
        .await
        .expect_err("a different signed user must not consume alice's change");
    assert!(
        cross.to_lowercase().contains("account") || cross.to_lowercase().contains("request"),
        "expected a binding/forbidden error, got: {cross}"
    );

    alice
        .invoke_json(
            "verify_email_change",
            json!({ "userId": alice_id, "newEmail": new_email, "code": crate::harness::DEV_OTP, "currentCode": crate::harness::DEV_OTP }),
        )
        .await;

    let conn = world().await.remote.conn().await.expect("remote conn");
    let read_email = |id: String| {
        let conn = &conn;
        async move {
            let mut rows = conn
                .query("SELECT email FROM users WHERE id = ?1", libsql::params![id])
                .await
                .expect("users select");
            let row = rows.next().await.expect("rows").expect("user row");
            row.get::<String>(0).expect("email")
        }
    };

    assert_eq!(
        read_email(alice_id.clone()).await,
        new_email,
        "alice's email must be swapped to the new address"
    );
    assert_eq!(
        read_email(bob_id.clone()).await,
        "bob@test.local",
        "bob's email must be untouched by alice's change"
    );

    drop(alice);
    drop(bob);
}

/// Safety numbers: TOFU pin, order-independent, verify, and a server-side key swap shows "changed".
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

/// Group reconcile also pins account keys (#277), so a swapped key flips verified → changed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn group_reconcile_tofu_detects_key_swap() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let carol_profile = carol.sign_up("carol@test.local").await;
    let alice_id = alice_profile.id.clone();
    let bob_id = bob_profile.id.clone();

    let group_id = alice.create_group("TOFU Group").await;
    alice.invite(&group_id, &bob_profile.username).await;
    let invite = bob
        .first_pending_invite()
        .await
        .expect("bob should see one pending invite");
    let invite_id = invite["id"].as_str().expect("invite id").to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;

    let pre = alice
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": alice_id, "peerUserId": bob_id }),
        )
        .await;
    assert_eq!(
        pre["status"], "unverified",
        "after invite+accept Alice should have Bob TOFU-pinned via group reconcile"
    );

    alice
        .invoke_json(
            "set_contact_verified",
            json!({ "peerUserId": bob_id, "verified": true }),
        )
        .await;

    let conn = world().await.remote.conn().await.expect("remote conn");
    conn.execute(
        "UPDATE users SET account_id_pub = ?1, identity_version = identity_version + 1 \
         WHERE id = ?2",
        libsql::params![vec![9u8; 32], bob_id.clone()],
    )
    .await
    .expect("swap bob account_id_pub");

    // Stale pin: old key still verified, but a fresh comparison flags the change.
    let stale = alice
        .invoke_json("list_peer_verifications", json!({}))
        .await;
    let stale_entries = stale.as_array().expect("array of PeerVerificationEntry");
    let stale_bob = stale_entries
        .iter()
        .find(|e| e["peer_user_id"] == bob_id)
        .expect("bob must be in list_peer_verifications");
    assert_eq!(
        stale_bob["verified"], true,
        "verified flag should still be true before the next reconcile"
    );
    assert_eq!(
        stale_bob["key_changed"], true,
        "list_peer_verifications must flag the Turso swap as key_changed before reconcile"
    );

    // The next reconcile (inviting Carol) batch-checks every roster key.
    alice.invite(&group_id, &carol_profile.username).await;

    let post = alice
        .invoke_json(
            "get_safety_number",
            json!({ "myUserId": alice_id, "peerUserId": bob_id }),
        )
        .await;
    assert_eq!(
        post["status"], "unverified",
        "after the batch helper refreshes the pin, status returns to 'unverified' — \
         the verified flag was cleared in place"
    );

    let listed = alice
        .invoke_json("list_peer_verifications", json!({}))
        .await;
    let entries = listed.as_array().expect("array of PeerVerificationEntry");
    let bob_entry = entries
        .iter()
        .find(|e| e["peer_user_id"] == bob_id)
        .expect("bob must be in list_peer_verifications");
    assert_eq!(
        bob_entry["verified"], false,
        "the previously-verified flag MUST be cleared — this is the user-visible \
         signal that a key change was detected through the group path"
    );
    assert_eq!(
        bob_entry["key_changed"], false,
        "the pin was refreshed in place, so a fresh comparison no longer \
         flags key_changed"
    );

    // The actor's own row is excluded.
    let alice_entry = entries.iter().find(|e| e["peer_user_id"] == alice_id);
    assert!(
        alice_entry.is_none(),
        "actor's own user_id must never end up in contact_verification"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// C4: `logout(delete_data = true)` removes the device row via a signed DS call before teardown.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn logout_with_delete_removes_device_via_ds() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let device_id: String = {
        let db = crate::harness::writable_remote().await;
        let conn = db.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT device_id FROM user_device WHERE user_id = ?1",
                libsql::params![user_id.clone()],
            )
            .await
            .expect("user_device select");
        rows.next()
            .await
            .expect("rows")
            .expect("device row present pre-logout")
            .get::<String>(0)
            .expect("device_id")
    };

    invoke::<()>(&alice.webview, "logout", json!({ "deleteData": true }))
        .await
        .expect("logout(delete_data=true)");

    let db = crate::harness::writable_remote().await;
    let conn = db.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT 1 FROM user_device WHERE device_id = ?1",
            libsql::params![device_id.clone()],
        )
        .await
        .expect("user_device exists query");
    assert!(
        rows.next().await.expect("rows").is_none(),
        "logout(delete_data=true) must remove this device's user_device row via the DS"
    );

    drop(alice);
}

/// #1256: deleting an account empties that user's media cache.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn delete_account_clears_the_users_media_cache() {
    wipe().await;

    // `set_media_cache_dir` is first-call-wins, so this is the process's root.
    let root = std::env::temp_dir().join(format!("pollis-flows-media-{}", std::process::id()));
    pollis_core::commands::r2::set_media_cache_dir(root.clone());

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let user_id = profile.id.clone();

    let user_dir = root.join(&user_id);
    std::fs::create_dir_all(&user_dir).expect("create user cache dir");
    let entry = user_dir.join("aaaa.png.enc");
    std::fs::write(&entry, b"cached-media-bytes").expect("seed cache entry");

    invoke::<()>(&alice.webview, "delete_account", json!({ "userId": user_id }))
        .await
        .expect("delete_account");

    assert!(
        !entry.exists(),
        "delete_account left the deleted user's cached media on disk"
    );

    drop(alice);
}

/// #1099: a stale local address index must not let the soft reset delete the device's own row.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_stale_local_address_index_does_not_strand_an_unenrolled_device() {
    wipe().await;

    let email = "stale-index@test.local";
    let mut primary = TestClient::new().await;
    let user_id = primary.sign_up(email).await.id;

    // Registered but not enrolled: the state the bug needs.
    let second = TestClient::new().await;
    invoke::<()>(&second.webview, "request_otp", json!({ "email": email }))
        .await
        .expect("request_otp on the second device");
    let first_profile: pollis_lib::commands::auth::UserProfile = invoke(
        &second.webview,
        "verify_otp",
        json!({ "email": email, "code": crate::harness::DEV_OTP }),
    )
    .await
    .expect("first verify_otp on the second device");
    assert!(
        first_profile.enrollment_required,
        "the second device must land on the enrollment path"
    );
    let first_device_id = second
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("the second device has a device_id after its first sign-in");

    // Mirrors `auth::LOGIN_EMAIL_KEY`.
    second
        .state
        .keystore
        .store_for_user("login_email", &user_id, b"moved-elsewhere@test.local")
        .await
        .expect("overwrite the stored login address");

    invoke::<()>(&second.webview, "request_otp", json!({ "email": email }))
        .await
        .expect("second request_otp");
    let _: pollis_lib::commands::auth::UserProfile = invoke(
        &second.webview,
        "verify_otp",
        json!({ "email": email, "code": crate::harness::DEV_OTP }),
    )
    .await
    .expect("second verify_otp on the second device");
    let session_device_id = second
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("device_id after the stale-index sign-in");
    assert_ne!(
        session_device_id, first_device_id,
        "the device must adopt the session's id, not keep the one the session cannot authorize"
    );

    // The device's id and its OTP session's id must be the same string.
    assert!(
        second.state.enrollment_session.lock().await.is_some(),
        "the sign-in must leave a usable session for the writes that follow"
    );

    // The DS keeps the session's device in the `reset-recover` wipe.
    let w = world().await;
    let conn = w.remote.conn().await.expect("remote conn");
    let rows_for_session_device: i64 = {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM user_device WHERE user_id = ?1 AND device_id = ?2",
                libsql::params![user_id.clone(), session_device_id.clone()],
            )
            .await
            .expect("user_device count");
        rows.next().await.expect("row").expect("count row").get(0).expect("count")
    };
    assert_eq!(
        rows_for_session_device, 1,
        "register-device under the session must have created the row the wipe keeps"
    );

    let persisted = second
        .state
        .keystore
        .load_for_user("device_id", &user_id)
        .await
        .expect("keystore read")
        .expect("device_id slot");
    assert_eq!(
        String::from_utf8(persisted).expect("utf8"),
        session_device_id,
        "the adopted id must be the persisted one"
    );
}

/// `last_seen` moves on signed DS use (it was frozen after #987); the row is backdated.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn using_a_device_moves_its_last_seen_past_its_creation() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let profile = alice.sign_up("alice@test.local").await;
    let uid = profile.id.clone();

    let long_ago = "2020-01-01 00:00:00";
    let db = writable_remote().await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "UPDATE user_device SET created_at = ?2, last_seen = ?2 WHERE user_id = ?1",
            libsql::params![uid.clone(), long_ago],
        )
        .await
        .unwrap();
    expire_ds_last_seen_throttle(&uid);

    alice.create_group("Last seen").await;
    alice.list_group_ids().await;

    let mut current = serde_json::Value::Null;
    for _ in 0..100 {
        let devices = alice.invoke_json("list_user_devices", json!({ "userId": uid })).await;
        current = devices
            .as_array()
            .and_then(|a| a.iter().find(|d| d["is_current"] == true).cloned())
            .expect("the current device is listed");
        if current["last_seen"].as_str() != Some(long_ago) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    assert_eq!(current["created_at"], long_ago, "creation time is untouched");
    let last_seen = current["last_seen"].as_str().expect("last_seen is served");
    assert!(
        last_seen > long_ago,
        "last_seen moves past created_at once the device is used: {current}"
    );
}
