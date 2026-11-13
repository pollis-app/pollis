//! #1851: the DS never stores or hands out an MLS object that would put a
//! leaf in a group unless the claimed account certified that leaf's device key
//! (`pollis_delivery::leaf_cert`). Every object here is real openmls output.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_delivery::commit::{submit_commit, SubmitBody, SubmitResponse, SubmitVerdict, WelcomeBody};
use pollis_delivery::devices::{
    apply_claim_key_package, apply_publish_key_packages, apply_replenish_key_packages,
    ClaimKeyPackageBody, ClaimOutcome, PublishKeyPackagesBody, ReplenishKeyPackagesBody,
};
use pollis_delivery::writes::{apply_welcomes_resubmit, ResubmitBody, ResubmitOutcome, WriteOutcome};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

use common::mls::{b64, plant_key_package, Kp, MlsAccount, MlsDevice};

const CONV: &str = "conv-1851";

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("leaf_cert.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// alice and bob in one group, each with one certified device.
async fn alice_and_bob(conn: &libsql::Connection) -> (MlsAccount, MlsDevice, MlsAccount, MlsDevice) {
    let alice = MlsAccount::new(conn, "alice").await;
    let alice_dev = alice.device(conn, "alice-d1").await;
    let bob = MlsAccount::new(conn, "bob").await;
    let bob_dev = bob.device(conn, "bob-d1").await;
    for u in ["alice", "bob"] {
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES (?1, ?2)",
            libsql::params![CONV, u],
        )
        .await
        .unwrap();
    }
    (alice, alice_dev, bob, bob_dev)
}

fn publish_body(device: &MlsDevice, kps: &[&Kp]) -> PublishKeyPackagesBody {
    PublishKeyPackagesBody {
        device_id: device.device_id.clone(),
        packages: kps.iter().map(|k| k.entry()).collect(),
        user_id: None,
    }
}

async fn stored_packages(conn: &libsql::Connection, user_id: &str) -> i64 {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package WHERE user_id = ?1",
            libsql::params![user_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn claim_body(device: &MlsDevice) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: device.user_id.clone(),
        target_device_id: Some(device.device_id.clone()),
        ciphersuite: None,
    }
}

fn submit_body(sender: &MlsDevice, epoch: i64, add: &common::mls::AddBundle, recipient: &MlsDevice) -> SubmitBody {
    SubmitBody {
        conversation_id: CONV.to_string(),
        generation: 0,
        based_on_epoch: epoch,
        closes_epoch: None,
        sender_id: sender.user_id.clone(),
        commit: b64(&add.commit),
        added_user_id: Some(recipient.user_id.clone()),
        added_device_ids: Some(recipient.device_id.clone()),
        group_info: Some(b64(&add.group_info)),
        welcomes: vec![WelcomeBody {
            recipient_id: recipient.user_id.clone(),
            recipient_device_id: recipient.device_id.clone(),
            welcome: b64(&add.welcome),
        }],
    }
}

async fn head(conn: &libsql::Connection) -> i64 {
    pollis_delivery::commit::head_epoch(conn, CONV).await.unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_certified_key_package_is_stored_and_served() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, _, bob, bob_dev) = alice_and_bob(&conn).await;
    let kp = bob_dev.key_package();

    let out = apply_publish_key_packages(&conn, Some(&bob.user_id), &publish_body(&bob_dev, &[&kp]))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");

    match apply_claim_key_package(&conn, Some("alice"), &claim_body(&bob_dev)).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            assert_eq!(ref_hash, kp.ref_hash);
            assert_eq!(key_package, kp.bytes);
        }
        other => panic!("a certified package must be served, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_package_on_an_uncertified_key_is_refused_at_publish() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, _, bob, bob_dev) = alice_and_bob(&conn).await;
    let good = bob_dev.key_package();
    let forged = bob_dev.forged_key_package();

    // One bad package refuses the whole request; nothing is stored.
    let out = apply_publish_key_packages(
        &conn,
        Some(&bob.user_id),
        &publish_body(&bob_dev, &[&good, &forged]),
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");
    assert_eq!(stored_packages(&conn, "bob").await, 0);

    let out = apply_replenish_key_packages(
        &conn,
        Some(&bob.user_id),
        &ReplenishKeyPackagesBody {
            device_id: bob_dev.device_id.clone(),
            packages: vec![forged.entry()],
            user_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");
    assert_eq!(stored_packages(&conn, "bob").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_package_for_someone_elses_device_is_refused_at_publish() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (alice, alice_dev, _, bob_dev) = alice_and_bob(&conn).await;

    // alice files bob's (genuine, certified) package under her own account.
    let out = apply_publish_key_packages(
        &conn,
        Some(&alice.user_id),
        &publish_body(&alice_dev, &[&bob_dev.key_package()]),
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");

    // …or under a mislabelled reference.
    let mut kp = alice_dev.key_package();
    kp.ref_hash = "00".repeat(32);
    let out = apply_publish_key_packages(&conn, Some(&alice.user_id), &publish_body(&alice_dev, &[&kp]))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");

    // Bytes that are not a KeyPackage at all.
    let junk = Kp {
        ref_hash: "ab".repeat(32),
        bytes: b"kp-bytes".to_vec(),
        last_resort: false,
        suite: common::mls::SUITE,
    };
    let out = apply_publish_key_packages(&conn, Some(&alice.user_id), &publish_body(&alice_dev, &[&junk]))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Invalid(_)), "{out:?}");
    assert_eq!(stored_packages(&conn, "alice").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_uncertified_package_already_in_the_table_is_burnt_not_served() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, _, _, bob_dev) = alice_and_bob(&conn).await;
    let forged = bob_dev.forged_key_package();
    let good = bob_dev.key_package();
    // The forged row is the oldest, so it is drawn first.
    plant_key_package(&conn, &bob_dev, &forged).await;
    conn.execute(
        "UPDATE mls_key_package SET created_at = '2000-01-01 00:00:00' WHERE ref_hash = ?1",
        libsql::params![forged.ref_hash.clone()],
    )
    .await
    .unwrap();
    plant_key_package(&conn, &bob_dev, &good).await;

    match apply_claim_key_package(&conn, Some("alice"), &claim_body(&bob_dev)).await.unwrap() {
        ClaimOutcome::Claimed { ref_hash, .. } => assert_eq!(ref_hash, good.ref_hash),
        other => panic!("the certified package must be served, got {other:?}"),
    }
    assert_eq!(stored_packages(&conn, "bob").await, 1, "the forged row is deleted");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_devices_packages_are_never_served() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, _, bob, bob_dev) = alice_and_bob(&conn).await;
    let kp = bob_dev.key_package();
    let last_resort = bob_dev.last_resort_key_package();
    let out = apply_publish_key_packages(
        &conn,
        Some(&bob.user_id),
        &publish_body(&bob_dev, &[&kp, &last_resort]),
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");

    conn.execute(
        "UPDATE user_device SET revoked_at = datetime('now') WHERE device_id = ?1",
        libsql::params![bob_dev.device_id.clone()],
    )
    .await
    .unwrap();

    let out = apply_claim_key_package(&conn, Some("alice"), &claim_body(&bob_dev)).await.unwrap();
    assert!(matches!(out, ClaimOutcome::NoKeyPackage), "{out:?}");
    assert_eq!(stored_packages(&conn, "bob").await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_package_is_not_served_after_the_account_key_is_reset() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, _, bob, bob_dev) = alice_and_bob(&conn).await;
    let kp = bob_dev.key_package();
    let out = apply_publish_key_packages(&conn, Some(&bob.user_id), &publish_body(&bob_dev, &[&kp]))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");

    // A new account key: the old cert no longer chains to it.
    MlsAccount::new(&conn, "bob").await;
    let out = apply_claim_key_package(&conn, Some("alice"), &claim_body(&bob_dev)).await.unwrap();
    assert!(matches!(out, ClaimOutcome::NoKeyPackage), "{out:?}");
}

/// A commit bundle whose Welcome and GroupInfo admit only certified leaves is accepted.
#[tokio::test(flavor = "multi_thread")]
async fn a_commit_adding_a_certified_device_is_accepted() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, alice_dev, bob, bob_dev) = alice_and_bob(&conn).await;
    let kp = bob_dev.key_package();
    apply_publish_key_packages(&conn, Some(&bob.user_id), &publish_body(&bob_dev, &[&kp]))
        .await
        .unwrap();
    let ClaimOutcome::Claimed { .. } =
        apply_claim_key_package(&conn, Some("alice"), &claim_body(&bob_dev)).await.unwrap()
    else {
        panic!("claim");
    };

    let mut group = alice_dev.create_group(CONV);
    let add = alice_dev.add(&mut group, &[&kp]);
    let out = submit_commit(&conn, &conn, &submit_body(&alice_dev, 0, &add, &bob_dev))
        .await
        .unwrap();
    assert!(
        matches!(out, SubmitVerdict::Response(SubmitResponse::Accepted { .. })),
        "{out:?}"
    );
    assert_eq!(head(&conn).await, 1);
}

/// The committer adds a KeyPackage the DS never accepted (a forgery for a
/// real device): both the Welcome and the GroupInfo give it away, and nothing
/// is written.
#[tokio::test(flavor = "multi_thread")]
async fn a_commit_adding_an_uncertified_leaf_is_refused() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, alice_dev, _, bob_dev) = alice_and_bob(&conn).await;
    let forged = bob_dev.forged_key_package();

    let mut group = alice_dev.create_group(CONV);
    let add = alice_dev.add(&mut group, &[&forged]);

    // The full bundle.
    let out = submit_commit(&conn, &conn, &submit_body(&alice_dev, 0, &add, &bob_dev))
        .await
        .unwrap();
    assert!(matches!(out, SubmitVerdict::Invalid(_)), "{out:?}");

    // Without the Welcome: the GroupInfo alone gives the leaf away.
    let mut no_welcome = submit_body(&alice_dev, 0, &add, &bob_dev);
    no_welcome.welcomes.clear();
    let out = submit_commit(&conn, &conn, &no_welcome).await.unwrap();
    assert!(matches!(out, SubmitVerdict::Invalid(_)), "{out:?}");

    // Without the GroupInfo: the Welcome alone gives it away.
    let mut no_group_info = submit_body(&alice_dev, 0, &add, &bob_dev);
    no_group_info.group_info = None;
    let out = submit_commit(&conn, &conn, &no_group_info).await.unwrap();
    assert!(matches!(out, SubmitVerdict::Invalid(_)), "{out:?}");

    assert_eq!(head(&conn).await, 0, "nothing was written");

    // Even planted in the table, the forgery is not a certified device's package.
    plant_key_package(&conn, &bob_dev, &forged).await;
    let out = submit_commit(&conn, &conn, &submit_body(&alice_dev, 0, &add, &bob_dev))
        .await
        .unwrap();
    assert!(matches!(out, SubmitVerdict::Invalid(_)), "{out:?}");
    assert_eq!(head(&conn).await, 0);
}

/// A leaf that slipped in through a commit carrying no GroupInfo is caught by
/// the next GroupInfo published for the conversation, through either path.
#[tokio::test(flavor = "multi_thread")]
async fn a_group_info_is_diffed_against_the_stored_one() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, alice_dev, _, bob_dev) = alice_and_bob(&conn).await;
    let state = AppState::new(db.arc(), false);

    let mut group = alice_dev.create_group(CONV);
    // Epoch 0's commit row so the GroupInfo is not above the head; its GroupInfo is the baseline.
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
         VALUES (?1, 0, 0, 'alice', X'00')",
        libsql::params![CONV],
    )
    .await
    .unwrap();
    let baseline = alice_dev.group_info(&group);
    let req = |epoch: i64, gi: &[u8]| {
        Request::builder()
            .method("POST")
            .uri("/v1/group-info")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "conversation_id": CONV,
                    "epoch": epoch,
                    "group_info": b64(gi),
                    "updated_by_device_id": "alice-d1",
                }))
                .unwrap(),
            ))
            .unwrap()
    };
    let resp = build_router_with_state(state.clone()).oneshot(req(0, &baseline)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK, "alice's own certified leaf passes");

    let forged = bob_dev.forged_key_package();
    let add = alice_dev.add(&mut group, &[&forged]);
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
         VALUES (?1, 0, 1, 'alice', X'00')",
        libsql::params![CONV],
    )
    .await
    .unwrap();
    let resp = build_router_with_state(state.clone()).oneshot(req(1, &add.group_info)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST, "the new leaf is not certified");

    // A placeholder that is not a GroupInfo cannot be checked, so it is not stored.
    let resp = build_router_with_state(state).oneshot(req(1, b"tree")).await.unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resubmitted_welcome_is_held_to_the_same_check() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let (_, alice_dev, bob, bob_dev) = alice_and_bob(&conn).await;
    conn.execute(
        "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
         VALUES (?1, 0, 0, 'alice', X'00')",
        libsql::params![CONV],
    )
    .await
    .unwrap();

    let resubmit = |welcome: &[u8]| ResubmitBody {
        conversation_id: CONV.to_string(),
        generation: 0,
        recipient_id: bob_dev.user_id.clone(),
        recipient_device_id: bob_dev.device_id.clone(),
        welcome: b64(welcome),
    };

    let forged = bob_dev.forged_key_package();
    let mut group = alice_dev.create_group(CONV);
    let bad = alice_dev.add(&mut group, &[&forged]);
    let out = apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(&bad.welcome))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Invalid(_)), "{out:?}");

    let kp = bob_dev.key_package();
    apply_publish_key_packages(&conn, Some(&bob.user_id), &publish_body(&bob_dev, &[&kp]))
        .await
        .unwrap();
    let mut group = alice_dev.create_group("other-conv");
    let good = alice_dev.add(&mut group, &[&kp]);
    let out = apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(&good.welcome))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok), "{out:?}");

    // A genuine Welcome for bob, re-addressed to another of his devices.
    let bob_d2 = bob.device(&conn, "bob-d2").await;
    let out = apply_welcomes_resubmit(
        &conn,
        &conn,
        Some("alice"),
        &ResubmitBody {
            recipient_device_id: bob_d2.device_id.clone(),
            ..resubmit(&good.welcome)
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, ResubmitOutcome::Invalid(_)), "{out:?}");
}
