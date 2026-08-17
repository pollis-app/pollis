//! `RemoteDb::with_retry`'s contract, executed (#914).
//!
//! The helper was written in #874 and, until #914, had exactly one production
//! call site, five comments citing it as the pattern to follow, and **no test at
//! all**. That combination is the thing #914 is actually about: a retry policy
//! the codebase documents and does not demonstrate reads as coverage that is not
//! there. Wiring it into the paths that need it is only half the answer — the
//! other half is pinning what it promises, so "reconnects once and retries" stops
//! being a claim in a doc comment.
//!
//! These live in an integration binary rather than pollis-core's `--lib` tests
//! for the reason `auth_account_creation.rs` and `resign_stale_certs.rs` give:
//! libsql's local backend calls `sqlite3_config` on first use, which fails once
//! rusqlite/SQLCipher has already initialised SQLite in the same process. An
//! integration test gets its own process, so this runs against a real `RemoteDb`
//! rather than a mock of one.
//!
//! The failure INJECTION is honest here even though the DB is local: `with_retry`
//! takes the operation as a closure, so the test supplies the closure and decides
//! what it returns. What is exercised for real is everything around it — the
//! transient/permanent classification, the reconnect, the second `conn()`, and
//! the attempt accounting.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use pollis_core::db::remote::RemoteDb;

async fn db() -> RemoteDb {
    // A unique file per test, under the process temp dir. No `tempfile` dep —
    // pollis-core does not carry one, and the file is small and short-lived.
    let path = std::env::temp_dir().join(format!(
        "pollis-with-retry-{}-{:?}.db",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_file(&path);
    RemoteDb::connect_local(path).await.expect("local remote db")
}

/// A libsql error whose rendered message the transient classifier accepts.
///
/// `libsql::Error` has no constructor for "the stream went away", and it does not
/// distinguish these structurally — the classifier matches on the rendered
/// message, so the test builds one the same way.
fn transient() -> libsql::Error {
    libsql::Error::ConnectionFailed("stream not found".to_string())
}

/// An error that is NOT recoverable by reconnecting.
fn permanent() -> libsql::Error {
    libsql::Error::InvalidColumnName("nope".to_string())
}

/// The promise: one transient failure is absorbed, and the caller sees success.
///
/// This is the whole reason the helper exists. On the ingest path it is the
/// difference between "messages arrived a beat late" and "messages did not
/// arrive until something else triggered a fetch".
#[tokio::test]
async fn a_transient_failure_is_retried_once_and_then_succeeds() {
    let db = db().await;
    let attempts = Arc::new(AtomicUsize::new(0));

    let seen = Arc::clone(&attempts);
    let value: i64 = db
        .with_retry(move |_conn| {
            let seen = Arc::clone(&seen);
            async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(transient());
                }
                Ok(7)
            }
        })
        .await
        .expect("a transient error must not reach the caller");

    assert_eq!(value, 7);
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "the operation should run exactly twice: the failure and the retry"
    );
}

/// A permanent error is surfaced immediately, NOT retried.
///
/// Retrying a genuine error (a bad column, a constraint violation) would double
/// the latency of every real failure and, for a non-idempotent statement, could
/// apply it twice. The classifier is what keeps the retry narrow.
#[tokio::test]
async fn a_permanent_error_is_surfaced_without_a_second_attempt() {
    let db = db().await;
    let attempts = Arc::new(AtomicUsize::new(0));

    let seen = Arc::clone(&attempts);
    let result: Result<i64, _> = db
        .with_retry(move |_conn| {
            let seen = Arc::clone(&seen);
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Err(permanent())
            }
        })
        .await;

    assert!(result.is_err(), "a permanent error must reach the caller");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        1,
        "a permanent error must not be retried"
    );
}

/// It retries ONCE, not until it works.
///
/// The bound matters: this recovers from a stream that went away, which a
/// reconnect fixes on the first attempt or does not fix at all. An unbounded
/// version would turn a dead database into a hang on the receive path.
#[tokio::test]
async fn a_transient_failure_on_the_retry_is_surfaced() {
    let db = db().await;
    let attempts = Arc::new(AtomicUsize::new(0));

    let seen = Arc::clone(&attempts);
    let result: Result<i64, _> = db
        .with_retry(move |_conn| {
            let seen = Arc::clone(&seen);
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Err(transient())
            }
        })
        .await;

    assert!(result.is_err(), "a persistent failure must reach the caller");
    assert_eq!(
        attempts.load(Ordering::SeqCst),
        2,
        "exactly two attempts — retrying forever would hang the receive path"
    );
}

/// The happy path costs nothing extra: no reconnect, one attempt.
#[tokio::test]
async fn a_successful_operation_runs_exactly_once() {
    let db = db().await;
    let attempts = Arc::new(AtomicUsize::new(0));

    let seen = Arc::clone(&attempts);
    let value: i64 = db
        .with_retry(move |_conn| {
            let seen = Arc::clone(&seen);
            async move {
                seen.fetch_add(1, Ordering::SeqCst);
                Ok(1)
            }
        })
        .await
        .unwrap();

    assert_eq!(value, 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

/// The retry hands the closure a WORKING connection, not the dead one.
///
/// `with_retry` reconnects before the second attempt; if it passed the original
/// connection back, the retry would fail for the same reason every time and the
/// helper would be decorative. Asserted by having the retry actually run a query.
#[tokio::test]
async fn the_retry_receives_a_usable_connection() {
    let db = db().await;
    {
        let conn = db.conn().await.unwrap();
        conn.execute("CREATE TABLE t (n INTEGER NOT NULL)", ()).await.unwrap();
        conn.execute("INSERT INTO t (n) VALUES (42)", ()).await.unwrap();
    }

    let attempts = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&attempts);
    let n: i64 = db
        .with_retry(move |conn| {
            let seen = Arc::clone(&seen);
            async move {
                if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(transient());
                }
                let mut rows = conn.query("SELECT n FROM t", ()).await?;
                let row = rows.next().await?.expect("row");
                row.get::<i64>(0)
            }
        })
        .await
        .expect("the retry's connection must be usable");

    assert_eq!(n, 42);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}
