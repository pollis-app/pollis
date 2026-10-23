//! The attachment key ring (#1382): every write that would fork the ring, roll
//! a rotation back, or freeze the row is refused by the DS itself.
//!
//! A forked ring is the failure that matters: two devices holding different
//! newest keys would seal files the other half of the conversation can never
//! open. The DS cannot see the keys, so it enforces the one thing it can see —
//! the ring's length — under three rules (mint = 1, re-wrap = same, rotate =
//! +1). These tests pin each rule.

use base64::Engine as _;
use pollis_delivery::file_keys::{
    apply_upsert_keystate, read_stored, FileKeystateOp, FileKeystateOutcome,
    UpsertFileKeystateBody,
};

mod common;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Group `g1`: alice and bob are members, mallory is not. The commit log's
/// head is epoch 10, so writes up to it are within bounds.
async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("file-keystate.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');",
    )
    .await
    .unwrap();
    for epoch in 0..10 {
        conn.execute(
            "INSERT INTO mls_commit_log \
                 (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES ('g1', 0, ?1, 'alice', X'00')",
            libsql::params![epoch],
        )
        .await
        .unwrap();
    }
    db
}

/// A wrap of the right shape for a ring of `key_version` keys.
fn body(actor: &str, op: FileKeystateOp, key_version: i64, epoch: i64) -> UpsertFileKeystateBody {
    UpsertFileKeystateBody {
        conversation_id: "g1".into(),
        wrapped_keyring: b64(&vec![7u8; key_version as usize * 48 + 16]),
        nonce: b64(&[1u8; 12]),
        key_version,
        generation: 0,
        epoch,
        device_id: format!("{actor}-dev"),
        actor_id: Some(actor.into()),
        op,
    }
}

async fn apply(db: &common::TempDb, b: &UpsertFileKeystateBody) -> FileKeystateOutcome {
    let conn = db.conn().await.unwrap();
    let actor = b.actor_id.clone();
    apply_upsert_keystate(&conn, &conn, actor.as_deref(), b).await.unwrap()
}

async fn stored_version(db: &common::TempDb) -> Option<(i64, i64)> {
    let conn = db.conn().await.unwrap();
    read_stored(&conn, "g1")
        .await
        .unwrap()
        .map(|k| (k.key_version, k.epoch))
}

fn landed(out: &FileKeystateOutcome) -> bool {
    matches!(out, FileKeystateOutcome::Ok { updated: true })
}

fn refused(out: &FileKeystateOutcome) -> bool {
    matches!(out, FileKeystateOutcome::Ok { updated: false })
}

#[tokio::test]
async fn only_the_first_mint_lands_so_two_minters_cannot_fork_the_ring() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    // Bob minted his own ring concurrently, at a NEWER epoch: he must lose and
    // adopt alice's, never replace a ring files may already be sealed under.
    assert!(refused(&apply(&db, &body("bob", FileKeystateOp::Mint, 1, 5)).await));
    assert_eq!(stored_version(&db).await, Some((1, 2)));
}

#[tokio::test]
async fn a_mint_must_be_a_ring_of_one_key() {
    let db = fresh().await;
    let out = apply(&db, &body("alice", FileKeystateOp::Mint, 2, 2)).await;
    assert!(matches!(out, FileKeystateOutcome::Invalid(_)));
    assert_eq!(stored_version(&db).await, None);
}

#[tokio::test]
async fn a_rotation_appends_exactly_one_key() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    // Skipping a version would let a device invent keys nobody else holds.
    assert!(refused(&apply(&db, &body("alice", FileKeystateOp::Rotate, 3, 3)).await));
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Rotate, 2, 3)).await));
    assert_eq!(stored_version(&db).await, Some((2, 3)));
}

#[tokio::test]
async fn two_concurrent_rotations_cannot_both_land() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Rotate, 2, 3)).await));
    // Bob rotated from the same v1 ring, so he also claims v2 — with a
    // DIFFERENT new key. It must not replace alice's.
    assert!(refused(&apply(&db, &body("bob", FileKeystateOp::Rotate, 2, 4)).await));
    assert_eq!(stored_version(&db).await, Some((2, 3)));
}

#[tokio::test]
async fn a_rewrap_of_the_old_ring_cannot_roll_back_a_rotation() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Rotate, 2, 3)).await));
    // Bob still holds the v1 ring and re-wraps it at a newer epoch. Landing it
    // would drop the key that keeps the removed member out of new files.
    assert!(refused(&apply(&db, &body("bob", FileKeystateOp::Rewrap, 1, 6)).await));
    assert_eq!(stored_version(&db).await, Some((2, 3)));
    // A re-wrap of the CURRENT ring at a newer epoch lands.
    assert!(landed(&apply(&db, &body("bob", FileKeystateOp::Rewrap, 2, 6)).await));
    assert_eq!(stored_version(&db).await, Some((2, 6)));
}

#[tokio::test]
async fn a_rewrap_never_moves_the_epoch_backwards() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 5)).await));
    assert!(refused(&apply(&db, &body("bob", FileKeystateOp::Rewrap, 1, 4)).await));
    assert!(refused(&apply(&db, &body("bob", FileKeystateOp::Rewrap, 1, 5)).await));
    assert_eq!(stored_version(&db).await, Some((1, 5)));
}

#[tokio::test]
async fn a_wrap_whose_length_disagrees_with_key_version_is_refused() {
    let db = fresh().await;
    let mut b = body("alice", FileKeystateOp::Mint, 1, 2);
    b.wrapped_keyring = b64(&[7u8; 96 + 16]);
    assert!(matches!(apply(&db, &b).await, FileKeystateOutcome::Invalid(_)));
}

#[tokio::test]
async fn a_non_member_can_neither_mint_nor_rotate() {
    let db = fresh().await;
    let out = apply(&db, &body("mallory", FileKeystateOp::Mint, 1, 2)).await;
    assert!(matches!(out, FileKeystateOutcome::Forbidden));
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    let out = apply(&db, &body("mallory", FileKeystateOp::Rotate, 2, 3)).await;
    assert!(matches!(out, FileKeystateOutcome::Forbidden));
    assert_eq!(stored_version(&db).await, Some((1, 2)));
}

#[tokio::test]
async fn an_epoch_above_the_commit_log_head_cannot_freeze_the_ring() {
    let db = fresh().await;
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Mint, 1, 2)).await));
    let out = apply(&db, &body("bob", FileKeystateOp::Rewrap, 1, 1_000_000)).await;
    assert!(matches!(out, FileKeystateOutcome::AheadOfHead { .. }));
    // The honest re-wrap still lands afterwards.
    assert!(landed(&apply(&db, &body("alice", FileKeystateOp::Rewrap, 1, 9)).await));
}
