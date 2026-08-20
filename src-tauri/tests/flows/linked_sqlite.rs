//! What this harness's local database actually is — which is not what the
//! shipped client's is.
//!
//! `pollis-core` links SQLCipher and nothing else, so `pollis_{user_id}.db` is
//! encrypted at rest on every platform (#992, and the `db::local` tests that
//! read the bytes back to prove it). **This test binary is different.** It runs
//! the Delivery Service in-process, which brings `libsql` and therefore a
//! SECOND sqlite3 amalgamation; the linker gives that one the `sqlite3_*`
//! symbols, SQLCipher's object is never pulled, and `PRAGMA key` becomes an
//! unknown pragma — which SQLite ignores silently. The clients in this suite
//! consequently run on an unencrypted local database.
//!
//! That is a property of the harness, not of the product, and it costs nothing
//! for what the harness is for (MLS state machines, delivery, membership). It
//! is pinned here for two reasons:
//!
//! 1. So nobody reads a green flows run as evidence that at-rest encryption
//!    works. It is not evidence either way;
//!    `db::local::tests::the_local_database_file_is_encrypted_at_rest` is.
//! 2. Because `db::local::destroy_plaintext_database` keys off exactly this
//!    condition. It destroys a legacy plaintext database only in a build that
//!    will replace it with an encrypted one; here it would loop forever,
//!    zeroing a file two simulated devices of the same user share, out from
//!    under whichever of them still has it open.
//!
//! If `libsql` ever leaves this binary, this test fails — and that is good
//! news: delete it, and the harness starts covering the encrypted path.
//!
//! Asserted on the FILE rather than by asking a fresh connection for
//! `PRAGMA cipher_version`: opening one runs `sqlite3_initialize()`, and libsql
//! calls `sqlite3_config` on first use and panics if sqlite3 is already
//! initialised (see `.codesight/wiki/testing.md`). Going through `sign_up`
//! keeps this behind the harness's own ordering.

use serial_test::serial;

use crate::harness::{wipe, TestClient};

/// A database this harness writes is readable without a key.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_harness_local_database_is_not_encrypted() {
    wipe().await;

    let mut client = TestClient::new().await;
    let profile = client.sign_up("sqlite-probe@test.local").await;

    // Push what the client has written out to the file before reading it.
    {
        let guard = client.state.local_db.lock().await;
        let db = guard.as_ref().expect("local db open after sign_up");
        pollis_core::db::local::reclaim(db.conn()).expect("checkpoint");
    }

    let path = pollis_core::db::local::dirs_path().join(format!("pollis_{}.db", profile.id));
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
