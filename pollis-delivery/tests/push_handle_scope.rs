//! #1122: the push payload stops naming the conversation.

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

/// Asserted on the string: the one property a third party can check.
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
    // No substantial slice either: a truncated id is still a correlatable prefix.
    assert!(
        !handle.contains(&conversation[..12]),
        "the handle must not embed even a prefix of the conversation id: {handle}"
    );
}
