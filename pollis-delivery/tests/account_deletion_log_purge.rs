//! #1883: account deletion's commit-log half is recorded in the deletion
//! transaction, so a failure after the main commit leaves a record the next
//! run finishes rather than rows no request can reach.

use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use libsql::Connection;
use ml_dsa::Keypair as _;
use pollis_api::account::DeleteAccountBody;
use pollis_delivery::account::apply_delete_account;
use pollis_delivery::teardown::{drain_account_log_purges, finish_account_log_purge};
use pollis_delivery::util::SingleFlight;
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::AppState;

mod common;

async fn open(log: bool) -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
    if log {
        pollis_schema::apply::log_db(&c).await.unwrap();
    } else {
        pollis_schema::apply::main_db(&c).await.unwrap();
    }
    c
}

async fn exec(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut rows = c.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// The production shape: a main DB and a separate commit-log DB. `victim` is
/// the sole member of `solo_g`; `bystander` has a Welcome of its own.
async fn seeded() -> (Connection, Connection) {
    let main = open(false).await;
    for sql in [
        "INSERT INTO users (id, email, username) VALUES ('victim', 'v@x.com', 'victim')",
        "INSERT INTO user_device (device_id, user_id) VALUES ('victim_dev', 'victim')",
        "INSERT INTO conversation (id, kind) VALUES ('solo_g', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('solo_g', 'G', 'victim')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('solo_g', 'victim', 'admin')",
    ] {
        exec(&main, sql).await;
    }
    let log = open(true).await;
    for sql in [
        "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
         VALUES ('w1', 'other_g', 'victim', 'victim_dev', x'00')",
        "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
         VALUES ('w2', 'other_g', 'bystander', 'b_dev', x'00')",
        "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, since_epoch) \
         VALUES ('other_g', 'victim', 'victim_dev', 3)",
        "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
         VALUES ('solo_g', 1, x'00', 'victim_dev')",
    ] {
        exec(&log, sql).await;
    }
    (main, log)
}

async fn delete_victim(main: &Connection) {
    let (outcome, _) = apply_delete_account(
        main,
        Some("victim"),
        &DeleteAccountBody { user_id: Some("victim".into()), withdraw: Vec::new() },
    )
    .await
    .unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
}

/// What the commit-log half removes, still present.
async fn log_rows_naming_victim(log: &Connection) -> i64 {
    count(log, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'victim'").await
        + count(log, "SELECT COUNT(*) FROM mls_commit_since WHERE user_id = 'victim'").await
        + count(log, "SELECT COUNT(*) FROM mls_group_info WHERE conversation_id = 'solo_g'").await
}

async fn owed(main: &Connection) -> i64 {
    count(main, "SELECT COUNT(*) FROM account_log_purge").await
}

#[tokio::test]
async fn the_deletion_transaction_records_its_commit_log_half() {
    let (main, log) = seeded().await;
    delete_victim(&main).await;
    assert_eq!(owed(&main).await, 1, "the commit-log half is owed once the main commit lands");
    assert_eq!(log_rows_naming_victim(&log).await, 3);

    assert!(finish_account_log_purge(&main, &log, "victim").await.unwrap());
    assert_eq!(log_rows_naming_victim(&log).await, 0);
    assert_eq!(owed(&main).await, 0, "the record goes with the half it described");
    assert_eq!(
        count(&log, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'bystander'").await,
        1,
        "another account's Welcome is untouched"
    );
    assert!(
        !finish_account_log_purge(&main, &log, "victim").await.unwrap(),
        "nothing is owed twice"
    );
}

/// The commit-log DB fails after the main commit. The deleted account's devices
/// are gone, so no retry of the request can authenticate; the record survives
/// and the next drain (the next deletion, or a DS start) leaves nothing behind.
#[tokio::test]
async fn a_failed_commit_log_half_stays_owed_and_the_next_drain_finishes_it() {
    let (main, log) = seeded().await;
    delete_victim(&main).await;

    exec(
        &log,
        "CREATE TRIGGER block_welcome_delete BEFORE DELETE ON mls_welcome \
         BEGIN SELECT RAISE(ABORT, 'log db unavailable'); END",
    )
    .await;
    assert!(
        finish_account_log_purge(&main, &log, "victim").await.is_err(),
        "the failure surfaces, so the request fails"
    );
    assert_eq!(owed(&main).await, 1, "a failed half stays owed");
    assert_eq!(count(&main, "SELECT COUNT(*) FROM users WHERE id = 'victim'").await, 0);
    assert_eq!(
        drain_account_log_purges(&main, &log).await.unwrap(),
        0,
        "a drain while the log DB still fails runs nothing and keeps the record"
    );
    assert_eq!(owed(&main).await, 1);

    exec(&log, "DROP TRIGGER block_welcome_delete").await;
    assert_eq!(drain_account_log_purges(&main, &log).await.unwrap(), 1);
    assert_eq!(log_rows_naming_victim(&log).await, 0, "nothing naming the account is left");
    assert_eq!(owed(&main).await, 0);
}

/// A record whose id lists cannot be read still runs the account-scoped part
/// and is deleted, rather than failing every drain.
#[tokio::test]
async fn an_unreadable_record_still_runs_and_goes() {
    let (main, log) = seeded().await;
    exec(
        &main,
        "INSERT INTO account_log_purge (user_id, scrub_account, device_ids, dead_conversations) \
         VALUES ('victim', 1, 'not json', '[1')",
    )
    .await;
    assert!(finish_account_log_purge(&main, &log, "victim").await.unwrap());
    assert_eq!(owed(&main).await, 0);
    assert_eq!(
        count(&log, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'victim'").await
            + count(&log, "SELECT COUNT(*) FROM mls_commit_since WHERE user_id = 'victim'").await,
        0,
        "the account-scoped rows still go"
    );
}

/// `POST /v1/account/delete` with a failing commit-log DB: the account is gone
/// and the half is recorded, so the request succeeds (a retry could no longer
/// sign) and the record waits for the next drain.
#[tokio::test(flavor = "multi_thread")]
async fn the_deletion_request_succeeds_when_the_commit_log_half_fails() {
    let main = common::TempDb::open("main.db").await;
    pollis_schema::apply::main_db(&main.conn().await.unwrap()).await.unwrap();
    let log = common::TempDb::open("log.db").await;
    pollis_schema::apply::log_db(&log.conn().await.unwrap()).await.unwrap();
    let state = AppState::new_with_log_db(main.arc(), log.arc(), true);

    let sk = common::gen_signing_key();
    let main_conn = main.conn().await.unwrap();
    exec(&main_conn, "INSERT INTO users (id, email, username) VALUES ('victim', 'v@x.com', 'victim')").await;
    main_conn
        .execute(
            "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES ('victim_dev', 'victim', ?1)",
            libsql::params![sk.verifying_key().encode().to_vec()],
        )
        .await
        .unwrap();
    let log_conn = log.conn().await.unwrap();
    exec(
        &log_conn,
        "INSERT INTO mls_welcome (id, conversation_id, recipient_id, recipient_device_id, welcome_data) \
         VALUES ('w1', 'other_g', 'victim', 'victim_dev', x'00')",
    )
    .await;
    exec(
        &log_conn,
        "CREATE TRIGGER block_welcome_delete BEFORE DELETE ON mls_welcome \
         BEGIN SELECT RAISE(ABORT, 'log db unavailable'); END",
    )
    .await;

    let (s, body) = common::send_signed(
        &state,
        "/v1/account/delete",
        serde_json::json!({}),
        "victim",
        "victim_dev",
        &sk,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "the deletion committed, so the request succeeds: {body}");
    assert_eq!(count(&main_conn, "SELECT COUNT(*) FROM users WHERE id = 'victim'").await, 0);
    assert_eq!(owed(&main_conn).await, 1, "the commit-log half stays owed");

    exec(&log_conn, "DROP TRIGGER block_welcome_delete").await;
    // The request also kicked a background drain; either may be the one that runs it.
    drain_account_log_purges(&main_conn, &log_conn).await.unwrap();
    assert_eq!(count(&log_conn, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'victim'").await, 0);
    assert_eq!(owed(&main_conn).await, 0);
}

/// Overlapping kicks never run the drain twice at once, and the last kick is
/// always served by a run that starts after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn drains_run_one_at_a_time_and_never_miss_a_kick() {
    let gate = SingleFlight::default();
    let kicks = Arc::new(AtomicUsize::new(0));
    let active = Arc::new(AtomicUsize::new(0));
    let max_active = Arc::new(AtomicUsize::new(0));
    let runs = Arc::new(AtomicUsize::new(0));
    let last_seen = Arc::new(AtomicUsize::new(0));

    let mut kickers = Vec::new();
    for _ in 0..8 {
        let gate = gate.clone();
        let (kicks, active, max_active, runs, last_seen) =
            (kicks.clone(), active.clone(), max_active.clone(), runs.clone(), last_seen.clone());
        kickers.push(tokio::spawn(async move {
            for _ in 0..10 {
                kicks.fetch_add(1, SeqCst);
                let (kicks, active, max_active, runs, last_seen) =
                    (kicks.clone(), active.clone(), max_active.clone(), runs.clone(), last_seen.clone());
                gate.kick(move || {
                    let (kicks, active, max_active, runs, last_seen) =
                        (kicks.clone(), active.clone(), max_active.clone(), runs.clone(), last_seen.clone());
                    async move {
                        let now = active.fetch_add(1, SeqCst) + 1;
                        max_active.fetch_max(now, SeqCst);
                        last_seen.store(kicks.load(SeqCst), SeqCst);
                        tokio::time::sleep(Duration::from_millis(2)).await;
                        active.fetch_sub(1, SeqCst);
                        runs.fetch_add(1, SeqCst);
                    }
                });
                tokio::time::sleep(Duration::from_micros(300)).await;
            }
        }));
    }
    for k in kickers {
        k.await.unwrap();
    }
    // Let the last run finish.
    let mut stable = 0;
    let mut prev = usize::MAX;
    while stable < 20 {
        tokio::time::sleep(Duration::from_millis(10)).await;
        let now = runs.load(SeqCst);
        if now == prev && active.load(SeqCst) == 0 {
            stable += 1;
        } else {
            stable = 0;
        }
        prev = now;
    }

    assert_eq!(max_active.load(SeqCst), 1, "two drains ran at once");
    assert!(runs.load(SeqCst) >= 1);
    assert!(runs.load(SeqCst) < 80, "overlapping kicks share a run");
    assert_eq!(last_seen.load(SeqCst), 80, "a run started after the last kick");
}
