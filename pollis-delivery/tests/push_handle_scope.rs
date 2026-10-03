//! #1122: the push payload stops naming the conversation.
//!
//! Expo, APNs and FCM sit outside the overlay by design, so every field in a
//! notification's `data` is disclosed to three third parties on every message.
//! The payload was already content-free — no plaintext, no sender — but it
//! carried `conversationId`, which is exactly the "which conversation, when"
//! signal the metadata-minimisation design sets out to withhold.
//!
//! The DS now mints an opaque handle per notification and the client trades it
//! for the routing fields over its own authenticated channel. The properties
//! that make the handle worth having are pinned by the unit tests in
//! `src/push.rs` (same real schema): one handle per recipient, never reused
//! across notifications, a foreign handle indistinguishable from a missing one,
//! expiry plus sweep. This file keeps the one property a third party can check
//! from the outside — the handle string itself names no conversation. If any of
//! them slips, the handle is just a slower way of leaking the same thing.

use pollis_delivery::push::mint_push_handles;

mod common;

const ALICE: &str = "alice-1";

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
