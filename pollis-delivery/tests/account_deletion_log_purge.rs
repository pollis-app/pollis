//! #1883: account deletion's commit-log half is recorded in the deletion
//! transaction, so a failure after the main commit leaves a record the next
//! run finishes rather than rows no request can reach.

use libsql::Connection;
use pollis_api::account::DeleteAccountBody;
use pollis_delivery::account::apply_delete_account;
use pollis_delivery::teardown::{drain_account_log_purges, finish_account_log_purge};
use pollis_delivery::writes::WriteOutcome;

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
