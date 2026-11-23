//! #2084: a profile change wakes every room that shows the user's name, and says nothing else.

use pollis_delivery::profile::{profile_change_rooms, profile_changed_event};

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('ann','ann@x','ann');\
         INSERT INTO users (id, email, username) VALUES ('bo','bo@x','bo');\
         INSERT INTO users (id, email, username) VALUES ('cy','cy@x','cy');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','One','ann');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','ann','admin');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','bo','member');\
         INSERT INTO conversation (id, kind) VALUES ('grp-2','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-2','Two','cy');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-2','cy','admin');\
         INSERT INTO conversation (id, kind) VALUES ('dm-1','dm');\
         INSERT INTO dm_channel (id, created_by) VALUES ('dm-1','ann');\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) VALUES ('dm-1','ann','ann',datetime('now'));\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-1','cy','ann');",
    )
    .await
    .expect("seed");
    db
}

#[tokio::test]
async fn a_rename_reaches_the_inbox_and_every_group_and_dm_room_of_the_user() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let rooms = profile_change_rooms(&conn, "ann").await.unwrap();
    assert_eq!(rooms, vec!["inbox-ann", "grp-1", "dm-1"]);
}

#[tokio::test]
async fn a_pending_dm_request_counts_and_unrelated_rooms_do_not() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    assert_eq!(profile_change_rooms(&conn, "bo").await.unwrap(), vec!["inbox-bo", "grp-1"]);
    assert_eq!(profile_change_rooms(&conn, "cy").await.unwrap(), vec!["inbox-cy", "grp-2", "dm-1"]);
}

#[test]
fn the_nudge_carries_the_user_and_nothing_about_the_change() {
    let event = profile_changed_event("ann");
    let obj = event.as_object().unwrap();
    assert_eq!(obj.len(), 2);
    assert_eq!(obj["type"], "profile_changed");
    assert_eq!(obj["user_id"], "ann");
}
