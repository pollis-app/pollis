//! `POST /v1/push/fetch` (#1804): a push notification's resolve, Welcome read,
//! catch-up and DM peer keys in one round trip, driven through the real router.
//!
//! The invariant: it answers exactly what the separate endpoints answer for the
//! same signed device, and nothing for a handle that is not the caller's.

use std::sync::Arc;

use base64::Engine as _;
use ml_dsa::{Keypair as _, MlDsa44, SigningKey};
use pollis_delivery::db::Db;
use pollis_delivery::AppState;

mod common;

const DM: &str = "dm-push-fetch";

struct Account {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

async fn seed_account(db: &Db, user_id: &str, account_key: Option<&[u8]>) -> Account {
    let key = common::gen_signing_key();
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO users (id, email, username, account_id_pub, identity_version) \
         VALUES (?1, ?1 || '@x', ?1, ?2, 1)",
        libsql::params![user_id, account_key.map(<[u8]>::to_vec)],
    )
    .await
    .unwrap();
    let device_id = format!("{user_id}-dev");
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id.clone(), user_id, key.verifying_key().encode().to_vec()],
    )
    .await
    .unwrap();
    Account {
        user_id: user_id.to_string(),
        device_id,
        key,
    }
}

async fn seed_dm(db: &Db, members: &[&str]) {
    let conn = db.conn().await.unwrap();
    conn.execute("INSERT INTO conversation (id, kind) VALUES (?1, 'dm')", libsql::params![DM])
        .await
        .unwrap();
    conn.execute(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, '2026-01-01')",
        libsql::params![DM, members[0]],
    )
    .await
    .unwrap();
    for user in members {
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at) \
             VALUES (?1, ?2, ?3, '2026-01-01', '2026-01-01')",
            libsql::params![DM, *user, members[0]],
        )
        .await
        .unwrap();
    }
    conn.execute(
        "INSERT INTO message_envelope (id, conversation_id, sent_at, sender_id, ciphertext, seq) \
         VALUES ('env-1', ?1, datetime('now'), 'sealed', 'ct', 1)",
        libsql::params![DM],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, 1)",
        libsql::params![DM],
    )
    .await
    .unwrap();
}

async fn handle_for(db: &Db, user: &str, kind: &str) -> String {
    let conn = db.conn().await.unwrap();
    let handles = pollis_delivery::push::mint_push_handles(&conn, &[user.to_string()], DM, kind).await;
    handles.get(user).expect("minted").clone()
}

async fn post(state: &AppState, who: &Account, path: &str, body: serde_json::Value) -> serde_json::Value {
    let (status, json) =
        common::send_signed(state, path, body, &who.user_id, &who.device_id, &who.key).await;
    assert!(status.is_success(), "{path} answered {status}: {json}");
    json
}

/// One request answers what resolve + welcomes/fetch + catch-up + account-keys answer.
#[tokio::test(flavor = "multi_thread")]
async fn a_dm_push_answers_what_the_separate_reads_answer() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice", Some(b"alice-key")).await;
    let _bob = seed_account(&db, "bob", Some(b"bob-key")).await;
    seed_dm(&db, &["alice", "bob"]).await;
    let handle = handle_for(&db, "alice", "dm").await;
    let state = AppState::new(Arc::clone(&db), true);

    let got = post(
        &state,
        &alice,
        "/v1/push/fetch",
        serde_json::json!({ "handle": handle, "count_key_packages_in_suite": 1 }),
    )
    .await;

    assert_eq!(got["conversation_id"], DM);
    assert_eq!(got["kind"], "dm");

    let welcomes = post(
        &state,
        &alice,
        "/v1/welcomes/fetch",
        serde_json::json!({ "count_key_packages_in_suite": 1 }),
    )
    .await;
    assert_eq!(got["welcomes"], welcomes, "the Welcome read must match /v1/welcomes/fetch");

    let catch_up = post(
        &state,
        &alice,
        "/v1/conversations/catch-up",
        serde_json::json!({
            "conversation_id": DM,
            "want_envelopes": true,
            "want_dm": true,
            "device_id": alice.device_id,
        }),
    )
    .await;
    assert_eq!(got["catch_up"], catch_up, "the catch-up must match /v1/conversations/catch-up");
    assert_eq!(got["catch_up"]["envelopes"][0]["id"], "env-1");

    let keys = post(
        &state,
        &alice,
        "/v1/read/account-keys",
        serde_json::json!({ "user_ids": ["bob"] }),
    )
    .await;
    assert_eq!(got["account_keys"], keys["keys"], "the peer keys must match /v1/read/account-keys");
    assert_eq!(
        got["account_keys"][0]["account_id_pub"],
        base64::engine::general_purpose::STANDARD.encode(b"bob-key")
    );
    assert_eq!(got["account_keys_for"], serde_json::json!(["bob"]));
}

/// Someone else's handle reads as unknown: no conversation, no content, no keys.
#[tokio::test(flavor = "multi_thread")]
async fn another_users_handle_answers_nothing() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice", Some(b"alice-key")).await;
    let bob = seed_account(&db, "bob", Some(b"bob-key")).await;
    seed_dm(&db, &["alice", "bob"]).await;
    let alices = handle_for(&db, "alice", "dm").await;
    let state = AppState::new(Arc::clone(&db), true);

    for handle in [alices.as_str(), "no-such-handle"] {
        let got = post(&state, &bob, "/v1/push/fetch", serde_json::json!({ "handle": handle })).await;
        assert!(got["conversation_id"].is_null(), "{handle}: {got}");
        assert!(got["kind"].is_null());
        assert!(got["welcomes"].is_null());
        assert!(got["catch_up"].is_null());
        assert_eq!(got["account_keys"], serde_json::json!([]));
    }
}

/// A DM request shows who is asking and nothing they wrote: no envelopes, no Welcomes.
#[tokio::test(flavor = "multi_thread")]
async fn a_dm_request_carries_the_members_but_no_envelopes() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice", Some(b"alice-key")).await;
    let _bob = seed_account(&db, "bob", None).await;
    seed_dm(&db, &["bob", "alice"]).await;
    let handle = handle_for(&db, "alice", pollis_api::devices::PUSH_KIND_DM_REQUEST).await;
    let state = AppState::new(Arc::clone(&db), true);

    let got = post(&state, &alice, "/v1/push/fetch", serde_json::json!({ "handle": handle })).await;
    assert_eq!(got["kind"], pollis_api::devices::PUSH_KIND_DM_REQUEST);
    assert!(got["welcomes"].is_null());
    assert_eq!(got["catch_up"]["envelopes"], serde_json::json!([]));
    assert_eq!(got["catch_up"]["dm"]["id"], DM);
    assert_eq!(got["account_keys"], serde_json::json!([]));
}

/// A body naming another user is refused, not ignored.
#[tokio::test(flavor = "multi_thread")]
async fn a_body_naming_another_user_is_forbidden() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice", None).await;
    let state = AppState::new(Arc::clone(&db), true);
    let (status, _) = common::send_signed(
        &state,
        "/v1/push/fetch",
        serde_json::json!({ "handle": "h", "user_id": "bob" }),
        &alice.user_id,
        &alice.device_id,
        &alice.key,
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::FORBIDDEN);
}
