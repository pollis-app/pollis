//! Envelope-GC retention, driven through the real `pollis_delivery` handlers.

use pollis_delivery::account::{apply_revoke_device, RevokeDeviceBody};
use pollis_delivery::db::Db;
use pollis_delivery::messages::{
    apply_advance_watermark, apply_delete_message, apply_envelope_gc, sweep_envelope_gc,
    DeleteMessageBody, EnvelopeGcBody, WatermarkBody,
};
use pollis_delivery::profile::{apply_add_dm_member, apply_create_dm, AddDmMemberBody, CreateDmBody};
use pollis_delivery::groups::{apply_approve_join_request, ApproveJoinRequestBody, ReviewOutcome};
use pollis_delivery::writes::WriteOutcome;

mod common;


/// The DS's default staleness window (#1486); wide enough that NULL `reported_at` fixtures stay live.
const STALE: &str = pollis_delivery::messages::DEFAULT_WATERMARK_STALE_MODIFIER;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

async fn add_device(db: &Db, user: &str, device: &str, revoked: bool) {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![user.to_string(), device.to_string(), revoked as i64],
        )
        .await
        .unwrap();
}

/// `last_fetched_at` is required by the schema but unasserted (#1087).
async fn seed_watermark(db: &Db, conv: &str, user: &str, device: &str, seq: i64) {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq) \
             VALUES (?1, ?2, ?3, datetime('now'), ?4)",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), seq],
        )
        .await
        .unwrap();
}

async fn seed_watermark_reported(
    db: &Db,
    conv: &str,
    user: &str,
    device: &str,
    seq: i64,
    reported: &str,
) {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
             VALUES (?1, ?2, ?3, datetime('now'), ?4, datetime('now', ?5))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                seq,
                reported.to_string()
            ],
        )
        .await
        .unwrap();
}

async fn add_envelope(db: &Db, id: &str, conv: &str, seq: i64) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sent_at, sender_id, ciphertext, seq) \
         VALUES (?1, ?2, datetime('now'), 'sender', 'ct', ?3)",
        libsql::params![id.to_string(), conv.to_string(), seq],
    )
    .await
    .unwrap();
    // Advance `conversation_seq` as `insert_envelope_with_seq` does, or the next write reuses `seq`.
    conn.execute(
        "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, ?2) \
         ON CONFLICT(conversation_id) DO UPDATE SET next_seq = MAX(next_seq, excluded.next_seq)",
        libsql::params![conv.to_string(), seq],
    )
    .await
    .unwrap();
}

async fn envelope_count(db: &Db, conv: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
            libsql::params![conv.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn watermark_devices(db: &Db, conv: &str) -> Vec<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT device_id FROM conversation_watermark WHERE conversation_id = ?1 ORDER BY device_id ASC",
            libsql::params![conv.to_string()],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<String>(0).unwrap());
    }
    out
}

#[tokio::test]
async fn channel_gc_ignores_revoked_device_with_no_watermark() {
    let db = fresh().await;
    db.conn().await
        .unwrap()
        .execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');",
        )
        .await
        .unwrap();
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-old", true).await;
    seed_watermark(&db, "c1", "alice", "a-live", 10).await;
    add_envelope(&db, "e1", "c1", 7).await;

    let body = EnvelopeGcBody {
        conversation_id: "c1".into(),
        is_dm: false,
        actor_id: Some("alice".into()),
    };
    let out = apply_envelope_gc(&db.conn().await.unwrap(), None, &body, STALE).await.unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "channel GC must prune once every LIVE device has read past the envelope; \
         a revoked device's absent watermark must not disable pruning (#685)"
    );
}

#[tokio::test]
async fn dm_gc_ignores_stale_revoked_watermark() {
    let db = fresh().await;
    db.conn().await
        .unwrap()
        .execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) \
                  VALUES ('d1', 'alice', 'alice', datetime('now'))", ())
        .await
        .unwrap();
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-old", true).await;
    seed_watermark(&db, "d1", "alice", "a-live", 10).await;
    seed_watermark(&db, "d1", "alice", "a-old", 4).await;
    add_envelope(&db, "e1", "d1", 7).await;

    let body = EnvelopeGcBody {
        conversation_id: "d1".into(),
        is_dm: true,
        actor_id: Some("alice".into()),
    };
    apply_envelope_gc(&db.conn().await.unwrap(), None, &body, STALE).await.unwrap();

    assert_eq!(
        envelope_count(&db, "d1").await,
        0,
        "DM GC must prune below the LIVE device's cursor; a revoked device's stale \
         watermark must not pin MIN(cw) (#685)"
    );
}

#[tokio::test]
async fn create_dm_skips_revoked_member_devices() {
    let db = fresh().await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b-live", false).await;
    add_device(&db, "bob", "b-rev", true).await;

    let body = CreateDmBody {
        id: "d1".into(),
        creator_id: "alice".into(),
        member_ids: vec!["bob".into()],
        created_at: "2026-01-01T00:00:00+00:00".into(),
    };
    apply_create_dm(&db.conn().await.unwrap(), None, &body).await.unwrap();

    assert_eq!(
        watermark_devices(&db, "d1").await,
        vec!["a1".to_string(), "b-live".to_string()],
        "DM create must seed watermarks for live devices only — never the revoked \
         'b-rev' (#685)"
    );
}

#[tokio::test]
async fn add_dm_member_skips_revoked_devices() {
    let db = fresh().await;
    db.conn().await
        .unwrap()
        .execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at) \
                  VALUES ('d1', 'alice', 'alice', datetime('now'), datetime('now'))", ())
        .await
        .unwrap();
    add_device(&db, "bob", "b-live", false).await;
    add_device(&db, "bob", "b-rev", true).await;

    let body = AddDmMemberBody {
        dm_channel_id: "d1".into(),
        user_id: "bob".into(),
        added_by: "alice".into(),
        added_at: "2026-01-02T00:00:00+00:00".into(),
    };
    apply_add_dm_member(&db.conn().await.unwrap(), None, &body).await.unwrap();

    assert_eq!(
        watermark_devices(&db, "d1").await,
        vec!["b-live".to_string()],
        "DM member-add must seed a watermark for the live device only — never the \
         revoked 'b-rev' (#685)"
    );
}

#[tokio::test]
async fn approve_join_request_skips_revoked_devices() {
    let db = fresh().await;
    db.conn().await
        .unwrap()
        .execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');\
             INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin');\
             INSERT INTO group_join_request (id, group_id, requester_id, status) \
                 VALUES ('jr1', 'g1', 'bob', 'pending');",
        )
        .await
        .unwrap();
    add_device(&db, "bob", "b-live", false).await;
    add_device(&db, "bob", "b-rev", true).await;

    let body = ApproveJoinRequestBody {
        request_id: "jr1".into(),
        approver_id: Some("alice".into()),
        reviewed_at: "2026-01-03T00:00:00+00:00".into(),
    };
    let out = apply_approve_join_request(&db.conn().await.unwrap(), None, &body).await.unwrap();
    assert!(matches!(out, ReviewOutcome::Reviewed { .. }));

    assert_eq!(
        watermark_devices(&db, "c1").await,
        vec!["b-live".to_string()],
        "group member-add must seed a watermark for the live device only — never \
         the revoked 'b-rev' (#685)"
    );
}

/// The sibling's surviving row proves revocation is scoped to the one device.
#[tokio::test]
async fn revoke_device_deletes_that_devices_watermarks_only() {
    let db = fresh().await;
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-doomed", false).await;
    add_device(&db, "bob", "b-live", false).await;

    seed_watermark(&db, "c1", "alice", "a-doomed", 8).await;
    seed_watermark(&db, "d1", "alice", "a-doomed", 8).await;
    seed_watermark(&db, "c1", "alice", "a-live", 10).await;
    seed_watermark(&db, "c1", "bob", "b-live", 10).await;

    let body = RevokeDeviceBody {
        device_id: "a-doomed".into(),
        user_id: None,
    };
    let out = apply_revoke_device(&db.conn().await.unwrap(), Some("alice"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert_eq!(
        watermark_devices(&db, "c1").await,
        vec!["a-live".to_string(), "b-live".to_string()],
        "revoking a-doomed must delete ITS c1 cursor and leave every other \
         device's cursor intact (#685)"
    );
    assert_eq!(
        watermark_devices(&db, "d1").await,
        Vec::<String>::new(),
        "the revoked device's cursor must be gone from EVERY conversation, not \
         just the one the GC happened to run on (#685)"
    );
}

#[tokio::test]
async fn revoke_device_cannot_delete_another_users_watermarks() {
    let db = fresh().await;
    add_device(&db, "bob", "b-live", false).await;
    seed_watermark(&db, "c1", "bob", "b-live", 10).await;

    // The DELETE is bound to `user_id = actor`, so naming bob's device is inert.
    let body = RevokeDeviceBody {
        device_id: "b-live".into(),
        user_id: None,
    };
    apply_revoke_device(&db.conn().await.unwrap(), Some("alice"), &body)
        .await
        .unwrap();

    assert_eq!(
        watermark_devices(&db, "c1").await,
        vec!["b-live".to_string()],
        "alice revoking a device id that belongs to bob must not touch bob's \
         cursor — the DELETE is bound WHERE user_id = actor (#685)"
    );
}

async fn channel_with_members(db: &Db, members: &[&str]) {
    let conn = db.conn().await.unwrap();
    conn.execute_batch(
        "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
         INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');",
    )
    .await
    .unwrap();
    for m in members {
        conn.execute(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', ?1)",
            libsql::params![m.to_string()],
        )
        .await
        .unwrap();
    }
}

async fn gc_channel(db: &Db) {
    let body = EnvelopeGcBody {
        conversation_id: "c1".into(),
        is_dm: false,
        actor_id: Some("alice".into()),
    };
    let out = apply_envelope_gc(&db.conn().await.unwrap(), None, &body, STALE).await.unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

/// F3 regression: an envelope older than the removed 30-day TTL survives while bob's cursor is behind it.
#[tokio::test]
async fn ancient_envelope_survives_while_a_member_device_has_not_collected_it() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;
    seed_watermark(&db, "c1", "alice", "a1", 10).await;
    seed_watermark(&db, "c1", "bob", "b1", 1).await;
    add_envelope(&db, "ancient", "c1", 2).await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "a 400-day-old envelope below a current member device's cursor must \
         survive: retention is bounded by the slowest member device, never by \
         wall-clock age (I3, failure mode F3)"
    );
}

/// No watermark is no evidence of delivery.
#[tokio::test]
async fn ancient_envelope_survives_for_a_device_that_never_reported() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b-silent", false).await;
    // Only alice has ever reported.
    seed_watermark(&db, "c1", "alice", "a1", 10).await;
    add_envelope(&db, "ancient", "c1", 2).await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "a member device with no watermark row must hold even a 400-day-old \
         envelope (I3, failure mode F3)"
    );
}

/// GC is bounded, not disabled; both envelopes are younger than the old TTL.
#[tokio::test]
async fn envelope_is_deleted_once_every_member_device_collected_past_it() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "alice", "a2", false).await;
    add_device(&db, "bob", "b1", false).await;
    add_envelope(&db, "collected", "c1", 8).await;
    add_envelope(&db, "pending", "c1", 11).await;
    seed_watermark(&db, "c1", "alice", "a1", 9).await;
    seed_watermark(&db, "c1", "alice", "a2", 9).await;
    seed_watermark(&db, "c1", "bob", "b1", 9).await;

    gc_channel(&db).await;

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT id FROM message_envelope WHERE conversation_id = 'c1'", ())
        .await
        .unwrap();
    let mut survivors = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        survivors.push(r.get::<String>(0).unwrap());
    }
    assert_eq!(
        survivors,
        vec!["pending".to_string()],
        "the envelope every member device collected must be pruned and the one \
         still above the floor kept — removing the TTL must not disable GC"
    );
}

/// Without the `revoked_at IS NULL` filter the dead cursor would pin this forever.
#[tokio::test]
async fn a_revoked_device_does_not_pin_retention_forever() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b-revoked", true).await;
    seed_watermark(&db, "c1", "alice", "a1", 10).await;
    seed_watermark(&db, "c1", "bob", "b-revoked", 1).await;
    add_envelope(&db, "e1", "c1", 8).await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "a revoked device can never rejoin the tree (I5), so its dead cursor must \
         not hold the retention floor down forever (#685)"
    );
}

/// An empty roster yields `sent_at < NULL`, so nothing is deleted.
#[tokio::test]
async fn an_empty_member_device_set_deletes_nothing() {
    let db = fresh().await;
    channel_with_members(&db, &["alice"]).await;
    add_envelope(&db, "ancient", "c1", 2).await;
    add_envelope(&db, "fresh", "c1", 11).await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        2,
        "an empty member-device set must delete NOTHING, not everything"
    );
}

#[tokio::test]
async fn an_all_revoked_roster_deletes_nothing() {
    let db = fresh().await;
    channel_with_members(&db, &["alice"]).await;
    add_device(&db, "alice", "a-old", true).await;
    seed_watermark(&db, "c1", "alice", "a-old", 1).await;
    add_envelope(&db, "ancient", "c1", 2).await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "excluding revoked devices must never leave a roster that deletes by \
         virtue of being empty"
    );
}

/// RFC3339 as clients write it: `sent_at_after` falls back to `now` on any other shape.
fn rfc3339_from_now(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
}

async fn tombstone_seq(db: &Db, conv: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT seq FROM message_envelope \
             WHERE conversation_id = ?1 AND type = 'delete'",
            libsql::params![conv.to_string()],
        )
        .await
        .unwrap();
    let row = rows
        .next()
        .await
        .unwrap()
        .expect("an admin delete must write a tombstone");
    let out: i64 = row
        .get::<Option<i64>>(0)
        .unwrap()
        .expect("every envelope the DS writes carries a delivery sequence");
    assert!(
        rows.next().await.unwrap().is_none(),
        "expected exactly one tombstone"
    );
    out
}

async fn watermark_seqs(db: &Db, conv: &str) -> Vec<i64> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COALESCE(last_seq, 0) FROM conversation_watermark \
             WHERE conversation_id = ?1",
            libsql::params![conv.to_string()],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<i64>(0).unwrap());
    }
    out
}

/// #692/#1087: the tombstone takes `MAX(seq)+1`, so client clocks running ahead are irrelevant.
#[tokio::test]
async fn a_tombstone_outranks_surviving_cursors_after_gc_emptied_the_conversation() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    db.conn().await
        .unwrap()
        .execute("UPDATE group_member SET role = 'admin' WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;

    // `last_fetched_at` an hour in the future: the #692 clock shape.
    add_envelope(&db, "m1", "c1", 1).await;
    for (user, device) in [("alice", "a1"), ("bob", "b1")] {
        db.conn().await
            .unwrap()
            .execute(
                "INSERT INTO conversation_watermark \
                     (conversation_id, user_id, device_id, last_fetched_at, last_seq) \
                 VALUES ('c1', ?1, ?2, ?3, 1)",
                libsql::params![user.to_string(), device.to_string(), rfc3339_from_now(60)],
            )
            .await
            .unwrap();
    }

    gc_channel(&db).await;
    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "fixture precondition: watermark-gated GC must have emptied the conversation"
    );

    let out = apply_delete_message(
        &db.conn().await.unwrap(),
        Some("alice"),
        &DeleteMessageBody {
            message_id: "m1".into(),
            conversation_id: "c1".into(),
            msg_sender_id: Some("bob".into()),
            actor_id: None,
            delete_token: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "admin delete must be allowed");

    let tombstone_seq = tombstone_seq(&db, "c1").await;
    let cursors = watermark_seqs(&db, "c1").await;
    assert_eq!(cursors.len(), 2, "both watermark rows must have survived GC");
    for cursor in &cursors {
        assert!(
            tombstone_seq > *cursor,
            "tombstone seq {tombstone_seq} must be above the surviving cursor \
             {cursor}, or the device never fetches it and the deleted message \
             stays readable there (#692)"
        );
    }
}

/// The sweep alone (no ingest, no endpoint) collects caught-up channels and DMs (#689).
#[tokio::test]
async fn sweep_collects_quiet_conversations_and_spares_uncollected() {
    let db = fresh().await;
    db.conn().await
        .unwrap()
        .execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('gA', 'alice');\
             INSERT INTO conversation (id, kind) VALUES ('cA', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('cA', 'gA', 'chan');\
             INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) \
               VALUES ('dB', 'bob', 'bob', datetime('now'));\
             INSERT INTO group_member (group_id, user_id) VALUES ('gC', 'carol');\
             INSERT INTO conversation (id, kind) VALUES ('cC', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('cC', 'gC', 'chan');",
        )
        .await
        .unwrap();

    add_device(&db, "alice", "aA", false).await;
    seed_watermark(&db, "cA", "alice", "aA", 10).await;
    add_envelope(&db, "eA", "cA", 7).await;

    add_device(&db, "bob", "bB", false).await;
    seed_watermark(&db, "dB", "bob", "bB", 10).await;
    add_envelope(&db, "eB", "dB", 7).await;

    add_device(&db, "carol", "cC-dev", false).await;
    add_envelope(&db, "eC", "cC", 7).await;

    let visited = sweep_envelope_gc(&db.conn().await.unwrap(), STALE).await.unwrap().visited;

    assert_eq!(visited, 3, "the sweep visits every conversation that still has envelopes");
    assert_eq!(
        envelope_count(&db, "cA").await,
        0,
        "a quiet channel every device has caught up on must be swept, with no ingest"
    );
    assert_eq!(
        envelope_count(&db, "dB").await,
        0,
        "a quiet DM every device has caught up on must be swept (DM predicate)"
    );
    assert_eq!(
        envelope_count(&db, "cC").await,
        1,
        "a conversation with an uncollected recipient must NOT be swept"
    );
}

/// A dormant device that reports again is live immediately and re-pins.
#[tokio::test]
async fn a_returning_device_reports_and_pins_again() {
    {
        let db = fresh().await;
        db.conn().await.unwrap().execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');",
        ).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, "c1", "alice", "a1", 10, "-1 day").await;
        seed_watermark_reported(&db, "c1", "bob", "b1", 1, "-400 days").await;
        add_envelope(&db, "e1", "c1", 3).await;

        let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
        apply_envelope_gc(&db.conn().await.unwrap(), None, &body, STALE).await.unwrap();
        assert_eq!(envelope_count(&db, "c1").await, 0, "dormant → collected (control)");
    }
    {
        let db = fresh().await;
        db.conn().await.unwrap().execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');",
        ).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, "c1", "alice", "a1", 10, "-1 day").await;
        seed_watermark_reported(&db, "c1", "bob", "b1", 1, "-400 days").await;
        add_envelope(&db, "e1", "c1", 3).await;

        // RFC 3339: a SQLite-shaped stamp would be refused, making the assertion vacuous.
        let wm = WatermarkBody {
            conversation_id: "c1".into(),
            user_id: Some("bob".into()),
            device_id: "b1".into(),
            last_fetched_at: rfc3339_from_now(-200 * 24 * 60),
            last_seq: None,
        };
        apply_advance_watermark(&db.conn().await.unwrap(), None, &wm).await.unwrap();

        let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
        apply_envelope_gc(&db.conn().await.unwrap(), None, &body, STALE).await.unwrap();

        assert_eq!(
            envelope_count(&db, "c1").await,
            1,
            "reporting a watermark makes a stale device live again immediately, so \
             it re-pins the envelope below its cursor (#720 reversibility)"
        );
    }
}

/// The sweep and the endpoint share one liveness predicate, so both collect (#720).
#[tokio::test]
async fn sweep_and_endpoint_agree_on_the_staleness_bound() {
    async fn seeded(conv: &str) -> common::TempDb {
        let db = fresh().await;
        db.conn().await.unwrap().execute_batch(&format!(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO conversation (id, kind) VALUES ('{conv}', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('{conv}', 'g1', 'chan');",
        )).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, conv, "alice", "a1", 10, "-1 day").await;
        seed_watermark_reported(&db, conv, "bob", "b1", 1, "-400 days").await;
        add_envelope(&db, "e1", conv, 3).await;
        db
    }

    let via_endpoint = seeded("c1").await;
    let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
    apply_envelope_gc(&via_endpoint.conn().await.unwrap(), None, &body, STALE).await.unwrap();
    let endpoint_count = envelope_count(&via_endpoint, "c1").await;

    let via_sweep = seeded("c1").await;
    sweep_envelope_gc(&via_sweep.conn().await.unwrap(), STALE).await.unwrap();
    let sweep_count = envelope_count(&via_sweep, "c1").await;

    assert_eq!(endpoint_count, sweep_count, "sweep and endpoint must agree on the bound (#720)");
    assert_eq!(endpoint_count, 0, "both must collect the dormant-pinned envelope (fails pre-#720)");
}

/// The growth snapshot reflects post-cleanup survivors (#720).
#[tokio::test]
async fn sweep_reports_identity_free_growth_metrics() {
    let db = fresh().await;
    db.conn().await.unwrap().execute_batch(
        "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
         INSERT INTO conversation (id, kind) VALUES ('big', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('big', 'g1', 'chan');\
         INSERT INTO conversation (id, kind) VALUES ('small', 'channel');INSERT INTO channels (id, group_id, name) VALUES ('small', 'g1', 'chan');",
    ).await.unwrap();
    add_device(&db, "alice", "a1", false).await;
    add_envelope(&db, "b1", "big", 4).await;
    add_envelope(&db, "b2", "big", 5).await;
    add_envelope(&db, "b3", "big", 6).await;
    add_envelope(&db, "s1", "small", 9).await;

    let report = sweep_envelope_gc(&db.conn().await.unwrap(), STALE).await.unwrap();
    let m = report.metrics;

    assert_eq!(m.total_envelopes, 4, "total surviving envelopes across conversations");
    assert_eq!(m.conversations_with_envelopes, 2, "two conversations hold envelopes");
    assert_eq!(m.largest_conversation_envelopes, 3, "the worst offender holds three (no id)");
    assert!(
        m.oldest_sent_at.is_some() && m.largest_conversation_oldest_sent_at.is_some(),
        "oldest timestamps are surfaced (a timestamp is not an identifier)"
    );
}
