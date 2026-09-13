//! The commit log is the CEILING on every control-plane write that names a
//! `(generation, epoch)`.
//!
//! `mls_group_info` and `pin_keystate` are one mutable row per conversation,
//! guarded by a lexicographic `(generation, epoch)` compare-and-set. Monotone —
//! but, until these tests, unbounded above. Any current member could publish
//! `generation = 2^62` and freeze the row for the life of the conversation:
//! every later GroupInfo republish is refused, so an external-joining device
//! reads a tree for a lineage that does not exist, and every later pin re-wrap
//! is refused, so a member removed after that point keeps a KEK that still opens
//! the stored `Kpin`. One request, permanent.
//!
//! `mls_welcome` had the mirror-image hole: the row is keyed on
//! `(conversation_id, recipient_id, recipient_device_id)` and
//! `/v1/welcomes/resubmit` was gated only on the SUBMITTER being some member, so
//! a member could address a Welcome to a non-member, name a lineage that was
//! never opened, or overwrite the pending Welcome the real adder had just
//! written with a blob of its own.
//!
//! Every test below fails against the pre-fix code.

use pollis_delivery::pins::{apply_upsert_keystate, KeystateOutcome, UpsertPinKeystateBody};
use pollis_delivery::writes::{
    apply_group_info, apply_welcomes_resubmit, GroupInfoBody, HeadBoundedOutcome, ResubmitBody,
    ResubmitOutcome, GROUP_INFO_MAX_BYTES,
};
use base64::Engine as _;

mod common;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Group `g1` in channel-free form (the conversation id IS the group id, the
/// shape a group's shared MLS conversation takes): alice is an admin, bob and
/// carol are plain members, mallory is not a member at all.
async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("head-bounds.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');
         INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'carol');",
    )
    .await
    .unwrap();
    db
}

/// Append commits `0..head_epoch` of `generation` to `conv`, authored by
/// `sender`. The lineage's head is then `head_epoch`.
async fn seed_lineage(
    db: &common::TempDb,
    conv: &str,
    generation: i64,
    head_epoch: i64,
    sender: &str,
) {
    let conn = db.conn().await.unwrap();
    for epoch in 0..head_epoch {
        conn.execute(
            "INSERT OR IGNORE INTO mls_commit_log \
                 (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, ?4, X'00')",
            libsql::params![conv, generation, epoch, sender],
        )
        .await
        .unwrap();
    }
}

fn group_info(generation: i64, epoch: i64, blob: &[u8]) -> GroupInfoBody {
    GroupInfoBody {
        conversation_id: "g1".to_string(),
        generation,
        epoch,
        group_info: b64(blob),
        updated_by_device_id: "dev-a".to_string(),
    }
}

fn keystate(generation: i64, epoch: i64, actor: &str, mint: bool) -> UpsertPinKeystateBody {
    UpsertPinKeystateBody {
        conversation_id: "g1".to_string(),
        wrapped_kpin: b64(&[0xAB; 48]),
        nonce: b64(&[7u8; 12]),
        generation,
        epoch,
        device_id: "dev-a".to_string(),
        actor_id: Some(actor.to_string()),
        mint,
    }
}

fn resubmit(generation: i64, recipient: &str, device: &str, blob: &[u8]) -> ResubmitBody {
    ResubmitBody {
        conversation_id: "g1".to_string(),
        generation,
        recipient_id: recipient.to_string(),
        recipient_device_id: device.to_string(),
        welcome: b64(blob),
    }
}

/// The stored GroupInfo's `(generation, epoch)`, or `None` when no row exists.
async fn stored_group_info(db: &common::TempDb) -> Option<(i64, i64, Vec<u8>)> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT generation, epoch, group_info FROM mls_group_info WHERE conversation_id = 'g1'",
            (),
        )
        .await
        .unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| (r.get(0).unwrap(), r.get(1).unwrap(), r.get(2).unwrap()))
}

// ── GroupInfo ────────────────────────────────────────────────────────────────

/// The head IS publishable: a member that merged the head commit republishes the
/// resulting epoch, and that must keep working.
#[tokio::test]
async fn group_info_at_the_head_is_accepted() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 3, "alice").await;
    let conn = db.conn().await.unwrap();

    let out = apply_group_info(&conn, &group_info(0, 3, b"tree-at-head"))
        .await
        .unwrap();
    assert!(matches!(out, HeadBoundedOutcome::Ok { affected: 1 }));
    assert_eq!(stored_group_info(&db).await.unwrap().1, 3);
}

/// A brand-new conversation has no commits at all, and its group is at epoch 0.
/// The ceiling must not lock that out.
#[tokio::test]
async fn group_info_for_a_fresh_group_at_epoch_zero_is_accepted() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let out = apply_group_info(&conn, &group_info(0, 0, b"fresh-tree"))
        .await
        .unwrap();
    assert!(matches!(out, HeadBoundedOutcome::Ok { affected: 1 }));
}

/// THE FREEZE. A member publishes an astronomically high generation; nothing can
/// ever exceed it, so the row is dead. Refused, and the real GroupInfo stands.
#[tokio::test]
async fn a_generation_far_above_the_head_cannot_freeze_the_group_info() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 3, "alice").await;
    let conn = db.conn().await.unwrap();

    apply_group_info(&conn, &group_info(0, 3, b"real-tree"))
        .await
        .unwrap();

    let out = apply_group_info(&conn, &group_info(i64::MAX / 2, 0, b"freeze"))
        .await
        .unwrap();
    assert!(
        matches!(
            out,
            HeadBoundedOutcome::AheadOfHead {
                head_generation: 0,
                head_epoch: 3
            }
        ),
        "a lineage the commit log never opened must be refused, with the head reported"
    );

    let (generation, epoch, blob) = stored_group_info(&db).await.unwrap();
    assert_eq!((generation, epoch), (0, 3), "the real GroupInfo must stand");
    assert_eq!(blob, b"real-tree");

    // And the row is still WRITABLE: the next honest commit's GroupInfo lands.
    seed_lineage(&db, "g1", 0, 4, "alice").await;
    let out = apply_group_info(&conn, &group_info(0, 4, b"next-tree"))
        .await
        .unwrap();
    assert!(
        matches!(out, HeadBoundedOutcome::Ok { affected: 1 }),
        "the conversation must not be wedged"
    );
}

/// One epoch past the head is still past the head — the log decides what epochs
/// exist, not the publisher.
#[tokio::test]
async fn one_epoch_above_the_head_is_refused() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 3, "alice").await;
    let conn = db.conn().await.unwrap();

    let out = apply_group_info(&conn, &group_info(0, 4, b"future"))
        .await
        .unwrap();
    assert!(matches!(
        out,
        HeadBoundedOutcome::AheadOfHead {
            head_generation: 0,
            head_epoch: 3
        }
    ));
    assert!(stored_group_info(&db).await.is_none());
}

/// The blob is bounded: one mutable slot per conversation is not somewhere to
/// park megabytes.
#[tokio::test]
async fn an_oversized_group_info_is_refused() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    let huge = vec![0x5Au8; GROUP_INFO_MAX_BYTES + 1];
    let out = apply_group_info(&conn, &group_info(0, 1, &huge)).await.unwrap();
    assert!(matches!(out, HeadBoundedOutcome::Invalid(_)));
    assert!(stored_group_info(&db).await.is_none());

    // Exactly at the ceiling is fine — the bound rejects "much larger than a
    // ratchet tree", not "large".
    let at_cap = vec![0x5Au8; GROUP_INFO_MAX_BYTES];
    let out = apply_group_info(&conn, &group_info(0, 1, &at_cap)).await.unwrap();
    assert!(matches!(out, HeadBoundedOutcome::Ok { affected: 1 }));
}

// ── Pin keystate ─────────────────────────────────────────────────────────────

/// The same freeze, against the wrapped `Kpin`. A frozen keystate is worse than
/// a frozen GroupInfo: every re-wrap after a removal is refused, so the removed
/// member's KEK keeps opening the stored key.
#[tokio::test]
async fn a_generation_far_above_the_head_cannot_freeze_the_pin_keystate() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 2, "alice").await;
    let conn = db.conn().await.unwrap();

    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate(0, 2, "alice", true))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: true }));

    let out = apply_upsert_keystate(
        &conn,
        &conn,
        Some("bob"),
        &keystate(i64::MAX / 2, 0, "bob", false),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            out,
            KeystateOutcome::AheadOfHead {
                head_generation: 0,
                head_epoch: 2
            }
        ),
        "a wrap must name an exporter that exists"
    );

    // The next honest re-wrap still lands, which is the property the freeze
    // destroyed.
    seed_lineage(&db, "g1", 0, 3, "alice").await;
    let out = apply_upsert_keystate(&conn, &conn, Some("alice"), &keystate(0, 3, "alice", false))
        .await
        .unwrap();
    assert!(matches!(out, KeystateOutcome::Ok { updated: true }));
}

// ── Welcome resubmit ─────────────────────────────────────────────────────────

/// A Welcome may only be addressed to a CURRENT member. Parking one for an
/// outsider hands that device an invitation nobody issued.
#[tokio::test]
async fn a_welcome_addressed_to_a_non_member_is_refused() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    let out = apply_welcomes_resubmit(
        &conn,
        &conn,
        Some("alice"),
        &resubmit(0, "mallory", "m1", b"welcome"),
    )
    .await
    .unwrap();
    assert!(matches!(out, ResubmitOutcome::Forbidden));

    let mut rows = conn
        .query("SELECT COUNT(*) FROM mls_welcome", ())
        .await
        .unwrap();
    let count: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(count, 0, "no row may have been written");
}

/// A Welcome into a lineage the commit log never opened admits nobody.
#[tokio::test]
async fn a_welcome_into_an_unopened_lineage_is_refused() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    let out =
        apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(9, "bob", "b1", b"w"))
            .await
            .unwrap();
    assert!(matches!(
        out,
        ResubmitOutcome::AheadOfHead {
            head_generation: 0,
            ..
        }
    ));
}

/// THE HIJACK. Alice performed the Add and wrote bob's Welcome; carol — a plain
/// member — must not be able to replace it while it is still pending.
#[tokio::test]
async fn a_member_cannot_overwrite_another_members_pending_welcome() {
    let db = fresh().await;
    // carol authors the head commit here deliberately NOT: alice does, so carol
    // is neither the adder nor an admin.
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    let out =
        apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(0, "bob", "b1", b"honest"))
            .await
            .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok));

    let out =
        apply_welcomes_resubmit(&conn, &conn, Some("carol"), &resubmit(0, "bob", "b1", b"hijack"))
            .await
            .unwrap();
    assert!(
        matches!(out, ResubmitOutcome::Forbidden),
        "a plain member must not replace the adder's pending Welcome"
    );

    let mut rows = conn
        .query(
            "SELECT welcome_data, submitted_by FROM mls_welcome WHERE recipient_id = 'bob'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<Vec<u8>>(0).unwrap(), b"honest");
    assert_eq!(row.get::<String>(1).unwrap(), "alice");
}

/// The publisher may always refresh its OWN Welcome — that is the recovery this
/// endpoint exists for.
#[tokio::test]
async fn the_publisher_may_refresh_its_own_welcome() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    apply_welcomes_resubmit(&conn, &conn, Some("carol"), &resubmit(0, "bob", "b1", b"v1"))
        .await
        .unwrap();
    let out = apply_welcomes_resubmit(&conn, &conn, Some("carol"), &resubmit(0, "bob", "b1", b"v2"))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok));

    let mut rows = conn
        .query(
            "SELECT welcome_data FROM mls_welcome WHERE recipient_id = 'bob'",
            (),
        )
        .await
        .unwrap();
    let data: Vec<u8> = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(data, b"v2");
}

/// The head commit's author and a group admin may both re-drive somebody else's
/// pending Welcome — they are the two parties with a legitimate reason to.
#[tokio::test]
async fn the_adder_and_an_admin_may_re_drive_a_pending_welcome() {
    let db = fresh().await;
    // carol authored the head commit, so carol IS the adder; alice is the admin.
    seed_lineage(&db, "g1", 0, 1, "carol").await;
    let conn = db.conn().await.unwrap();

    apply_welcomes_resubmit(&conn, &conn, Some("bob"), &resubmit(0, "bob", "b1", b"v1"))
        .await
        .unwrap();

    let out = apply_welcomes_resubmit(&conn, &conn, Some("carol"), &resubmit(0, "bob", "b1", b"v2"))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok), "the adder may re-drive");

    let out = apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(0, "bob", "b1", b"v3"))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok), "an admin may re-drive");
}

/// A DELIVERED Welcome is spent, so refreshing it is not a hijack — the
/// recipient already consumed the blob it is being asked to replace.
#[tokio::test]
async fn a_delivered_welcome_may_be_refreshed_by_anyone() {
    let db = fresh().await;
    seed_lineage(&db, "g1", 0, 1, "alice").await;
    let conn = db.conn().await.unwrap();

    apply_welcomes_resubmit(&conn, &conn, Some("alice"), &resubmit(0, "bob", "b1", b"v1"))
        .await
        .unwrap();
    conn.execute("UPDATE mls_welcome SET delivered = 1", ())
        .await
        .unwrap();

    let out = apply_welcomes_resubmit(&conn, &conn, Some("carol"), &resubmit(0, "bob", "b1", b"v2"))
        .await
        .unwrap();
    assert!(matches!(out, ResubmitOutcome::Ok));
}
