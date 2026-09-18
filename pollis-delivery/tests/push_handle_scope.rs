//! #1122: the push payload stops naming the conversation.
//!
//! Expo, APNs and FCM sit outside the overlay by design, so every field in a
//! notification's `data` is disclosed to three third parties on every message.
//! The payload was already content-free — no plaintext, no sender — but it
//! carried `conversationId`, which is exactly the "which conversation, when"
//! signal the metadata-minimisation design sets out to withhold.
//!
//! The DS now mints an opaque handle per notification and the client trades it
//! for the routing fields over its own authenticated channel. These tests pin
//! the properties that make the handle worth having; if any of them slips, the
//! handle is just a slower way of leaking the same thing.

use pollis_delivery::push::{
    lookup_push_handle, mint_push_handles, sweep_push_handles, PUSH_HANDLE_TTL_DAYS,
};

mod common;

const ALICE: &str = "alice-1";
const BOB: &str = "bob-1";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('alice-1','a@x','alice');\
         INSERT INTO users (id, email, username) VALUES ('bob-1','b@x','bob');",
    )
    .await
    .expect("seed");
    db
}

/// The payload's own promise: a handle carries no conversation id.
///
/// Asserted on the STRING, because this is the one property a third party can
/// check. A handle that happened to embed or encode the conversation would
/// satisfy every other test here and leak anyway.
#[tokio::test]
async fn a_handle_does_not_contain_the_conversation_it_points_at() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let conversation = "conv-secret-01M2PUSHHANDLE";
    let handles = mint_push_handles(&conn, &[ALICE.to_string()], conversation, "dm").await;
    let handle = handles.get(ALICE).expect("minted");

    assert!(
        !handle.contains(conversation),
        "the handle must not embed the conversation id: {handle}"
    );
    // And no substantial slice of it either — a truncated id would still be a
    // correlatable prefix to a provider that sees many notifications.
    assert!(
        !handle.contains(&conversation[..12]),
        "the handle must not embed even a prefix of the conversation id: {handle}"
    );
}

/// Two recipients of the SAME message get different handles.
///
/// This is what stops a provider linking a notification it delivered to one
/// user with the one it delivered to another: without it, a shared handle names
/// the conversation just as well as the id did.
#[tokio::test]
async fn one_message_yields_a_distinct_handle_per_recipient() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let handles = mint_push_handles(
        &conn,
        &[ALICE.to_string(), BOB.to_string()],
        "conv-1",
        "channel",
    )
    .await;

    let a = handles.get(ALICE).expect("alice");
    let b = handles.get(BOB).expect("bob");
    assert_ne!(a, b, "two recipients must not share a handle");

    // Each resolves for its owner and for nobody else.
    assert!(lookup_push_handle(&conn, ALICE, a).await.unwrap().is_some());
    assert!(lookup_push_handle(&conn, BOB, b).await.unwrap().is_some());
    assert!(
        lookup_push_handle(&conn, ALICE, b).await.unwrap().is_none(),
        "alice must not resolve bob's handle"
    );
}

/// The same conversation, notified twice, yields unrelated handles.
///
/// The reason the design is a random handle rather than an HMAC of the
/// conversation id: a stable pseudonym would let a provider COUNT a
/// conversation without naming it, and "this handle fires 40 times a day" is
/// most of what the metadata was worth.
#[tokio::test]
async fn repeat_notifications_for_one_conversation_are_unlinkable() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let users = vec![ALICE.to_string()];

    let mut seen = std::collections::HashSet::new();
    for _ in 0..16 {
        let h = mint_push_handles(&conn, &users, "conv-1", "dm")
            .await
            .remove(ALICE)
            .expect("minted");
        assert!(seen.insert(h), "a handle was reused for the same conversation");
    }
    assert_eq!(seen.len(), 16);
}

/// Resolving is scoped to the owner, and a handle that is not yours is
/// indistinguishable from one that never existed.
///
/// A distinct answer for "not yours" would confirm the handle is real, which is
/// the single thing an opaque handle exists to avoid.
#[tokio::test]
async fn a_foreign_handle_is_indistinguishable_from_a_nonexistent_one() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let handles = mint_push_handles(&conn, &[ALICE.to_string()], "conv-1", "dm").await;
    let alices = handles.get(ALICE).unwrap();

    assert_eq!(
        lookup_push_handle(&conn, BOB, alices).await.unwrap(),
        lookup_push_handle(&conn, BOB, "never-was-a-handle").await.unwrap(),
        "a foreign handle and an absent one must answer identically"
    );
}

/// The TTL is the mitigation for this table's one real cost, so it has to be a
/// property rather than a comment.
///
/// The DS gains a durable record that a notification for conversation X went to
/// user Y — something it previously knew only for the instant it built the
/// payload. Expiry plus a sweep is what keeps that from becoming permanent.
#[tokio::test]
async fn an_expired_handle_resolves_to_nothing_and_is_collected() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let handles = mint_push_handles(&conn, &[ALICE.to_string()], "conv-1", "dm").await;
    let stale = handles.get(ALICE).unwrap().clone();

    conn.execute(
        "UPDATE push_handle SET created_at = datetime('now', ?1)",
        libsql::params![format!("-{} days", PUSH_HANDLE_TTL_DAYS + 1)],
    )
    .await
    .unwrap();

    assert!(
        lookup_push_handle(&conn, ALICE, &stale).await.unwrap().is_none(),
        "an expired handle must not resolve"
    );

    let fresh_handles = mint_push_handles(&conn, &[ALICE.to_string()], "conv-2", "dm").await;
    let live = fresh_handles.get(ALICE).unwrap().clone();

    assert_eq!(sweep_push_handles(&conn).await.unwrap(), 1, "only the stale row");
    assert!(
        lookup_push_handle(&conn, ALICE, &live).await.unwrap().is_some(),
        "the sweep must not take live handles with it"
    );
}
