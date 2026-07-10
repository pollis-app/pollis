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
//!   * The DELETE leaves the UNIQUE(conversation_id, epoch) fork-dedup index
//!     intact (migration 000003, main DB).
//!   * `record_commit_since` is monotone (a stale report can't lower a device's
//!     high-water and prune commits it still needs).

use pollis_delivery::commit::{
    delete_commits_below, prune_commit_log, record_commit_since, PRUNE_SLACK_EPOCHS,
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
  added_device_ids TEXT\
);\
CREATE UNIQUE INDEX idx_mls_commit_conv_epoch ON mls_commit_log (conversation_id, epoch);\
CREATE TABLE mls_commit_since (\
  conversation_id TEXT NOT NULL,\
  user_id TEXT NOT NULL,\
  device_id TEXT NOT NULL,\
  since_epoch INTEGER NOT NULL,\
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),\
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

/// Append commits at epochs `0..=up_to` for `conv`.
async fn seed_commits(log: &Db, conv: &str, up_to: i64) {
    let conn = log.conn().unwrap();
    for e in 0..=up_to {
        conn.execute(
            "INSERT INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, 'u', ?3)",
            libsql::params![conv, e, vec![e as u8]],
        )
        .await
        .unwrap();
    }
}

/// The surviving epochs for `conv`, ascending.
async fn epochs(log: &Db, conv: &str) -> Vec<i64> {
    let conn = log.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_commit_log WHERE conversation_id = ?1 ORDER BY epoch ASC",
            libsql::params![conv],
        )
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(r) = rows.next().await.unwrap() {
        out.push(r.get::<i64>(0).unwrap());
    }
    out
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
    record_commit_since(&log_conn, conv, "alice", "a-dev", 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 15).await.unwrap();

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
    record_commit_since(&log.conn().unwrap(), conv, "alice", "a-dev", 20)
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
    record_commit_since(&log_conn, conv, "alice", "a-old", 2).await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-new", 20).await.unwrap();

    let report = prune_commit_log(&main.conn().unwrap(), &log_conn, conv).await.unwrap();

    // Only the live device (20) counts → floor = 20 - 8 = 12, not 2 - 8.
    assert_eq!(report.floor, 20 - PRUNE_SLACK_EPOCHS);
    assert!(!epochs(&log, conv).await.contains(&2), "the revoked laggard does not hold epoch 2");
}

/// The prune DELETE leaves the UNIQUE(conversation_id, epoch) fork-dedup index
/// intact: a surviving epoch still rejects a duplicate INSERT (no fork), and a
/// pruned epoch is genuinely free.
#[tokio::test]
async fn prune_preserves_unique_epoch_index() {
    let log = fresh(LOG_SCHEMA).await;
    let conv = "grp1";
    seed_commits(&log, conv, 10).await;

    let deleted = delete_commits_below(&log.conn().unwrap(), conv, 5).await.unwrap();
    assert_eq!(deleted, 5, "epochs 0..4 pruned");

    // A duplicate at a SURVIVING epoch must still conflict (fork-dedup holds).
    let dup = log
        .conn()
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO mls_commit_log (conversation_id, epoch, sender_id, commit_data) \
             VALUES (?1, 7, 'u', ?2)",
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

    record_commit_since(&conn, conv, "alice", "a-dev", 10).await.unwrap();
    // A stale/reordered report at a lower epoch must be ignored.
    record_commit_since(&conn, conv, "alice", "a-dev", 3).await.unwrap();
    // A higher report advances it.
    record_commit_since(&conn, conv, "alice", "a-dev", 14).await.unwrap();

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
