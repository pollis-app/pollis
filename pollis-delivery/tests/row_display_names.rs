//! #2085: a person's display name travels with every row that lists them (group roster, DM
//! members, pending invites), so no client has to look it up separately.

use pollis_delivery::directory::{dm_members, group_invites, group_members};

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username, preferred_name) VALUES ('ann','ann@x','ann','Ann Admin');\
         INSERT INTO users (id, email, username) VALUES ('bo','bo@x','bo');\
         INSERT INTO users (id, email, username, preferred_name) VALUES ('cy','cy@x','cy','Cy Invitee');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','One','ann');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','ann','admin');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','bo','member');\
         INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv-1','grp-1','ann','cy');\
         INSERT INTO conversation (id, kind) VALUES ('dm-1','dm');\
         INSERT INTO dm_channel (id, created_by) VALUES ('dm-1','ann');\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-1','ann','ann');\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-1','bo','ann');",
    )
    .await
    .expect("seed");
    db
}

#[tokio::test]
async fn the_group_roster_carries_each_members_display_name() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let mut members = group_members(&conn, "grp-1").await.unwrap();
    members.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    assert_eq!(members[0].preferred_name.as_deref(), Some("Ann Admin"));
    assert_eq!(members[1].preferred_name, None, "no display name set: none, the handle stands");
    assert_eq!(members[1].username.as_deref(), Some("bo"));
}

#[tokio::test]
async fn dm_members_and_invites_carry_display_names() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let dm = dm_members(&conn, "dm-1").await.unwrap();
    let ann = dm.iter().find(|m| m.user_id == "ann").unwrap();
    assert_eq!(ann.preferred_name.as_deref(), Some("Ann Admin"));
    let invites = group_invites(&conn, "grp-1").await.unwrap();
    assert_eq!(invites[0].invitee_preferred_name.as_deref(), Some("Cy Invitee"));
}

#[test]
fn rows_from_a_ds_without_the_field_still_parse() {
    // Old DS → new client: the field defaults to none.
    let m: pollis_api::directory::GroupMemberWire =
        serde_json::from_str(r#"{"user_id":"u","role":"member","joined_at":"t"}"#).unwrap();
    assert_eq!(m.preferred_name, None);
    let d: pollis_api::directory::DmMemberWire = serde_json::from_str(r#"{"user_id":"u"}"#).unwrap();
    assert_eq!(d.preferred_name, None);
}
