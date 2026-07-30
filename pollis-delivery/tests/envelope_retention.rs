//! Envelope-GC retention against a REVOKED device (#685). Drives the real
//! `pollis_delivery` write handlers against local libsql DBs, exactly as the DS
//! runs them in production.
//!
//! A revoked device can never rejoin the MLS tree (I5), so it must not count
//! toward the roster the envelope-GC watermark gate is measured against. Three
//! parts are proved here:
//!
//!   * **Part A** — `apply_envelope_gc` prunes an envelope every LIVE device has
//!     read past, even when a revoked device's watermark is stale (pins
//!     `MIN(cw)` down) or entirely absent (breaks the `COUNT(ud) = COUNT(cw)`
//!     "every device reported" check that otherwise disables pruning).
//!   * **Part B** — the watermark-seeding writes (DM create, DM member add, group
//!     member add) never seed a row for a revoked device, so a device revoked
//!     before it ever synced cannot re-introduce the wedge on the next join.
//!   * **Part D** — `apply_revoke_device` DELETEs the revoked device's watermark
//!     rows outright, so the dead cursor stops existing rather than merely being
//!     filtered out by every future reader — and does so scoped to that one
//!     device of that one user.
//!
//! The mirror-image failures are covered as unit tests in
//! `pollis_delivery::messages` (the raw `CLEANUP_*` SQL) and end-to-end in
//! `src-tauri/tests/flows/adversarial.rs`.

use pollis_delivery::account::{apply_revoke_device, RevokeDeviceBody};
use pollis_delivery::db::Db;
use pollis_delivery::messages::{apply_envelope_gc, EnvelopeGcBody};
use pollis_delivery::profile::{apply_add_dm_member, apply_create_dm, AddDmMemberBody, CreateDmBody};
use pollis_delivery::groups::{apply_approve_join_request, ApproveJoinRequestBody};
use pollis_delivery::writes::WriteOutcome;

/// Every table the handlers under test touch. No FK `REFERENCES` (mirrors
/// `retention.rs`) — `connect_local` runs with `foreign_keys=OFF`, so standalone
/// tables suffice and keep the fixtures minimal.
const SCHEMA: &str = "\
CREATE TABLE message_envelope (id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, sent_at TEXT NOT NULL);\
CREATE TABLE user_device (user_id TEXT NOT NULL, device_id TEXT NOT NULL, revoked_at TEXT);\
CREATE TABLE group_member (group_id TEXT NOT NULL, user_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member');\
CREATE TABLE channels (id TEXT PRIMARY KEY, group_id TEXT NOT NULL);\
CREATE TABLE dm_channel (id TEXT PRIMARY KEY, created_by TEXT NOT NULL, created_at TEXT NOT NULL);\
CREATE TABLE dm_channel_member (\
  dm_channel_id TEXT NOT NULL, user_id TEXT NOT NULL, added_by TEXT NOT NULL, \
  added_at TEXT NOT NULL, accepted_at TEXT, PRIMARY KEY (dm_channel_id, user_id));\
CREATE TABLE group_join_request (\
  id TEXT PRIMARY KEY, group_id TEXT NOT NULL, requester_id TEXT NOT NULL, \
  reviewed_by TEXT, reviewed_at TEXT, status TEXT NOT NULL DEFAULT 'pending');\
CREATE TABLE conversation_watermark (\
  conversation_id TEXT NOT NULL, user_id TEXT NOT NULL, device_id TEXT NOT NULL, \
  last_fetched_at TEXT NOT NULL, PRIMARY KEY (conversation_id, user_id, device_id));\
CREATE TABLE mls_key_package (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, device_id TEXT NOT NULL);";

async fn fresh() -> Db {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    db
}

async fn add_device(db: &Db, user: &str, device: &str, revoked: bool) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![user.to_string(), device.to_string(), revoked as i64],
        )
        .await
        .unwrap();
}

async fn seed_watermark(db: &Db, conv: &str, user: &str, device: &str, offset: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4))",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
}

async fn add_envelope(db: &Db, id: &str, conv: &str, offset: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at) \
             VALUES (?1, ?2, datetime('now', ?3))",
            libsql::params![id.to_string(), conv.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
}

async fn envelope_count(db: &Db, conv: &str) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
            libsql::params![conv.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// The device ids that got a seeded watermark for `conv`, ascending.
async fn watermark_devices(db: &Db, conv: &str) -> Vec<String> {
    let conn = db.conn().unwrap();
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

// ── Part A — apply_envelope_gc through the public handler ─────────────────────

/// Channel GC, the "no watermark row" mode: a revoked device with no `cw` row
/// must not disable pruning via the `COUNT(ud) != COUNT(cw)` gate.
#[tokio::test]
async fn channel_gc_ignores_revoked_device_with_no_watermark() {
    let db = fresh().await;
    db.conn()
        .unwrap()
        .execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO channels (id, group_id) VALUES ('c1', 'g1');",
        )
        .await
        .unwrap();
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-old", true).await;
    // Only the live device reported; the revoked device has no watermark row.
    seed_watermark(&db, "c1", "alice", "a-live", "-1 day").await;
    add_envelope(&db, "e1", "c1", "-5 days").await;

    let body = EnvelopeGcBody {
        conversation_id: "c1".into(),
        is_dm: false,
        actor_id: Some("alice".into()),
    };
    let out = apply_envelope_gc(&db.conn().unwrap(), None, &body).await.unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "channel GC must prune once every LIVE device has read past the envelope; \
         a revoked device's absent watermark must not disable pruning (#685)"
    );
}

/// DM GC, the "stale watermark row" mode: a revoked device's old cursor must not
/// pin `MIN(cw)` down and keep an envelope the live device has read past.
#[tokio::test]
async fn dm_gc_ignores_stale_revoked_watermark() {
    let db = fresh().await;
    db.conn()
        .unwrap()
        .execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) \
                  VALUES ('d1', 'alice', 'alice', datetime('now'))", ())
        .await
        .unwrap();
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-old", true).await;
    seed_watermark(&db, "d1", "alice", "a-live", "-1 day").await;
    seed_watermark(&db, "d1", "alice", "a-old", "-10 days").await;
    add_envelope(&db, "e1", "d1", "-5 days").await;

    let body = EnvelopeGcBody {
        conversation_id: "d1".into(),
        is_dm: true,
        actor_id: Some("alice".into()),
    };
    apply_envelope_gc(&db.conn().unwrap(), None, &body).await.unwrap();

    assert_eq!(
        envelope_count(&db, "d1").await,
        0,
        "DM GC must prune below the LIVE device's cursor; a revoked device's stale \
         watermark must not pin MIN(cw) (#685)"
    );
}

// ── Part B — watermark seeding never covers a revoked device ──────────────────

/// DM create seeds a watermark for every member device EXCEPT revoked ones.
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
    apply_create_dm(&db.conn().unwrap(), None, &body).await.unwrap();

    assert_eq!(
        watermark_devices(&db, "d1").await,
        vec!["a1".to_string(), "b-live".to_string()],
        "DM create must seed watermarks for live devices only — never the revoked \
         'b-rev' (#685)"
    );
}

/// DM member-add seeds a watermark for the new member's live devices only.
#[tokio::test]
async fn add_dm_member_skips_revoked_devices() {
    let db = fresh().await;
    db.conn()
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
    apply_add_dm_member(&db.conn().unwrap(), None, &body).await.unwrap();

    assert_eq!(
        watermark_devices(&db, "d1").await,
        vec!["b-live".to_string()],
        "DM member-add must seed a watermark for the live device only — never the \
         revoked 'b-rev' (#685)"
    );
}

/// Group member-add (via join-request approval) seeds per-channel watermarks for
/// the new member's live devices only.
#[tokio::test]
async fn approve_join_request_skips_revoked_devices() {
    let db = fresh().await;
    db.conn()
        .unwrap()
        .execute_batch(
            "INSERT INTO channels (id, group_id) VALUES ('c1', 'g1');\
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
    let out = apply_approve_join_request(&db.conn().unwrap(), None, &body).await.unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert_eq!(
        watermark_devices(&db, "c1").await,
        vec!["b-live".to_string()],
        "group member-add must seed a watermark for the live device only — never \
         the revoked 'b-rev' (#685)"
    );
}

// ── Part D — revocation removes the dead cursor at the chokepoint ─────────────

/// `apply_revoke_device` must DELETE the revoked device's `conversation_watermark`
/// rows, not merely leave them for the GC reads to filter out.
///
/// The read-time filters (Parts A/B) make the floor correct, but a cursor that can
/// never advance is itself the invalid state — every future reader of
/// `conversation_watermark` would have to remember to exclude it. Removing it in
/// the revoke transaction means it cannot be represented at all.
///
/// The live sibling device's watermark is the control: revocation must be surgical
/// (`WHERE user_id = actor AND device_id = ?`), so a pass cannot be earned by
/// wiping the whole table.
#[tokio::test]
async fn revoke_device_deletes_that_devices_watermarks_only() {
    let db = fresh().await;
    add_device(&db, "alice", "a-live", false).await;
    add_device(&db, "alice", "a-doomed", false).await;
    add_device(&db, "bob", "b-live", false).await;

    // The doomed device holds a cursor in two conversations; the live sibling and
    // a different user hold cursors that must survive untouched.
    seed_watermark(&db, "c1", "alice", "a-doomed", "-3 days").await;
    seed_watermark(&db, "d1", "alice", "a-doomed", "-3 days").await;
    seed_watermark(&db, "c1", "alice", "a-live", "-1 day").await;
    seed_watermark(&db, "c1", "bob", "b-live", "-1 day").await;

    let body = RevokeDeviceBody {
        device_id: "a-doomed".into(),
        user_id: None,
    };
    let out = apply_revoke_device(&db.conn().unwrap(), Some("alice"), &body)
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

/// Revocation is self-scoped: it must never delete another user's watermark rows,
/// even when the body names that user's device.
#[tokio::test]
async fn revoke_device_cannot_delete_another_users_watermarks() {
    let db = fresh().await;
    add_device(&db, "bob", "b-live", false).await;
    seed_watermark(&db, "c1", "bob", "b-live", "-1 day").await;

    // Alice is the authenticated actor; the DELETE is bound `user_id = actor`, so
    // naming bob's device id must be inert rather than destructive.
    let body = RevokeDeviceBody {
        device_id: "b-live".into(),
        user_id: None,
    };
    apply_revoke_device(&db.conn().unwrap(), Some("alice"), &body)
        .await
        .unwrap();

    assert_eq!(
        watermark_devices(&db, "c1").await,
        vec!["b-live".to_string()],
        "alice revoking a device id that belongs to bob must not touch bob's \
         cursor — the DELETE is bound WHERE user_id = actor (#685)"
    );
}
