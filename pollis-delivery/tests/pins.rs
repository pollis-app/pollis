//! Pinned messages (#99) — what the DS may and may not do with the pin rows
//! and the wrapped pin-key state.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use pollis_api::pins::{
    ListPinsBody, ListPinsResponse, PinKeystateBody, PinKeystateResponse, PinMessageBody,
    UnpinMessageBody, UpsertPinKeystateBody,
};
use pollis_api::DsRequest;
use pollis_delivery::pins::{
    apply_pin_message, apply_unpin_message, apply_upsert_keystate, KeystateOutcome, PinOutcome,
};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("pins.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Stands in for AES-GCM output: not UTF-8, not JSON.
const CIPHERTEXT: &[u8] = &[
    0xF5, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x91, 0x7A, 0x00, 0x00, 0xC3, 0x28,
];
const WRAP: &[u8] = &[0xAB; 48];

async fn seed_member(db: &common::TempDb, conversation_id: &str, user_id: &str) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
        libsql::params![conversation_id, user_id],
    )
    .await
    .unwrap();
}

/// Give `conv` a commit log whose head in `generation` is `head_epoch` (the keystate CAS ceiling).
async fn seed_head(db: &common::TempDb, conv: &str, generation: i64, head_epoch: i64) {
    let conn = db.conn().await.unwrap();
    for epoch in 0..head_epoch {
        conn.execute(
            "INSERT INTO mls_commit_log \
                 (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, 'alice', X'00')",
            libsql::params![conv, generation, epoch],
        )
        .await
        .unwrap();
    }
}

fn keystate(conv: &str, actor: &str, generation: i64, epoch: i64, mint: bool) -> UpsertPinKeystateBody {
    UpsertPinKeystateBody {
        conversation_id: conv.to_string(),
        wrapped_kpin: b64(WRAP),
        nonce: b64(&[7u8; 12]),
        generation,
        epoch,
        device_id: "dev-1".to_string(),
        actor_id: Some(actor.to_string()),
        mint,
    }
}

fn pin(conv: &str, message_id: &str, actor: &str, ct: &[u8]) -> PinMessageBody {
    PinMessageBody {
        id: format!("pin-{message_id}"),
        conversation_id: conv.to_string(),
        message_id: message_id.to_string(),
        pinned_by: Some(actor.to_string()),
        encrypted_content: b64(ct),
        nonce: b64(&[9u8; 12]),
    }
}

async fn read_keystate(db: &common::TempDb, conv: &str, actor: &str) -> (StatusCode, Option<PinKeystateResponse>) {
    let app = build_router_with_state(AppState::new(db.arc(), false));
    let body = serde_json::to_vec(&PinKeystateBody {
        conversation_id: conv.to_string(),
        user_id: Some(actor.to_string()),
    })
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(<PinKeystateBody as DsRequest>::PATH)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    if status != StatusCode::OK {
        return (status, None);
    }
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, Some(serde_json::from_slice(&bytes).unwrap()))
}

async fn read_pins(db: &common::TempDb, conv: &str, actor: &str) -> (StatusCode, Option<ListPinsResponse>) {
    let app = build_router_with_state(AppState::new(db.arc(), false));
    let body = serde_json::to_vec(&ListPinsBody {
        conversation_id: conv.to_string(),
        user_id: Some(actor.to_string()),
    })
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(<ListPinsBody as DsRequest>::PATH)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    if status != StatusCode::OK {
        return (status, None);
    }
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, Some(serde_json::from_slice(&bytes).unwrap()))
}

/// A second mint loses even at a newer epoch: the loser adopts, never forks the key.
#[tokio::test]
async fn a_second_mint_is_refused_even_at_a_newer_epoch() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    seed_member(&db, "conv1", "bob").await;
    seed_head(&db, "conv1", 0, 5).await;
    let conn = db.conn().await.unwrap();

    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 4, true))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: true }));

    let mut second = keystate("conv1", "bob", 0, 5, true);
    second.wrapped_kpin = b64(&[0xCD; 48]);
    let out = apply_upsert_keystate(&conn, &conn, Some("bob"), &second).await.unwrap();
    assert!(
        matches!(out, KeystateOutcome::Ok { updated: false }),
        "a second mint must be refused — accepting it forks the key lineage"
    );

    let (_, resp) = read_keystate(&db, "conv1", "alice").await;
    let ks = resp.unwrap().keystate.expect("row exists");
    assert_eq!(ks.wrapped_kpin, b64(WRAP), "the first mint's key must stand");
    assert_eq!(ks.epoch, 4);
}

#[tokio::test]
async fn rewrap_is_lexicographically_monotone() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    seed_head(&db, "conv1", 0, 7).await;
    let conn = db.conn().await.unwrap();
    apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 4, true))
        .await
        .unwrap();

    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 7, false))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: true }));

    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 5, false))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: false }));
    let (_, resp) = read_keystate(&db, "conv1", "alice").await;
    assert_eq!(resp.unwrap().keystate.unwrap().epoch, 7);

    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 7, false))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: false }));

    // A successor lineage at epoch 0 must win, or a suite migration strands the keystate.
    seed_head(&db, "conv1", 1, 1).await;
    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 1, 0, false))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: true }));
    let (_, resp) = read_keystate(&db, "conv1", "alice").await;
    let ks = resp.unwrap().keystate.unwrap();
    assert_eq!((ks.generation, ks.epoch), (1, 0));
}

#[tokio::test]
async fn a_non_member_cannot_touch_the_keystate() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    let conn = db.conn().await.unwrap();
    let out = apply_upsert_keystate(
        &conn,
        &conn,
        Some("mallory"),
        &keystate("conv1", "mallory", 0, 9, true),
    )
    .await
    .unwrap();
    assert!(matches!(out, KeystateOutcome::Forbidden));
    let (_, resp) = read_keystate(&db, "conv1", "alice").await;
    assert!(resp.unwrap().keystate.is_none(), "nothing may have been written");
}

#[tokio::test]
async fn a_malformed_keystate_envelope_is_a_client_error() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    let conn = db.conn().await.unwrap();

    let mut bad = keystate("conv1", "alice", 0, 1, true);
    bad.nonce = b64(&[1u8; 11]);
    assert!(matches!(
        apply_upsert_keystate(&conn, &conn, Some("alice"), &bad).await.unwrap(),
        KeystateOutcome::Invalid(_)
    ));

    let mut bad = keystate("conv1", "alice", 0, 1, true);
    bad.wrapped_kpin = "not base64!!!".into();
    assert!(matches!(
        apply_upsert_keystate(&conn, &conn, Some("alice"), &bad).await.unwrap(),
        KeystateOutcome::Invalid(_)
    ));

    let mut bad = keystate("conv1", "alice", 0, 1, true);
    bad.wrapped_kpin = b64(&[0u8; 4096]);
    assert!(matches!(
        apply_upsert_keystate(&conn, &conn, Some("alice"), &bad).await.unwrap(),
        KeystateOutcome::Invalid(_)
    ));
}

#[tokio::test]
async fn pin_ciphertext_round_trips_byte_for_byte() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    let conn = db.conn().await.unwrap();
    let out = apply_pin_message(&conn, Some("alice"), &pin("conv1", "m1", "alice", CIPHERTEXT))
        .await
        .unwrap();
    assert!(matches!(out, PinOutcome::Ok));

    let (_, resp) = read_pins(&db, "conv1", "alice").await;
    let pins = resp.unwrap().pins;
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].encrypted_content, b64(CIPHERTEXT));
    assert_eq!(pins[0].message_id, "m1");
    assert_eq!(pins[0].pinned_by, "alice");
}

#[tokio::test]
async fn pinning_twice_keeps_the_first_pin() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    seed_member(&db, "conv1", "bob").await;
    let conn = db.conn().await.unwrap();
    apply_pin_message(&conn, Some("alice"), &pin("conv1", "m1", "alice", CIPHERTEXT))
        .await
        .unwrap();
    let mut second = pin("conv1", "m1", "bob", &[0x01, 0x02]);
    second.id = "pin-other".into();
    let out = apply_pin_message(&conn, Some("bob"), &second).await.unwrap();
    assert!(matches!(out, PinOutcome::Ok), "a duplicate pin is a no-op, not an error");

    let (_, resp) = read_pins(&db, "conv1", "alice").await;
    let pins = resp.unwrap().pins;
    assert_eq!(pins.len(), 1);
    assert_eq!(pins[0].pinned_by, "alice");
    assert_eq!(pins[0].encrypted_content, b64(CIPHERTEXT));
}

#[tokio::test]
async fn membership_gates_writes_and_any_member_may_unpin() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    seed_member(&db, "conv1", "bob").await;
    let conn = db.conn().await.unwrap();

    let out = apply_pin_message(
        &conn,
        Some("mallory"),
        &pin("conv1", "m1", "mallory", CIPHERTEXT),
    )
    .await
    .unwrap();
    assert!(matches!(out, PinOutcome::Forbidden));

    apply_pin_message(&conn, Some("alice"), &pin("conv1", "m1", "alice", CIPHERTEXT))
        .await
        .unwrap();

    let out = apply_unpin_message(
        &conn,
        Some("mallory"),
        &UnpinMessageBody {
            conversation_id: "conv1".into(),
            message_id: "m1".into(),
            actor_id: Some("mallory".into()),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden));

    let out = apply_unpin_message(
        &conn,
        Some("bob"),
        &UnpinMessageBody {
            conversation_id: "conv1".into(),
            message_id: "m1".into(),
            actor_id: Some("bob".into()),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    let (_, resp) = read_pins(&db, "conv1", "alice").await;
    assert!(resp.unwrap().pins.is_empty());
}

/// Non-members get a 403 from both reads: even ciphertext leaks activity.
#[tokio::test]
async fn reads_are_membership_gated() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    seed_head(&db, "conv1", 0, 1).await;
    let conn = db.conn().await.unwrap();
    apply_pin_message(&conn, Some("alice"), &pin("conv1", "m1", "alice", CIPHERTEXT))
        .await
        .unwrap();
    apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate("conv1", "alice", 0, 1, true))
        .await
        .unwrap();

    let (status, _) = read_pins(&db, "conv1", "mallory").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = read_keystate(&db, "conv1", "mallory").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a_conversation_without_a_key_reads_as_none() {
    let db = fresh().await;
    seed_member(&db, "conv1", "alice").await;
    let (status, resp) = read_keystate(&db, "conv1", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert!(resp.unwrap().keystate.is_none());
    let (status, resp) = read_pins(&db, "conv1", "alice").await;
    assert_eq!(status, StatusCode::OK);
    assert!(resp.unwrap().pins.is_empty());
}
