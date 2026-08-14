//! Envelope-GC retention. Drives the real `pollis_delivery` write handlers
//! against local libsql DBs, exactly as the DS runs them in production.
//!
//! **Part E (I3 — no TTL)** is the headline invariant: envelope retention is
//! bounded by the SLOWEST current member device and by nothing else. An envelope
//! a member device has not collected is retained however old it is. The 30-day
//! TTL that used to be OR'd into the deletion predicate deleted such envelopes on
//! its own — failure mode F3, silent permanent loss for any member offline longer
//! than a month.
//!
//! Parts A/B/D are the complementary #685 property: a revoked device can never
//! rejoin the MLS tree (I5), so it must not count toward the roster the watermark
//! gate is measured against — otherwise it holds the floor down forever.
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
//!   * **Part E** — the no-TTL invariant and the conservative edges (empty
//!     roster, never-reported device), plus the positive leg proving GC is
//!     bounded rather than disabled.
//!   * **Part F (#692)** — the tombstone `sent_at` floor must OUTLIVE this GC.
//!     An admin delete only ever reaches a recipient whose watermark sorts BELOW
//!     the tombstone (ingest selects `sent_at > last_fetched_at` and the cursor
//!     never rewinds), so the DS stamps the tombstone above the conversation's
//!     floor. That floor used to be `MAX(sent_at)` over envelopes STILL PRESENT
//!     — which is exactly what the pruning tested above empties. Part F runs the
//!     real GC first, then the real delete, and proves the tombstone still clears
//!     every surviving cursor.
//!   * **Part G (#689)** — the GC *trigger* is the DS-internal sweep
//!     `sweep_envelope_gc`, not a member's ingest path. One sweep collects a
//!     conversation every member device has caught up on — with no ingest and no
//!     per-conversation endpoint call — and spares one with an uncollected
//!     recipient.
//!
//! The mirror-image failures are covered as unit tests in
//! `pollis_delivery::messages` (the raw `CLEANUP_*` SQL and the floor query) and
//! end-to-end in `src-tauri/tests/flows/adversarial.rs`.

use pollis_delivery::account::{apply_revoke_device, RevokeDeviceBody};
use pollis_delivery::db::Db;
use pollis_delivery::messages::{
    apply_advance_watermark, apply_delete_message, apply_envelope_gc, sweep_envelope_gc,
    DeleteMessageBody, EnvelopeGcBody, WatermarkBody,
};
use pollis_delivery::profile::{apply_add_dm_member, apply_create_dm, AddDmMemberBody, CreateDmBody};
use pollis_delivery::groups::{apply_approve_join_request, ApproveJoinRequestBody};
use pollis_delivery::writes::WriteOutcome;

/// Every table the handlers under test touch. No FK `REFERENCES` (mirrors
/// `retention.rs`) — `connect_local` runs with `foreign_keys=OFF`, so standalone
/// tables suffice and keep the fixtures minimal.
const SCHEMA: &str = "\
CREATE TABLE message_envelope (\
  id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, sent_at TEXT NOT NULL, \
  sender_id TEXT NOT NULL DEFAULT '', ciphertext TEXT NOT NULL DEFAULT '', \
  reply_to_id TEXT, type TEXT NOT NULL DEFAULT 'message', target_message_id TEXT);\
CREATE TABLE user_device (user_id TEXT NOT NULL, device_id TEXT NOT NULL, revoked_at TEXT);\
CREATE TABLE group_member (group_id TEXT NOT NULL, user_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member');\
CREATE TABLE groups (id TEXT PRIMARY KEY);\
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
  last_fetched_at TEXT NOT NULL, reported_at TEXT, \
  PRIMARY KEY (conversation_id, user_id, device_id));\
CREATE TABLE mls_key_package (id TEXT PRIMARY KEY, user_id TEXT NOT NULL, device_id TEXT NOT NULL);\
CREATE TABLE attachment_ref (content_hash TEXT NOT NULL, message_id TEXT NOT NULL, \
  PRIMARY KEY (content_hash, message_id));";

/// The device-liveness staleness window these tests drive GC with (#720). Wide
/// enough that legacy fixtures (which seed `reported_at = NULL`, treated as live)
/// are never excluded; the #720 tests set `reported_at` and probe the boundary.
const STALE: &str = "-6 months";

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

/// Seed a watermark with an explicit `reported_at` (the #720 liveness signal),
/// independent of the message cursor. `cursor`/`reported` are `datetime('now', …)`
/// offsets.
async fn seed_watermark_reported(
    db: &Db,
    conv: &str,
    user: &str,
    device: &str,
    cursor: &str,
    reported: &str,
) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, reported_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4), datetime('now', ?5))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                cursor.to_string(),
                reported.to_string()
            ],
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
    let out = apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();
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
    apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();

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

// ── Part E — I3: retention is bounded by the slowest member device, not a TTL ──

/// Seed a channel `c1` in group `g1` with the given members.
async fn channel_with_members(db: &Db, members: &[&str]) {
    let conn = db.conn().unwrap();
    conn.execute("INSERT INTO channels (id, group_id) VALUES ('c1', 'g1')", ())
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
    let out = apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

/// **The regression test for failure mode F3.**
///
/// Bob's device has been offline for 500 days; the envelope is 400 days old —
/// more than 13× the deleted 30-day TTL. Under the old predicate the TTL arm
/// deleted this row on its own, without consulting a single watermark, and bob
/// permanently lost a message he was owed. Retention must be bounded by bob's
/// cursor instead, so the envelope survives.
///
/// The age is an explicit `datetime('now','-400 days')` offset rather than a real
/// elapsed wall-clock interval, so "well over 30 days" is a property of the
/// fixture and not of when the suite happens to run.
#[tokio::test]
async fn ancient_envelope_survives_while_a_member_device_has_not_collected_it() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;
    // Alice is caught up; bob's cursor sits BELOW the envelope.
    seed_watermark(&db, "c1", "alice", "a1", "-1 day").await;
    seed_watermark(&db, "c1", "bob", "b1", "-500 days").await;
    add_envelope(&db, "ancient", "c1", "-400 days").await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "a 400-day-old envelope below a current member device's cursor must \
         survive: retention is bounded by the slowest member device, never by \
         wall-clock age (I3, failure mode F3)"
    );
}

/// The other never-collected shape: the absent member device has never reported a
/// watermark at all. No watermark is no evidence of delivery, so the envelope is
/// retained however old it is.
#[tokio::test]
async fn ancient_envelope_survives_for_a_device_that_never_reported() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b-silent", false).await;
    // Only alice has ever reported.
    seed_watermark(&db, "c1", "alice", "a1", "-1 day").await;
    add_envelope(&db, "ancient", "c1", "-400 days").await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "a member device with no watermark row must hold even a 400-day-old \
         envelope (I3, failure mode F3)"
    );
}

/// The positive leg: GC is bounded, not disabled. Once EVERY current member
/// device has collected past an envelope it is deleted — and the envelope still
/// above the floor is kept. Both envelopes are far younger than the old TTL, so
/// this cannot be passing via the deleted arm.
#[tokio::test]
async fn envelope_is_deleted_once_every_member_device_collected_past_it() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "alice", "a2", false).await;
    add_device(&db, "bob", "b1", false).await;
    add_envelope(&db, "collected", "c1", "-3 days").await;
    add_envelope(&db, "pending", "c1", "-1 hour").await;
    // Every device sits strictly above `collected` and strictly below `pending`.
    seed_watermark(&db, "c1", "alice", "a1", "-2 days").await;
    seed_watermark(&db, "c1", "alice", "a2", "-2 days").await;
    seed_watermark(&db, "c1", "bob", "b1", "-2 days").await;

    gc_channel(&db).await;

    let conn = db.conn().unwrap();
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

/// A revoked device must not pin retention forever. Bob's only device is revoked
/// and its cursor is 500 days stale; alice — the sole remaining LIVE member
/// device — has collected past the envelope, so it goes. Without the
/// `revoked_at IS NULL` filter the dead cursor would hold this envelope for good.
#[tokio::test]
async fn a_revoked_device_does_not_pin_retention_forever() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b-revoked", true).await;
    seed_watermark(&db, "c1", "alice", "a1", "-1 day").await;
    seed_watermark(&db, "c1", "bob", "b-revoked", "-500 days").await;
    add_envelope(&db, "e1", "c1", "-3 days").await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "a revoked device can never rejoin the tree (I5), so its dead cursor must \
         not hold the retention floor down forever (#685)"
    );
}

/// The empty-member-set edge, pinned conservative. With no current member devices
/// the gate's aggregate degenerates to `COUNT 0 = COUNT 0` with `MIN(...) = NULL`
/// over zero rows, so the CASE yields NULL and `sent_at < NULL` is NULL — nothing
/// is deleted. The alternative reading of an empty roster ("everyone has
/// collected, drop it all") is precisely the unbounded delete this ticket removes.
#[tokio::test]
async fn an_empty_member_device_set_deletes_nothing() {
    let db = fresh().await;
    channel_with_members(&db, &["alice"]).await;
    // A member row exists, but the member has no device at all.
    add_envelope(&db, "ancient", "c1", "-400 days").await;
    add_envelope(&db, "fresh", "c1", "-1 hour").await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        2,
        "an empty member-device set must delete NOTHING, not everything"
    );
}

/// The same, taken one step further: excluding revoked devices must never
/// *manufacture* an empty roster that then deletes. When every device is revoked
/// the roster is empty, and an empty roster deletes nothing.
#[tokio::test]
async fn an_all_revoked_roster_deletes_nothing() {
    let db = fresh().await;
    channel_with_members(&db, &["alice"]).await;
    add_device(&db, "alice", "a-old", true).await;
    seed_watermark(&db, "c1", "alice", "a-old", "-500 days").await;
    add_envelope(&db, "ancient", "c1", "-400 days").await;

    gc_channel(&db).await;

    assert_eq!(
        envelope_count(&db, "c1").await,
        1,
        "excluding revoked devices must never leave a roster that deletes by \
         virtue of being empty"
    );
}

// ── Part F (#692) — the tombstone floor outlives envelope GC ──────────────────
//
// Part F uses RFC3339 timestamps rather than the `datetime('now', ...)` shape the
// parts above use. That is load-bearing, not cosmetic: `sent_at_after` parses the
// floor with `DateTime::parse_from_rfc3339` and falls back to plain `now` when
// that fails, so a SQLite-shaped floor would silently exercise the fallback path
// instead of the guard. RFC3339 is also what production actually stores — clients
// write `chrono::Utc::now().to_rfc3339()` into `sent_at`, and ingest copies a
// consumed `sent_at` straight into `last_fetched_at`.

/// An RFC3339 stamp `minutes` away from the DS's now, exactly as a client writes
/// it. Negative is the past; positive models a client clock running AHEAD.
fn rfc3339_from_now(minutes: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::minutes(minutes)).to_rfc3339()
}

async fn add_envelope_at(db: &Db, id: &str, conv: &str, sender: &str, sent_at: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, ?2, ?3, 'mls:00', ?4)",
            libsql::params![
                id.to_string(),
                conv.to_string(),
                sender.to_string(),
                sent_at.to_string()
            ],
        )
        .await
        .unwrap();
}

async fn seed_watermark_at(db: &Db, conv: &str, user: &str, device: &str, at: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                at.to_string()
            ],
        )
        .await
        .unwrap();
}

/// The single `type='delete'` tombstone in `conv`, as `(id, sent_at)`.
async fn only_tombstone(db: &Db, conv: &str) -> (String, String) {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT id, sent_at FROM message_envelope \
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
    let out = (
        row.get::<String>(0).unwrap(),
        row.get::<String>(1).unwrap(),
    );
    assert!(
        rows.next().await.unwrap().is_none(),
        "expected exactly one tombstone"
    );
    out
}

/// Every surviving `last_fetched_at` in `conv` — the cursors the tombstone has to
/// clear to be fetched at all.
async fn watermark_values(db: &Db, conv: &str) -> Vec<String> {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT last_fetched_at FROM conversation_watermark WHERE conversation_id = ?1",
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

/// **The #692 regression test.** The whole conversation has been GC'd — every
/// current member device reported a watermark past every envelope, so the real
/// cleanup predicate emptied `message_envelope`. The watermark rows survive that
/// pruning, and they were written from client clocks running an HOUR AHEAD of the
/// DS clock. An admin then deletes the message.
///
/// With the floor read only from surviving envelopes there is nothing left to
/// read: the floor is NULL, `sent_at_after` falls back to wall-clock `now`, and
/// `now` sorts an hour BELOW every surviving cursor. Ingest's `sent_at >
/// last_fetched_at` then never selects the tombstone, the cursor never rewinds to
/// pick it up later, and the deleted message stays readable on those devices
/// forever. Consulting `MAX(last_fetched_at)` — the evidence GC does not destroy
/// — is what makes the tombstone reachable.
#[tokio::test]
async fn tombstone_clears_watermarks_that_outlived_envelope_gc() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    db.conn()
        .unwrap()
        .execute("UPDATE group_member SET role = 'admin' WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;

    // Bob's client clock runs ahead: his message is stamped 30 minutes into the
    // DS's future, and both devices' cursors — copied from that `sent_at` on
    // ingest — end up an hour ahead.
    add_envelope_at(&db, "m1", "c1", "bob", &rfc3339_from_now(30)).await;
    let alice_cursor = rfc3339_from_now(60);
    let bob_cursor = rfc3339_from_now(60);
    seed_watermark_at(&db, "c1", "alice", "a1", &alice_cursor).await;
    seed_watermark_at(&db, "c1", "bob", "b1", &bob_cursor).await;

    // The REAL cleanup predicate, not a hand-emptied table: every current member
    // device has collected past `m1`, so it is pruned.
    gc_channel(&db).await;
    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "fixture precondition: watermark-gated GC must have emptied the \
         conversation, leaving the floor with no envelope to read"
    );

    // Alice (a group admin) deletes bob's message — the admin branch, which is
    // the one that writes a tombstone.
    let out = apply_delete_message(
        &db.conn().unwrap(),
        Some("alice"),
        &DeleteMessageBody {
            message_id: "m1".into(),
            conversation_id: "c1".into(),
            msg_sender_id: Some("bob".into()),
            actor_id: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "admin delete must be allowed");

    let (_, tombstone_sent_at) = only_tombstone(&db, "c1").await;
    let cursors = watermark_values(&db, "c1").await;
    assert_eq!(cursors.len(), 2, "both watermark rows must have survived GC");
    for cursor in &cursors {
        assert!(
            tombstone_sent_at.as_str() > cursor.as_str(),
            "tombstone {tombstone_sent_at} must sort strictly above the surviving \
             cursor {cursor}: ingest selects `sent_at > last_fetched_at` and never \
             rewinds, so a tombstone at or below a cursor is skipped PERMANENTLY \
             and the deleted message stays readable on that device (#692)"
        );
    }
}

/// The complementary shape, to prove the widened floor did not simply become
/// "always jump forward": with every surviving cursor BEHIND the DS clock the
/// tombstone keeps a stamp at wall-clock now — above the cursors, but not pushed
/// into the future by them.
#[tokio::test]
async fn tombstone_is_not_pushed_forward_by_watermarks_behind_the_ds_clock() {
    let db = fresh().await;
    channel_with_members(&db, &["alice", "bob"]).await;
    db.conn()
        .unwrap()
        .execute("UPDATE group_member SET role = 'admin' WHERE user_id = 'alice'", ())
        .await
        .unwrap();
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;
    add_envelope_at(&db, "m1", "c1", "bob", &rfc3339_from_now(-120)).await;
    seed_watermark_at(&db, "c1", "alice", "a1", &rfc3339_from_now(-60)).await;
    seed_watermark_at(&db, "c1", "bob", "b1", &rfc3339_from_now(-60)).await;

    gc_channel(&db).await;
    assert_eq!(envelope_count(&db, "c1").await, 0, "fixture precondition");

    let before = chrono::Utc::now();
    apply_delete_message(
        &db.conn().unwrap(),
        Some("alice"),
        &DeleteMessageBody {
            message_id: "m1".into(),
            conversation_id: "c1".into(),
            msg_sender_id: Some("bob".into()),
            actor_id: None,
        },
    )
    .await
    .unwrap();
    let after = chrono::Utc::now();

    let (_, tombstone_sent_at) = only_tombstone(&db, "c1").await;
    // Compared as instants, not lexically: the bound is "inside the window the
    // call occupied", and the two stamps can carry different fraction widths.
    let stamped = chrono::DateTime::parse_from_rfc3339(&tombstone_sent_at)
        .expect("tombstone sent_at must be RFC3339")
        .to_utc();
    assert!(
        stamped >= before && stamped <= after,
        "with every cursor behind the DS clock the tombstone must keep plain \
         wall-clock now ({before} ..= {after}), got {tombstone_sent_at} — the \
         floor only ever pushes forward when something is already ahead"
    );
}

// ── Part G (#689) — the server-side sweep is the GC trigger ───────────────────
//
// GC used to fire only from whichever member device happened to ingest. This
// pins the new trigger: the DS-internal sweep collects a conversation every
// member device has caught up on WITHOUT any member calling the per-conversation
// endpoint, and spares one with an uncollected recipient. It fails on the old
// trigger (nothing drives GC when no member ingests) and passes on the sweep.

/// The sweep collects quiet-but-collected conversations and spares uncollected
/// ones — driving neither ingest nor the `/v1/envelopes/gc` endpoint. Covers a
/// channel AND a DM so it also proves the sweep classifies both predicates.
#[tokio::test]
async fn sweep_collects_quiet_conversations_and_spares_uncollected() {
    let db = fresh().await;
    db.conn()
        .unwrap()
        .execute_batch(
            // A channel every member device has caught up on.
            "INSERT INTO group_member (group_id, user_id) VALUES ('gA', 'alice');\
             INSERT INTO channels (id, group_id) VALUES ('cA', 'gA');\
             INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) \
               VALUES ('dB', 'bob', 'bob', datetime('now'));\
             INSERT INTO group_member (group_id, user_id) VALUES ('gC', 'carol');\
             INSERT INTO channels (id, group_id) VALUES ('cC', 'gC');",
        )
        .await
        .unwrap();

    // cA — channel, its one device caught up past the envelope.
    add_device(&db, "alice", "aA", false).await;
    seed_watermark(&db, "cA", "alice", "aA", "-1 day").await;
    add_envelope(&db, "eA", "cA", "-5 days").await;

    // dB — DM, its one device caught up past the envelope.
    add_device(&db, "bob", "bB", false).await;
    seed_watermark(&db, "dB", "bob", "bB", "-1 day").await;
    add_envelope(&db, "eB", "dB", "-5 days").await;

    // cC — channel with an uncollected recipient: carol's device never reported.
    add_device(&db, "carol", "cC-dev", false).await;
    add_envelope(&db, "eC", "cC", "-5 days").await;

    // No member ingests, no endpoint call — the sweep is the sole trigger.
    let visited = sweep_envelope_gc(&db.conn().unwrap(), STALE).await.unwrap().visited;

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

// ─── Part H (#720 — device-liveness bound) ───────────────────────────────────
//
// The envelope floor is now bounded by member watermarks AND by device liveness:
// a member device that has not REPORTED a watermark within the staleness window
// stops pinning retention (`apply_advance_watermark` stamps `reported_at`; the
// `CLEANUP_*` `?2` arm excludes the stale). These drive the REAL handlers, and
// each asserts a COLLECTION the pre-#720 watermark-only predicate never performs,
// so each fails against the pre-change code.

/// A device that reported a watermark long ago and then went silent stops pinning
/// once every LIVE device has read past the envelope. Real handler path.
/// Pre-#720 bob's ancient cursor pins `MIN(cw)` below the envelope forever.
#[tokio::test]
async fn dormant_device_stops_pinning_via_endpoint() {
    let db = fresh().await;
    db.conn()
        .unwrap()
        .execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO channels (id, group_id) VALUES ('c1', 'g1');",
        )
        .await
        .unwrap();
    add_device(&db, "alice", "a1", false).await;
    add_device(&db, "bob", "b1", false).await;
    seed_watermark_reported(&db, "c1", "alice", "a1", "-1 day", "-1 day").await;
    seed_watermark_reported(&db, "c1", "bob", "b1", "-500 days", "-400 days").await;
    add_envelope(&db, "e1", "c1", "-100 days").await;

    let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
    apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();

    assert_eq!(
        envelope_count(&db, "c1").await,
        0,
        "a device silent past the window stops pinning; the envelope every LIVE \
         device read past is collected (#720)"
    );
}

/// **Reversibility.** A dormant device that comes back and reports a watermark —
/// even one whose cursor is still behind the envelope — is live again IMMEDIATELY
/// and re-pins. This drives the real `apply_advance_watermark`, proving it stamps
/// `reported_at` and flips the device back into the roster.
#[tokio::test]
async fn a_returning_device_reports_and_pins_again() {
    // Control: bob dormant, does NOT report → the envelope is collected.
    {
        let db = fresh().await;
        db.conn().unwrap().execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO channels (id, group_id) VALUES ('c1', 'g1');",
        ).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, "c1", "alice", "a1", "-1 day", "-1 day").await;
        seed_watermark_reported(&db, "c1", "bob", "b1", "-500 days", "-400 days").await;
        add_envelope(&db, "e1", "c1", "-100 days").await;

        let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
        apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();
        assert_eq!(envelope_count(&db, "c1").await, 0, "dormant → collected (control)");
    }
    // Comeback: bob reports a watermark BEFORE GC. Its cursor stays behind the
    // envelope, but `reported_at` is now fresh → bob is live → it re-pins.
    {
        let db = fresh().await;
        db.conn().unwrap().execute_batch(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO channels (id, group_id) VALUES ('c1', 'g1');",
        ).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, "c1", "alice", "a1", "-1 day", "-1 day").await;
        seed_watermark_reported(&db, "c1", "bob", "b1", "-500 days", "-400 days").await;
        add_envelope(&db, "e1", "c1", "-100 days").await;

        // Bob comes back and reports — a cursor still BEHIND the envelope (-200d).
        // The monotone guard keeps the max cursor, but `reported_at` jumps to now.
        let wm = WatermarkBody {
            conversation_id: "c1".into(),
            user_id: Some("bob".into()),
            device_id: "b1".into(),
            last_fetched_at: past_datetime(&db, "-200 days").await,
        };
        apply_advance_watermark(&db.conn().unwrap(), None, &wm).await.unwrap();

        let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
        apply_envelope_gc(&db.conn().unwrap(), None, &body, STALE).await.unwrap();

        assert_eq!(
            envelope_count(&db, "c1").await,
            1,
            "reporting a watermark makes a stale device live again immediately, so \
             it re-pins the envelope below its cursor (#720 reversibility)"
        );
    }
}

/// The sweep and the per-conversation endpoint apply the SAME liveness predicate
/// (both flow through `cleanup_conversation_envelopes`), so they agree on the
/// same dormant-device fixture: BOTH collect the envelope. Two GC paths with two
/// different predicates is exactly the invalid state this repo forbids (#720
/// checkbox 3). Pre-#720 both KEEP the envelope, so the collection assertion also
/// fails against the pre-change code.
#[tokio::test]
async fn sweep_and_endpoint_agree_on_the_staleness_bound() {
    async fn seeded(conv: &str) -> Db {
        let db = fresh().await;
        db.conn().unwrap().execute_batch(&format!(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');\
             INSERT INTO channels (id, group_id) VALUES ('{conv}', 'g1');",
        )).await.unwrap();
        add_device(&db, "alice", "a1", false).await;
        add_device(&db, "bob", "b1", false).await;
        seed_watermark_reported(&db, conv, "alice", "a1", "-1 day", "-1 day").await;
        seed_watermark_reported(&db, conv, "bob", "b1", "-500 days", "-400 days").await;
        add_envelope(&db, "e1", conv, "-100 days").await;
        db
    }

    // Endpoint path.
    let via_endpoint = seeded("c1").await;
    let body = EnvelopeGcBody { conversation_id: "c1".into(), is_dm: false, actor_id: Some("alice".into()) };
    apply_envelope_gc(&via_endpoint.conn().unwrap(), None, &body, STALE).await.unwrap();
    let endpoint_count = envelope_count(&via_endpoint, "c1").await;

    // Sweep path — same fixture, no endpoint call, no ingest.
    let via_sweep = seeded("c1").await;
    sweep_envelope_gc(&via_sweep.conn().unwrap(), STALE).await.unwrap();
    let sweep_count = envelope_count(&via_sweep, "c1").await;

    assert_eq!(endpoint_count, sweep_count, "sweep and endpoint must agree on the bound (#720)");
    assert_eq!(endpoint_count, 0, "both must collect the dormant-pinned envelope (fails pre-#720)");
}

/// The sweep gathers the identity-free growth snapshot on its walk (#720 checkbox
/// 1): counts and the worst offender's shape, reflecting POST-cleanup survivors.
#[tokio::test]
async fn sweep_reports_identity_free_growth_metrics() {
    let db = fresh().await;
    db.conn().unwrap().execute_batch(
        "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');\
         INSERT INTO channels (id, group_id) VALUES ('big', 'g1');\
         INSERT INTO channels (id, group_id) VALUES ('small', 'g1');",
    ).await.unwrap();
    // Alice never reported anywhere → COUNT(ud) != COUNT(cw) → nothing collected,
    // so every envelope survives and the snapshot sees them all.
    add_device(&db, "alice", "a1", false).await;
    add_envelope(&db, "b1", "big", "-10 days").await;
    add_envelope(&db, "b2", "big", "-9 days").await;
    add_envelope(&db, "b3", "big", "-8 days").await;
    add_envelope(&db, "s1", "small", "-2 days").await;

    let report = sweep_envelope_gc(&db.conn().unwrap(), STALE).await.unwrap();
    let m = report.metrics;

    assert_eq!(m.total_envelopes, 4, "total surviving envelopes across conversations");
    assert_eq!(m.conversations_with_envelopes, 2, "two conversations hold envelopes");
    assert_eq!(m.largest_conversation_envelopes, 3, "the worst offender holds three (no id)");
    assert!(
        m.oldest_sent_at.is_some() && m.largest_conversation_oldest_sent_at.is_some(),
        "oldest timestamps are surfaced (a timestamp is not an identifier)"
    );
}

/// Read a `datetime('now', offset)` value back as a string, so a watermark body
/// can carry a cursor at a known relative age in the DB's own clock/format.
async fn past_datetime(db: &Db, offset: &str) -> String {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query("SELECT datetime('now', ?1)", libsql::params![offset.to_string()])
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<String>(0).unwrap()
}
