//! The harness's local database is NOT encrypted, unlike the shipped client's.
//!
//! The in-process DS brings libsql's sqlite3, which wins the symbols, so `PRAGMA key` is silently ignored.
//! A green flows run is not evidence for at-rest encryption; the `db::local` tests are.
//! Checked on the file: a fresh connection would init sqlite3 and make libsql panic.

use serial_test::serial;

use crate::harness::{wipe, TestClient};

/// A database this harness writes is readable without a key.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_harness_local_database_is_not_encrypted() {
    wipe().await;

    let mut client = TestClient::new().await;
    let profile = client.sign_up("sqlite-probe@test.local").await;

    {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("local db open after sign_up");
        pollis_core::db::local::reclaim(db.conn()).expect("checkpoint");
    }

    let path = client.state.local_data_dir().join(format!("pollis_{}.db", profile.id));
    let bytes = std::fs::read(&path).expect("read local database file");
    assert_eq!(
        &bytes[..16],
        b"SQLite format 3\0",
        "the flows harness's local database is encrypted after all — SQLCipher won the symbols \
         here, which is an improvement and not a failure. Delete this module, and with it the \
         second reason `sqlcipher_is_linked()` exists in pollis-core/src/db/local.rs."
    );

    drop(client);
}
