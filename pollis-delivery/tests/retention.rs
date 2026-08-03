//! Commit-log retention floor (issue #539, I4). Drives the real
//! `pollis_delivery::commit` prune path against local libsql DBs — membership on
//! a MAIN handle, the commit log + reported high-waters on a LOG handle, exactly
//! as production splits them (`AppState { db, log_db }`).
//!
//! The properties proved here mirror `specs/tla/Delivery.tla` (Spec B):
//!   * Tier 1 never prunes a commit a current member still needs (the floor is
//!     the MIN over member devices — `NoLossForCurrentMember`).
//!   * A revoked device never pins the floor down (it can't rejoin — I5).
//!   * An unreported member disables Tier-1 pruning (no unsafe lower bound).
//!   * The DELETE leaves the UNIQUE(conversation_id, generation, epoch)
//!     fork-dedup index intact (migration 000004, log DB).
//!   * `record_commit_since` is monotone (a stale report can't lower a device's
//!     high-water and prune commits it still needs).
//!
//! Under suite generations (#454 P4) the same properties are exercised one key
//! wider: a device still on a retired lineage must not be pruned past by Tier 1,
//! and a fully-vacated lineage is retired outright.

use pollis_delivery::commit::{
    delete_commits_below, delete_generations_below, prune_commit_log, record_commit_since,
    PRUNE_MAX_BEHIND_HEAD, PRUNE_SLACK_EPOCHS,
};
use pollis_delivery::db::Db;

const MAIN_SCHEMA: &str = "\
CREATE TABLE user_device (user_id TEXT NOT NULL, device_id TEXT NOT NULL, revoked_at TEXT);\
CREATE TABLE group_member (group_id TEXT NOT NULL, user_id TEXT NOT NULL);\
CREATE TABLE channels (id TEXT PRIMARY KEY, group_id TEXT NOT NULL);\
CREATE TABLE dm_channel_member (dm_channel_id TEXT NOT NULL, user_id TEXT NOT NULL);";

const LOG_SCHEMA: &str = "\
CREATE TABLE mls_commit_log (\
  seq INTEGER PRIMARY KEY AUTOINCREMENT,\
  conversation_id TEXT NOT NULL,\
  epoch INTEGER NOT NULL,\
  sender_id TEXT NOT NULL,\
  commit_data BLOB NOT NULL,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  added_user_id TEXT,\
  added_device_ids TEXT,\
  generation INTEGER NOT NULL DEFAULT 0\
);\
CREATE UNIQUE INDEX idx_mls_commit_conv_gen_epoch ON mls_commit_log (conversation_id, generation, epoch);\
CREATE TABLE mls_commit_since (\
  conversation_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  device_id TEXT NOT NULL,\
  since_epoch INTEGER NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
  generation INTEGER NOT NULL DEFAULT 0,\
  PRIMARY KEY (conversation_id, user_id, device_id)\
);";

async fn fresh(schema: &str) -> Db {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(schema).await.expect("schema");
    db
}

async fn add_member(main: &Db, group_id: &str, user_id: &str, device_id: &str) {
    let conn = main.conn().unwrap();
    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
        libsql::params![group_id, user_id],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO user_device (user_id, device_id, revoked_at) VALUES (?1, ?2, NULL)",
        libsql::params![user_id, device_id],
    )
    .await
    .unwrap();
}

async fn revoke_device(main: &Db, device_id: &str) {
    main.conn()
        .unwrap()
        .execute(
            "UPDATE user_device SET revoked_at = datetime('now') WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
}

/// Append commits at epochs `0..=up_to` of generation `gen` for `conv`.
async fn seed_commits_in(log: &Db, conv: &str, gen: i64, up_to: i64) {
    let conn = log.conn().unwrap();
    for e in 0..=up_to {
        conn.execute(
            "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, 'u', ?4)",
            libsql::params![conv, gen, e, vec![e as u8]],
        )
        .await
        .unwrap();
    }
}

/// Append commits at epochs `0..=up_to` of generation 0 — the lineage every
/// pre-P4 conversation is in.
async fn seed_commits(log: &Db, conv: &str, up_to: i64) {
    seed_commits_in(log, conv, 0, up_to).await;
}

/// The surviving epochs of generation `gen` for `conv`, ascending.
async fn epochs_in(log: &Db, conv: &str, gen: i64) -> Vec<i64> {
    let conn = log.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2 \
             ORDER BY epoch ASC",
            libsql::params![conv, gen],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<i64>(0).unwrap());
    }
    out
}

/// The surviving epochs of generation 0 for `conv`, ascending.
async fn epochs(log: &Db, conv: &str) -> Vec<i64> {
    epochs_in(log, conv, 0).await
}

/// TIER 1: the slowest current member pins the floor — commits it still needs
/// (`epoch >= min_since`) are never pruned, but stale prefix epochs are.
#[tokio::test]
async fn tier1_prunes_prefix_but_keeps_slowest_member() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    // head = 25 (epochs 0..24). Alice caught up to 20, Bob (slowest) to 15.
    seed_commits(&log, conv, 24).await;
    let log_conn = log.conn().unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    // floor = min(15,20) - SLACK = 15 - 8 = 7.
    assert_eq!(report.floor, 15 - PRUNE_SLACK_EPOCHS);
    let surviving = epochs(&log, conv).await;
    assert_eq!(*surviving.first().unwrap(), 7, "prefix below the floor is pruned");
    assert!(
        surviving.contains(&15),
        "the slowest member's applied epoch must survive (NoLossForCurrentMember)"
    );
    assert!(surviving.contains(&24), "the head epoch always survives");
    assert!(!surviving.contains(&6), "epoch below the floor must be gone");
}

/// A member that has NOT reported disables Tier 1: without a safe lower bound
/// over the whole roster, nothing is pruned (Tier 2 does not bind on a short log).
#[tokio::test]
async fn unreported_member_blocks_tier1_pruning() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    seed_commits(&log, conv, 24).await;
    // Only alice reports; bob is unaccounted for.
    record_commit_since(&log.conn().unwrap(), conv, "alice", "a-dev", 0, 20)
        .await
        .unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log.conn().unwrap(), conv)
        .await
        .unwrap();

    assert_eq!(report.floor, 0, "an unreported member keeps the floor at 0");
    assert_eq!(report.deleted, 0, "nothing pruned while a member is unaccounted for");
    assert_eq!(epochs(&log, conv).await.len(), 25);
}

/// A REVOKED device never pins the floor down (it can't rejoin — I5). Alice's old
/// device lagged at epoch 2 but was revoked; only her live device (caught up to
/// 20) counts, so the floor advances.
#[tokio::test]
async fn revoked_device_does_not_pin_the_floor() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-old").await;
    // Second device for the same user, then revoke the old one.
    main.conn()
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) VALUES ('alice', 'a-new', NULL)",
            (),
        )
        .await
        .unwrap();
    revoke_device(&main, "a-old").await;

    seed_commits(&log, conv, 24).await;
    let log_conn = log.conn().unwrap();
    // The revoked device reported a low epoch; the live one is caught up.
    record_commit_since(&log_conn, conv, "alice", "a-old", 0, 2).await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-new", 0, 20).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    // Only the live device (20) counts → floor = 20 - 8 = 12, not 2 - 8.
    assert_eq!(report.floor, 20 - PRUNE_SLACK_EPOCHS);
    assert!(!epochs(&log, conv).await.contains(&2), "the revoked laggard does not hold epoch 2");
}

/// #681 — a device reporting a high-water FAR ABOVE the real head cannot delete
/// the live commit log. `prune_floor` is clamped to `head - 1`, so the head
/// commit survives, `head_epoch` is unchanged, and the group stays advanceable —
/// even though the same report, UNCLAMPED, drove the floor to ~`i64::MAX` and made
/// `delete_commits_below` take every epoch, resetting the head to 0 (the wipe).
#[tokio::test]
async fn bogus_high_water_above_head_cannot_wipe_the_live_log() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    // A single-member conversation, so the bogus report IS the roster minimum and
    // drives Tier 1 directly — the worst case for the clamp.
    add_member(&main, conv, "alice", "a-dev").await;
    seed_commits(&log, conv, 10).await; // head = 11, epochs 0..=10
    let log_conn = log.conn().unwrap();

    // A forged/buggy report claiming an epoch astronomically above the real head.
    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, i64::MAX)
        .await
        .unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv)
        .await
        .unwrap();

    // Clamped to head - 1 = 10; the head commit at epoch 10 is retained.
    assert_eq!(report.floor, 10, "floor is clamped to head - 1, never at/above head");
    assert_eq!(
        epochs(&log, conv).await,
        vec![10],
        "the head commit survives — the live commit log is NOT wiped"
    );
    // The head is unchanged, so the member can still advance from it (no forced
    // external-join, no lost history) — the property the wipe destroyed.
    assert_eq!(
        pollis_delivery::commit::head_epoch(&log_conn, conv).await.unwrap(),
        11,
        "a bogus report must not reset the conversation head"
    );
}

/// #681 no-regression: an HONEST report still prunes exactly as before once the
/// clamp is in place — the clamp only ever narrows pruning for absurd inputs, so
/// a real slowest-member floor well below head is untouched by it.
#[tokio::test]
async fn clamp_does_not_disturb_an_honest_prune() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    seed_commits(&log, conv, 24).await; // head = 25
    let log_conn = log.conn().unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    // Exactly the pre-clamp Tier-1 floor: min(15, 20) - SLACK, well below head.
    assert_eq!(report.floor, 15 - PRUNE_SLACK_EPOCHS);
    let surviving = epochs(&log, conv).await;
    assert_eq!(*surviving.first().unwrap(), 15 - PRUNE_SLACK_EPOCHS);
    assert!(surviving.contains(&24), "the head epoch survives");
}

/// The prune DELETE leaves the UNIQUE(conversation_id, generation, epoch) fork-dedup index
/// intact: a surviving epoch still rejects a duplicate INSERT (no fork), and a
/// pruned epoch is genuinely free.
#[tokio::test]
async fn prune_preserves_unique_epoch_index() {
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";
    seed_commits(&log, conv, 10).await;

    let deleted = delete_commits_below(&log.conn().unwrap(), conv, 0, 5).await.unwrap();
    assert_eq!(deleted, 5, "epochs 0..4 pruned");

    // A duplicate at a SURVIVING epoch must still conflict (fork-dedup holds).
    let dup = log
        .conn()
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, 0, 7, 'u', ?2)",
            libsql::params![conv, vec![7u8]],
        )
        .await
        .unwrap();
    assert_eq!(dup, 0, "a duplicate at a surviving epoch is rejected by the UNIQUE index");
    assert_eq!(epochs(&log, conv).await, vec![5, 6, 7, 8, 9, 10]);
}

/// `record_commit_since` is monotone: a later report at a LOWER epoch never
/// lowers the recorded high-water (which would raise the floor unsafely).
#[tokio::test]
async fn record_commit_since_is_monotone() {
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";
    let conn = log.conn().unwrap();

    record_commit_since(&conn, conv, "alice", "a-dev", 0, 10).await.unwrap();
    // A stale/reordered report at a lower epoch must be ignored.
    record_commit_since(&conn, conv, "alice", "a-dev", 0, 3).await.unwrap();
    // A higher report advances it.
    record_commit_since(&conn, conv, "alice", "a-dev", 0, 14).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT since_epoch FROM mls_commit_since WHERE device_id = 'a-dev'",
            (),
        )
        .await
        .unwrap();
    let got: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(got, 14, "high-water is MAX of reports, never lowered");
}

// ─── Suite generations (#454 P4) ─────────────────────────────────────────────

/// A device still draining the RETIRED lineage is not pruned past by Tier 1. Its
/// reported epoch belongs to generation 0 and says nothing about generation 1, so
/// treating it as a bound on generation 1 would be a category error — it counts
/// as unreported there instead, which disables Tier 1 outright.
#[tokio::test]
async fn a_device_on_the_old_lineage_disables_tier1_on_the_new_one() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    // Generation 0 ran to epoch 24, then the conversation migrated and
    // generation 1 has run to epoch 24 as well.
    seed_commits_in(&log, conv, 0, 24).await;
    seed_commits_in(&log, conv, 1, 24).await;

    let log_conn = log.conn().unwrap();
    // Alice is on the new lineage at epoch 20. Bob never left generation 0 —
    // where he sits at a NUMERICALLY HIGHER epoch than alice.
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 23).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.generation, 1, "the head lineage is the successor");
    assert_eq!(
        report.floor, 0,
        "bob is not IN generation 1, so he is unreported there and Tier 1 must not prune it"
    );
    assert_eq!(
        report.generation_floor, 0,
        "bob still occupies generation 0, so it must not be retired"
    );
    assert_eq!(report.deleted, 0);
    assert_eq!(epochs_in(&log, conv, 0).await.len(), 25, "the retired lineage is intact");
    assert_eq!(epochs_in(&log, conv, 1).await.len(), 25);
}

/// Once EVERY current member device has reported itself in the successor lineage,
/// the retired one is deleted outright — epoch pruning alone would keep it
/// forever, since all its epochs are below its own head and never below the head
/// generation's floor.
#[tokio::test]
async fn a_fully_vacated_lineage_is_retired() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    seed_commits_in(&log, conv, 0, 24).await;
    seed_commits_in(&log, conv, 1, 24).await;

    let log_conn = log.conn().unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 1, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.generation_floor, 1, "generation 0 is fully vacated");
    assert!(
        epochs_in(&log, conv, 0).await.is_empty(),
        "the retired lineage is gone in full — not just a prefix of it"
    );
    // And within the live lineage the ordinary Tier-1 floor still applies.
    assert_eq!(report.floor, 15 - PRUNE_SLACK_EPOCHS);
    let surviving = epochs_in(&log, conv, 1).await;
    assert_eq!(*surviving.first().unwrap(), 15 - PRUNE_SLACK_EPOCHS);
    assert!(
        surviving.contains(&15),
        "the slowest member's applied epoch survives (NoLossForCurrentMember)"
    );
}

/// TIER 2 under generations: a device stranded on a retired lineage does not pin
/// it forever. Once the successor has itself run `PRUNE_MAX_BEHIND_HEAD` epochs
/// the straggler is by construction at least that far behind head — already the
/// accepted-loss condition — so the closed lineage goes and it recovers by
/// external join. The LIVE lineage is never retired, whatever the reports say.
#[tokio::test]
async fn tier2_retires_a_lineage_a_stranded_device_still_occupies() {
    let main = fresh(MAIN_SCHEMA).await;
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    seed_commits_in(&log, conv, 0, 4).await;
    // The successor has run past the hard cap.
    seed_commits_in(&log, conv, 1, PRUNE_MAX_BEHIND_HEAD).await;

    let log_conn = log.conn().unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, PRUNE_MAX_BEHIND_HEAD)
        .await
        .unwrap();
    // Bob never migrated.
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 1).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.generation, 1);
    assert_eq!(report.generation_floor, 1, "Tier 2 retires the closed lineage");
    assert!(epochs_in(&log, conv, 0).await.is_empty());
    assert!(
        !epochs_in(&log, conv, 1).await.is_empty(),
        "the LIVE lineage is never retired wholesale"
    );
}

/// `record_commit_since` is monotone on the PAIR, compared lexicographically. The
/// case that a per-column `MAX` would get wrong: moving to the successor lineage
/// at epoch 0 is a step FORWARD even though the epoch drops, and a late report
/// from the retired lineage at a higher epoch must not drag the device back.
#[tokio::test]
async fn record_commit_since_is_lexicographically_monotone() {
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";
    let conn = log.conn().unwrap();

    record_commit_since(&conn, conv, "alice", "a-dev", 0, 40).await.unwrap();
    // Migrated: generation up, epoch DOWN. This is forward progress.
    record_commit_since(&conn, conv, "alice", "a-dev", 1, 0).await.unwrap();
    // A stale in-flight report from the retired lineage, at a much higher epoch.
    record_commit_since(&conn, conv, "alice", "a-dev", 0, 44).await.unwrap();

    let mut rows = conn
        .query(
            "SELECT generation, since_epoch FROM mls_commit_since WHERE device_id = 'a-dev'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()),
        (1, 0),
        "the stale generation-0 report must not pull the device back off the successor lineage"
    );

    // Forward within the new lineage still advances.
    record_commit_since(&conn, conv, "alice", "a-dev", 1, 3).await.unwrap();
    let mut rows = conn
        .query(
            "SELECT generation, since_epoch FROM mls_commit_since WHERE device_id = 'a-dev'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(
        (row.get::<i64>(0).unwrap(), row.get::<i64>(1).unwrap()),
        (1, 3)
    );
}

/// `delete_generations_below` is scoped: it retires closed lineages and leaves the
/// live one untouched, including its epoch 0 (which an epoch-based DELETE with the
/// same numeric bound would have taken).
#[tokio::test]
async fn delete_generations_below_only_touches_closed_lineages() {
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";
    seed_commits_in(&log, conv, 0, 4).await;
    seed_commits_in(&log, conv, 1, 4).await;

    let deleted = delete_generations_below(&log.conn().unwrap(), conv, 1).await.unwrap();
    assert_eq!(deleted, 5, "generation 0's five commits");
    assert!(epochs_in(&log, conv, 0).await.is_empty());
    assert_eq!(epochs_in(&log, conv, 1).await, vec![0, 1, 2, 3, 4]);

    // Floor 0 is a no-op — there is no closed lineage below generation 0.
    assert_eq!(
        delete_generations_below(&log.conn().unwrap(), conv, 0).await.unwrap(),
        0
    );
}
