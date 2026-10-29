//! #1506 — pending-invite list and invite revoke; revoke authz is re-derived from the invite row.

use pollis_delivery::directory::{desired_roster, group_invites};
use pollis_delivery::groups::{apply_revoke_invite, RevokeInviteBody};
use pollis_delivery::writes::WriteOutcome;

mod common;

const GROUP: &str = "grp-1";
const OTHER_GROUP: &str = "grp-2";
const ADMIN: &str = "admin-1";
const INVITER: &str = "inviter-1";
const MEMBER: &str = "member-1";
const INVITEE: &str = "invitee-1";
const OTHER_ADMIN: &str = "admin-2";
const STRANGER: &str = "stranger-1";
const INVITE: &str = "inv-1";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    // The inviter, since demoted, may still withdraw their own invite.
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('admin-1','admin@x','admin'),('inviter-1','inviter@x','inviter'),\
           ('member-1','member@x','member'),('invitee-1','invitee@x','invitee'),\
           ('admin-2','admin2@x','admintwo'),('stranger-1','stranger@x','stranger');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group'),('grp-2','group');\
         INSERT INTO groups (id, name, owner_id) VALUES \
           ('grp-1','Group One','admin-1'),('grp-2','Group Two','admin-2');\
         INSERT INTO group_member (group_id, user_id, role) VALUES \
           ('grp-1','admin-1','admin'),('grp-1','inviter-1','member'),\
           ('grp-1','member-1','member'),('grp-2','admin-2','admin');\
         INSERT INTO group_invite (id, group_id, inviter_id, invitee_id, created_at) VALUES \
           ('inv-1','grp-1','inviter-1','invitee-1','2026-10-01 12:00:00');",
    )
    .await
    .expect("seed");
    db
}

fn revoke(actor: &str) -> RevokeInviteBody {
    RevokeInviteBody {
        invite_id: INVITE.to_string(),
        actor_id: Some(actor.to_string()),
    }
}

async fn invite_exists(db: &common::TempDb) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM group_invite WHERE id = ?1", [INVITE])
        .await
        .expect("count");
    let n: i64 = rows.next().await.expect("row").expect("row").get(0).expect("count");
    n > 0
}

#[tokio::test]
async fn the_list_names_invitee_inviter_and_when() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let invites = group_invites(&conn, GROUP).await.expect("list");
    assert_eq!(invites.len(), 1);
    let i = &invites[0];
    assert_eq!(i.id, INVITE);
    assert_eq!(i.invitee_id, INVITEE);
    assert_eq!(i.invitee_username.as_deref(), Some("invitee"));
    assert_eq!(i.inviter_id, INVITER);
    assert_eq!(i.inviter_username.as_deref(), Some("inviter"));
    assert_eq!(i.created_at, "2026-10-01 12:00:00");
    assert!(
        group_invites(&conn, OTHER_GROUP).await.expect("list").is_empty(),
        "another group's invites must not appear"
    );
}

#[tokio::test]
async fn the_inviter_may_revoke_and_the_invitee_leaves_the_desired_roster() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    assert!(
        desired_roster(&conn, GROUP).await.unwrap().iter().any(|u| u == INVITEE),
        "precondition: a pending invitee is in the desired roster"
    );

    let outcome = apply_revoke_invite(&conn, Some(INVITER), &revoke(INVITER))
        .await
        .expect("apply");
    assert!(matches!(outcome, WriteOutcome::Ok), "got {outcome:?}");
    assert!(!invite_exists(&db).await, "the invite row must be gone");
    assert!(
        !desired_roster(&conn, GROUP).await.unwrap().iter().any(|u| u == INVITEE),
        "a revoked invitee must leave the desired roster, or reconcile keeps their leaves"
    );
}

#[tokio::test]
async fn an_admin_may_revoke_someone_elses_invite() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let outcome = apply_revoke_invite(&conn, Some(ADMIN), &revoke(ADMIN))
        .await
        .expect("apply");
    assert!(matches!(outcome, WriteOutcome::Ok), "got {outcome:?}");
    assert!(!invite_exists(&db).await);
}

#[tokio::test]
async fn nobody_else_may_revoke() {
    for actor in [MEMBER, STRANGER, OTHER_ADMIN, INVITEE] {
        let db = fresh().await;
        let conn = db.conn().await.unwrap();
        let outcome = apply_revoke_invite(&conn, Some(actor), &revoke(actor))
            .await
            .expect("apply");
        assert!(
            matches!(outcome, WriteOutcome::Forbidden),
            "{actor} must not revoke, got {outcome:?}"
        );
        assert!(invite_exists(&db).await, "{actor}'s refused revoke must leave the row");
    }
}

#[tokio::test]
async fn an_inviter_who_left_the_group_may_not_revoke() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        [GROUP, INVITER],
    )
    .await
    .unwrap();
    let outcome = apply_revoke_invite(&conn, Some(INVITER), &revoke(INVITER))
        .await
        .expect("apply");
    assert!(matches!(outcome, WriteOutcome::Forbidden), "got {outcome:?}");
    assert!(invite_exists(&db).await);
}

#[tokio::test]
async fn a_signer_cannot_revoke_as_someone_else() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let outcome = apply_revoke_invite(&conn, Some(MEMBER), &revoke(ADMIN))
        .await
        .expect("apply");
    assert!(matches!(outcome, WriteOutcome::Forbidden), "got {outcome:?}");
    assert!(invite_exists(&db).await);
}

#[tokio::test]
async fn revoking_a_vanished_invite_is_refused() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let body = RevokeInviteBody {
        invite_id: "inv-missing".to_string(),
        actor_id: Some(ADMIN.to_string()),
    };
    let outcome = apply_revoke_invite(&conn, Some(ADMIN), &body).await.expect("apply");
    assert!(matches!(outcome, WriteOutcome::Forbidden), "got {outcome:?}");
}
