//! Commit-log retention floor (#539, I4), with membership on MAIN and the log on LOG, as in production.

use pollis_delivery::commit::{
    delete_commits_below, delete_generations_below, prune_commit_log, record_commit_since,
    PRUNE_MAX_BEHIND_HEAD, PRUNE_SLACK_EPOCHS,
};
use pollis_delivery::db::Db;

mod common;

/// Which half of the #420 split a handle is, so a misrouted query is a type error.
#[derive(Clone, Copy)]
enum Half {
    Main,
    Log,
}

async fn fresh(half: Half) -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    match half {
        Half::Main => pollis_schema::apply::main_db(&conn).await.expect("schema"),
        Half::Log => pollis_schema::apply::log_db(&conn).await.expect("schema"),
    }
    db
}

async fn add_member(main: &Db, group_id: &str, user_id: &str, device_id: &str) {
    let conn = main.conn().await.unwrap();
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
    main.conn().await
        .unwrap()
        .execute(
            "UPDATE user_device SET revoked_at = datetime('now') WHERE device_id = ?1",
            libsql::params![device_id],
        )
        .await
        .unwrap();
}

async fn seed_commits_in(log: &Db, conv: &str, gen: i64, up_to: i64) {
    let conn = log.conn().await.unwrap();
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

/// Generation 0: the lineage every pre-P4 conversation is in.
async fn seed_commits(log: &Db, conv: &str, up_to: i64) {
    seed_commits_in(log, conv, 0, up_to).await;
}

async fn epochs_in(log: &Db, conv: &str, gen: i64) -> Vec<i64> {
    let conn = log.conn().await.unwrap();
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

async fn epochs(log: &Db, conv: &str) -> Vec<i64> {
    epochs_in(log, conv, 0).await
}

/// TIER 1: the slowest current member pins the floor.
#[tokio::test]
async fn tier1_prunes_prefix_but_keeps_slowest_member() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    seed_commits(&log, conv, 24).await;
    let log_conn = log.conn().await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

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

/// An unreported member disables Tier 1.
#[tokio::test]
async fn unreported_member_blocks_tier1_pruning() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    seed_commits(&log, conv, 24).await;
    record_commit_since(&log.conn().await.unwrap(), conv, "alice", "a-dev", 0, 20)
        .await
        .unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log.conn().await.unwrap(), conv)
        .await
        .unwrap();

    assert_eq!(report.floor, 0, "an unreported member keeps the floor at 0");
    assert_eq!(report.deleted, 0, "nothing pruned while a member is unaccounted for");
    assert_eq!(epochs(&log, conv).await.len(), 25);
}

/// A revoked device never pins the floor (it can't rejoin — I5).
#[tokio::test]
async fn revoked_device_does_not_pin_the_floor() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-old").await;
    main.conn().await
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) VALUES ('alice', 'a-new', NULL)",
            (),
        )
        .await
        .unwrap();
    revoke_device(&main, "a-old").await;

    seed_commits(&log, conv, 24).await;
    let log_conn = log.conn().await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-old", 0, 2).await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-new", 0, 20).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.floor, 20 - PRUNE_SLACK_EPOCHS);
    assert!(!epochs(&log, conv).await.contains(&2), "the revoked laggard does not hold epoch 2");
}

/// #681: a high-water far above head is clamped to `head - 1`, so the head survives.
#[tokio::test]
async fn bogus_high_water_above_head_cannot_wipe_the_live_log() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    // Single member, so the bogus report is the roster minimum: the worst case.
    add_member(&main, conv, "alice", "a-dev").await;
    seed_commits(&log, conv, 10).await; // head = 11, epochs 0..=10
    let log_conn = log.conn().await.unwrap();

    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, i64::MAX)
        .await
        .unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv)
        .await
        .unwrap();

    assert_eq!(report.floor, 10, "floor is clamped to head - 1, never at/above head");
    assert_eq!(
        epochs(&log, conv).await,
        vec![10],
        "the head commit survives — the live commit log is NOT wiped"
    );
    assert_eq!(
        pollis_delivery::commit::head_epoch(&log_conn, conv).await.unwrap(),
        11,
        "a bogus report must not reset the conversation head"
    );
}

/// #681: an honest report still prunes exactly as before.
#[tokio::test]
async fn clamp_does_not_disturb_an_honest_prune() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;
    seed_commits(&log, conv, 24).await; // head = 25
    let log_conn = log.conn().await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 0, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.floor, 15 - PRUNE_SLACK_EPOCHS);
    let surviving = epochs(&log, conv).await;
    assert_eq!(*surviving.first().unwrap(), 15 - PRUNE_SLACK_EPOCHS);
    assert!(surviving.contains(&24), "the head epoch survives");
}

/// The prune leaves the UNIQUE fork-dedup index intact.
#[tokio::test]
async fn prune_preserves_unique_epoch_index() {
    let log = fresh(Half::Log).await;
    let conv = "grp1";
    seed_commits(&log, conv, 10).await;

    let deleted = delete_commits_below(&log.conn().await.unwrap(), conv, 0, 5).await.unwrap();
    assert_eq!(deleted, 5, "epochs 0..4 pruned");

    let dup = log
        .conn().await
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

/// `record_commit_since` is monotone: a lower report never lowers the high-water.
#[tokio::test]
async fn record_commit_since_is_monotone() {
    let log = fresh(Half::Log).await;
    let conv = "grp1";
    let conn = log.conn().await.unwrap();

    record_commit_since(&conn, conv, "alice", "a-dev", 0, 10).await.unwrap();
    record_commit_since(&conn, conv, "alice", "a-dev", 0, 3).await.unwrap();
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

/// A retired-lineage report counts as unreported on the new lineage, disabling Tier 1.
#[tokio::test]
async fn a_device_on_the_old_lineage_disables_tier1_on_the_new_one() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    seed_commits_in(&log, conv, 0, 24).await;
    seed_commits_in(&log, conv, 1, 24).await;

    let log_conn = log.conn().await.unwrap();
    // Bob stays on generation 0, at a numerically higher epoch than alice.
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 23).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

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

/// Once every device reports in the successor, the retired lineage is deleted outright.
#[tokio::test]
async fn a_fully_vacated_lineage_is_retired() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    seed_commits_in(&log, conv, 0, 24).await;
    seed_commits_in(&log, conv, 1, 24).await;

    let log_conn = log.conn().await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, 20).await.unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 1, 15).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.generation_floor, 1, "generation 0 is fully vacated");
    assert!(
        epochs_in(&log, conv, 0).await.is_empty(),
        "the retired lineage is gone in full — not just a prefix of it"
    );
    assert_eq!(report.floor, 15 - PRUNE_SLACK_EPOCHS);
    let surviving = epochs_in(&log, conv, 1).await;
    assert_eq!(*surviving.first().unwrap(), 15 - PRUNE_SLACK_EPOCHS);
    assert!(
        surviving.contains(&15),
        "the slowest member's applied epoch survives (NoLossForCurrentMember)"
    );
}

/// TIER 2: a straggler on a retired lineage stops pinning it; the live lineage never goes.
#[tokio::test]
async fn tier2_retires_a_lineage_a_stranded_device_still_occupies() {
    let main = fresh(Half::Main).await;
    let log = fresh(Half::Log).await;
    let conv = "grp1";

    add_member(&main, conv, "alice", "a-dev").await;
    add_member(&main, conv, "bob", "b-dev").await;

    seed_commits_in(&log, conv, 0, 4).await;
    seed_commits_in(&log, conv, 1, PRUNE_MAX_BEHIND_HEAD).await;

    let log_conn = log.conn().await.unwrap();
    record_commit_since(&log_conn, conv, "alice", "a-dev", 1, PRUNE_MAX_BEHIND_HEAD)
        .await
        .unwrap();
    record_commit_since(&log_conn, conv, "bob", "b-dev", 0, 1).await.unwrap();

    let report = prune_commit_log(&main.conn().await.unwrap(), &log_conn, conv).await.unwrap();

    assert_eq!(report.generation, 1);
    assert_eq!(report.generation_floor, 1, "Tier 2 retires the closed lineage");
    assert!(epochs_in(&log, conv, 0).await.is_empty());
    assert!(
        !epochs_in(&log, conv, 1).await.is_empty(),
        "the LIVE lineage is never retired wholesale"
    );
}

/// Monotone on the (generation, epoch) pair: a migration's epoch drop is forward progress.
#[tokio::test]
async fn record_commit_since_is_lexicographically_monotone() {
    let log = fresh(Half::Log).await;
    let conv = "grp1";
    let conn = log.conn().await.unwrap();

    record_commit_since(&conn, conv, "alice", "a-dev", 0, 40).await.unwrap();
    record_commit_since(&conn, conv, "alice", "a-dev", 1, 0).await.unwrap();
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

/// `delete_generations_below` leaves the live lineage untouched, epoch 0 included.
#[tokio::test]
async fn delete_generations_below_only_touches_closed_lineages() {
    let log = fresh(Half::Log).await;
    let conv = "grp1";
    seed_commits_in(&log, conv, 0, 4).await;
    seed_commits_in(&log, conv, 1, 4).await;

    let deleted = delete_generations_below(&log.conn().await.unwrap(), conv, 1).await.unwrap();
    assert_eq!(deleted, 5, "generation 0's five commits");
    assert!(epochs_in(&log, conv, 0).await.is_empty());
    assert_eq!(epochs_in(&log, conv, 1).await, vec![0, 1, 2, 3, 4]);

    assert_eq!(
        delete_generations_below(&log.conn().await.unwrap(), conv, 0).await.unwrap(),
        0
    );
}

#[tokio::test]
async fn a_duplicate_membership_row_is_rejected() {
    let main = fresh(Half::Main).await;
    add_member(&main, "g", "alice", "a1").await;

    let err = main
        .conn().await
        .unwrap()
        .execute(
            "INSERT INTO group_member (group_id, user_id) VALUES ('g', 'alice')",
            (),
        )
        .await
        .expect_err("PRIMARY KEY (group_id, user_id) must reject a second row");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a uniqueness failure, got: {err}"
    );

    let conn = main.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g' AND user_id = 'alice'",
            (),
        )
        .await
        .unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap(), 1);
}

/// `user_device.device_id` alone is the key, so no device is counted twice.
#[tokio::test]
async fn a_device_id_cannot_belong_to_two_users() {
    let main = fresh(Half::Main).await;
    add_member(&main, "g", "alice", "shared-device").await;

    let err = main
        .conn().await
        .unwrap()
        .execute(
            "INSERT INTO user_device (user_id, device_id) VALUES ('mallory', 'shared-device')",
            (),
        )
        .await
        .expect_err("device_id is the primary key — one device, one user");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a uniqueness failure, got: {err}"
    );
}

/// Two high-water rows for one device would let MIN pick the stale one.
#[tokio::test]
async fn a_device_cannot_report_two_high_waters_for_one_conversation() {
    let log = fresh(Half::Log).await;
    let conn = log.conn().await.unwrap();
    conn.execute(
        "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, since_epoch) \
         VALUES ('c', 'alice', 'a1', 7)",
        (),
    )
    .await
    .unwrap();
    let err = conn
        .execute(
            "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, since_epoch) \
             VALUES ('c', 'alice', 'a1', 1)",
            (),
        )
        .await
        .expect_err("PRIMARY KEY (conversation_id, user_id, device_id) must reject a second row");
    assert!(
        err.to_string().to_lowercase().contains("unique"),
        "expected a uniqueness failure, got: {err}"
    );
}
