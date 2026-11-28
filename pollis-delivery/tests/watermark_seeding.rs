//! Where a device's fetch cursor may START.
//!
//! A watermark row is the device's cursor (`seq > last_seq`); a missing row
//! reads as 0, "fetch everything". A row seeded at the counter means "nothing
//! before now is yours", which is true at exactly two moments: a device's first
//! cert publish (a new device starts empty) and a user joining a conversation
//! (mail before joining). At any other time it would put the cursor past mail
//! the device never fetched, a silent loss. These tests pin both halves:
//! nothing re-seeds an existing device, and a new channel gives every member
//! device a row at its start, so retention stays pinned and bounded (#720).

use pollis_delivery::bootstrap::seed_conversation_watermarks;
use pollis_delivery::groups::{apply_accept_invite, apply_create_channel, AcceptInviteBody, CreateChannelBody};
use pollis_delivery::messages::{sweep_envelope_gc, DEFAULT_WATERMARK_STALE_MODIFIER};
use pollis_delivery::writes::WriteOutcome;

mod common;
#[path = "support/seq_invariants.rs"]
mod seq_invariants;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("seeding.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    // alice (a1) and bob (b1) are certified devices in group g; bob also has
    // b2, registered but never published; carol is invited, not a member.
    conn.execute_batch(
        "INSERT INTO conversation (id, kind) VALUES ('g', 'group');
         INSERT INTO groups (id, name, owner_id) VALUES ('g', 'G', 'alice');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g', 'bob', 'member');
         INSERT INTO user_device (user_id, device_id, mls_signature_pub) VALUES ('alice', 'a1', x'01');
         INSERT INTO user_device (user_id, device_id, mls_signature_pub) VALUES ('bob', 'b1', x'02');
         INSERT INTO user_device (user_id, device_id) VALUES ('bob', 'b2');
         INSERT INTO user_device (user_id, device_id, mls_signature_pub, revoked_at) VALUES ('bob', 'b3', x'03', datetime('now'));
         INSERT INTO user_device (user_id, device_id, mls_signature_pub) VALUES ('carol', 'k1', x'04');
         INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv', 'g', 'alice', 'carol');",
    )
    .await
    .unwrap();
    drop(conn);
    db
}

async fn create_channel(db: &common::TempDb, id: &str) {
    let out = apply_create_channel(
        &db.conn().await.unwrap(),
        Some("alice"),
        &CreateChannelBody {
            id: id.into(),
            group_id: "g".into(),
            name: id.into(),
            description: None,
            channel_type: "text".into(),
            creator_id: None,
            category_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
}

/// `(device, last_seq)` of every watermark row in `conversation`.
async fn cursors(db: &common::TempDb, conversation: &str) -> Vec<(String, i64)> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT device_id, COALESCE(last_seq, -1) FROM conversation_watermark \
             WHERE conversation_id = ?1 ORDER BY device_id",
            libsql::params![conversation.to_string()],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push((row.get(0).unwrap(), row.get(1).unwrap()));
    }
    out
}

async fn report(db: &common::TempDb, conversation: &str, device: &str, last_seq: i64, when: &str) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "UPDATE conversation_watermark SET last_seq = ?3, reported_at = datetime('now', ?4) \
             WHERE conversation_id = ?1 AND device_id = ?2",
            libsql::params![conversation.to_string(), device.to_string(), last_seq, when.to_string()],
        )
        .await
        .unwrap();
}

async fn sweep(db: &common::TempDb) {
    sweep_envelope_gc(&db.conn().await.unwrap(), DEFAULT_WATERMARK_STALE_MODIFIER)
        .await
        .unwrap();
}

/// A new channel gives every live, certified member device a row at its start
/// (0: everything), in its creation; a never-published or revoked device none.
#[tokio::test]
async fn a_new_channel_starts_every_member_device_at_its_beginning() {
    let db = fresh().await;
    create_channel(&db, "c").await;
    assert_eq!(cursors(&db, "c").await, [("a1".into(), 0), ("b1".into(), 0)]);
}

/// The loss this guards against: a channel is created and written to while a
/// member device is locked, and its unlock publishes its cert again. The
/// re-publish must not move its cursor, and its catch-up returns every message.
#[tokio::test]
async fn republishing_a_cert_never_moves_a_cursor_past_unfetched_mail() {
    let db = fresh().await;
    create_channel(&db, "c").await;
    for i in 1..=5 {
        seq_invariants::send_one(&db, &format!("m{i}"), "c").await;
    }
    let conn = db.conn().await.unwrap();
    seed_conversation_watermarks(&conn, "bob", "b1").await.unwrap();
    drop(conn);
    assert_eq!(cursors(&db, "c").await, [("a1".into(), 0), ("b1".into(), 0)]);

    // A channel that predates seeding at creation, with no row for b1 at all:
    // the re-publish leaves it without one (fetch everything), never at the head.
    db.conn()
        .await
        .unwrap()
        .execute("DELETE FROM conversation_watermark WHERE conversation_id = 'c' AND device_id = 'b1'", ())
        .await
        .unwrap();
    seed_conversation_watermarks(&db.conn().await.unwrap(), "bob", "b1").await.unwrap();
    assert_eq!(cursors(&db, "c").await, [("a1".into(), 0)]);
}

/// A device's FIRST publish seeds it at the head: a new device starts empty
/// (the second accepted loss), and pins retention from now on.
#[tokio::test]
async fn a_first_publish_starts_a_new_device_at_the_head() {
    let db = fresh().await;
    create_channel(&db, "c").await;
    for i in 1..=3 {
        seq_invariants::send_one(&db, &format!("m{i}"), "c").await;
    }
    // b2 has never published (`mls_signature_pub` NULL).
    seed_conversation_watermarks(&db.conn().await.unwrap(), "bob", "b2").await.unwrap();
    assert!(cursors(&db, "c").await.contains(&("b2".into(), 3)));
}

/// Retention over a new channel: nothing is collected until every member
/// device has fetched past it (the rows seeded at 0 pin it), then everything
/// is; and a device silent past the #720 window stops pinning, exactly as a
/// device whose row came from any other seed.
#[tokio::test]
async fn a_new_channel_pins_retention_until_every_device_has_fetched() {
    let db = fresh().await;
    // Only enrolled devices here: a registered-but-never-published one has no
    // row anywhere and holds every conversation's retention as it always has.
    db.conn()
        .await
        .unwrap()
        .execute("DELETE FROM user_device WHERE device_id = 'b2'", ())
        .await
        .unwrap();
    create_channel(&db, "c").await;
    for i in 1..=3 {
        seq_invariants::send_one(&db, &format!("m{i}"), "c").await;
    }
    report(&db, "c", "a1", 3, "+0 seconds").await;
    sweep(&db).await;
    assert_eq!(seq_invariants::seqs(&db, "c").await, [1, 2, 3], "b1 has fetched nothing yet");

    report(&db, "c", "b1", 2, "+0 seconds").await;
    sweep(&db).await;
    assert_eq!(seq_invariants::seqs(&db, "c").await, [3], "both past 2");

    // b1 goes silent past the window: it stops pinning (the third accepted loss).
    report(&db, "c", "b1", 2, "-13 months").await;
    sweep(&db).await;
    assert!(seq_invariants::seqs(&db, "c").await.is_empty());
}

/// Joining seeds the joiner's devices with the membership, in its
/// transaction: a join whose seed fails does not commit at all, rather than
/// leaving a member with no rows for a later seed to place past their mail.
#[tokio::test]
async fn a_join_whose_seed_fails_does_not_commit() {
    let db = fresh().await;
    create_channel(&db, "c").await;
    let conn = db.conn().await.unwrap();
    conn.execute_batch(
        "CREATE TRIGGER refuse_seed BEFORE INSERT ON conversation_watermark \
         WHEN NEW.user_id = 'carol' BEGIN SELECT RAISE(ABORT, 'refused'); END;",
    )
    .await
    .unwrap();
    let joined = apply_accept_invite(
        &conn,
        Some("carol"),
        &AcceptInviteBody { invite_id: "inv".into(), user_id: None },
    )
    .await;
    assert!(joined.is_err(), "{joined:?}");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM group_member WHERE user_id = 'carol'", ())
        .await
        .unwrap();
    let members: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(members, 0, "no membership without its seed");
    drop(rows);

    conn.execute_batch("DROP TRIGGER refuse_seed;").await.unwrap();
    let joined = apply_accept_invite(
        &conn,
        Some("carol"),
        &AcceptInviteBody { invite_id: "inv".into(), user_id: None },
    )
    .await
    .unwrap();
    assert!(matches!(joined, WriteOutcome::Ok), "{joined:?}");
    drop(conn);
    assert!(cursors(&db, "c").await.contains(&("k1".into(), 0)));
}
