//! #1095: a display name must not be able to carry notification markup.
//!
//! Group names and `preferred_name` are interpolated into OS notification
//! bodies. On Linux a body is not plain text — the freedesktop spec allows a
//! small HTML subset and `notify-rust` passes it verbatim, so a daemon that
//! renders body markup acts on it. A group called
//! `<img src="http://attacker/p.png">` therefore turned every message receipt
//! into a request from the victim's machine.
//!
//! The renderer escapes every notification title and body at the bridge
//! (`frontend/src/utils/notificationText.ts`, pinned by
//! `frontend/tests/notificationText.test.ts`) — that is the fix. These tests pin
//! the other half: the DS refuses to *store* such a name in the first place, so
//! the escaping is defence in depth rather than the only thing standing there.
//!
//! Driven at the `apply_*` level against a local libsql DB, mirroring
//! `username_shape.rs`.

use pollis_delivery::groups::{apply_create_group, apply_update_group};
use pollis_delivery::profile::{apply_update_profile, ProfileOutcome};
use pollis_delivery::writes::WriteOutcome;
use pollis_api::groups::{CreateGroupBody, UpdateGroupBody};
use pollis_api::profile::UpdateProfileBody;

mod common;

const ADMIN: &str = "admin-1";
/// The finding's concrete payload.
const MARKUP: &str = "<img src=\"http://attacker.test/p.png\">";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('admin-1','admin@x','admin');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','Group One','admin-1');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','admin-1','admin');",
    )
    .await
    .expect("seed");
    db
}

fn create(name: &str) -> CreateGroupBody {
    CreateGroupBody {
        id: "grp-new".to_string(),
        name: name.to_string(),
        description: None,
        owner_id: Some(ADMIN.to_string()),
        default_text_channel_id: None,
        default_voice_channel_id: None,
        created_at: "2026-01-01T00:00:00.000000000+00:00".to_string(),
    }
}

#[tokio::test]
async fn a_group_cannot_be_created_with_notification_markup_in_its_name() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let out = apply_create_group(&conn, Some(ADMIN), &create(MARKUP)).await.unwrap();
    assert!(
        matches!(out, WriteOutcome::Forbidden),
        "a group name carrying markup must be refused"
    );
    // And nothing was stored under it.
    let mut rows = conn
        .query("SELECT COUNT(*) FROM groups WHERE id = 'grp-new'", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0, "the refused group must not exist");
}

#[tokio::test]
async fn a_group_cannot_be_renamed_into_notification_markup() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let out = apply_update_group(
        &conn,
        Some(ADMIN),
        &UpdateGroupBody {
            group_id: "grp-1".to_string(),
            requester_id: Some(ADMIN.to_string()),
            name: Some(MARKUP.to_string()),
            description: None,
            icon_url: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(out, WriteOutcome::Forbidden),
        "renaming into markup must be refused — otherwise the create-side rule is a formality"
    );
    let mut rows = conn
        .query("SELECT name FROM groups WHERE id = 'grp-1'", ())
        .await
        .unwrap();
    let name: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(name, "Group One", "the old name must survive a refused rename");
}

#[tokio::test]
async fn a_preferred_name_cannot_carry_notification_markup() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let out = apply_update_profile(
        &conn,
        Some(ADMIN),
        &UpdateProfileBody {
            user_id: ADMIN.to_string(),
            username: None,
            preferred_name: Some(MARKUP.to_string()),
            phone: None,
            avatar_url: None,
        },
    )
    .await
    .unwrap();
    assert!(
        matches!(out, ProfileOutcome::Invalid(_)),
        "a preferred_name carrying markup must be refused"
    );
}

/// The rule must not have become "ASCII only" — a display name is rendered, not
/// resolved, and rejecting real names would be a bug rather than hardening.
#[tokio::test]
async fn ordinary_names_still_go_through() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for ok in ["Ops & Infra", "Café déjà vu", "研究チーム"] {
        let out = apply_update_profile(
            &conn,
            Some(ADMIN),
            &UpdateProfileBody {
                user_id: ADMIN.to_string(),
                username: None,
                preferred_name: Some(ok.to_string()),
                phone: None,
                avatar_url: None,
            },
        )
        .await
        .unwrap();
        assert!(matches!(out, ProfileOutcome::Ok), "{ok:?} must be accepted");
    }
}
