//! #679 — a revoked device must not count as registered.
//!
//! These live in an integration binary rather than pollis-core's `--lib` tests
//! deliberately. libsql's local backend calls `sqlite3_config` on first use,
//! which returns `SQLITE_MISUSE` (21) once rusqlite/SQLCipher — which `LocalDb`
//! uses — has already run `sqlite3_initialize` in the same process. Any lib test
//! that opens a local libsql DB therefore panics as soon as it shares a test
//! binary with a test that touches the local DB. An integration test gets its own
//! process and never loads rusqlite, so the real query runs against a real DB.
//!
//! The pure set-algebra half of the #679 invariant (`desired_set`, which covers
//! the KeyPackage/addition path) needs no DB and stays in the lib tests.

use std::collections::HashSet;

use pollis_core::commands::mls::registered_devices;

/// The `user_device` shape `registered_devices` reads, including the #372
/// tombstone column added by migration 000004.
async fn db_with_devices(rows: &[(&str, &str, Option<&str>)]) -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .expect("in-memory libsql");
    let conn = db.connect().expect("connect");
    // The SHIPPED remote schema (#875) rather than a hand-rolled `user_device`.
    for sql in pollis_schema::main_scripts() {
        conn.execute_batch(sql).await.expect("schema");
    }
    for (user_id, _, _) in rows {
        // `user_device.user_id` is a real foreign key; libsql's local backend
        // enforces it. Give each device an owner.
        conn.execute(
            "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
            libsql::params![user_id.to_string()],
        )
        .await
        .expect("seed user");
    }
    for (user_id, device_id, revoked_at) in rows {
        conn.execute(
            "INSERT INTO user_device (device_id, user_id, revoked_at) VALUES (?1, ?2, ?3)",
            libsql::params![
                device_id.to_string(),
                user_id.to_string(),
                revoked_at.map(|s| s.to_string())
            ],
        )
        .await
        .expect("insert device");
    }
    conn
}

fn roster(ids: &[&str]) -> HashSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

/// The #679 regression itself. `revoke_device` tombstones the row rather
/// than deleting it, so a revoked device must be excluded by the predicate,
/// not by the row's absence. Before the fix this returned BOTH devices,
/// which left the revoked leaf in `desired` and so out of `to_remove`.
#[tokio::test]
async fn revoked_device_is_not_registered() {
    let conn = db_with_devices(&[
        ("alice", "live-device", None),
        ("alice", "revoked-device", Some("2026-07-30 12:00:00")),
    ])
    .await;

    let got = registered_devices(&conn, &roster(&["alice"]))
        .await
        .unwrap();

    assert!(
        got.contains(&("alice".to_string(), "live-device".to_string())),
        "a non-revoked device must stay registered; got {got:?}"
    );
    assert!(
        !got.contains(&("alice".to_string(), "revoked-device".to_string())),
        "REVOCATION LEAK (#679): a tombstoned device is still reported as registered, so \
         reconcile keeps its leaf and it goes on decrypting. got {got:?}"
    );
    assert_eq!(got.len(), 1, "exactly the live device; got {got:?}");
}

/// Revocation is per-device, not per-user: tombstoning one device must not
/// evict its owner's other devices, and must not spare other users'.
#[tokio::test]
async fn mixed_users_yield_only_live_devices() {
    let conn = db_with_devices(&[
        ("alice", "a-live-1", None),
        ("alice", "a-live-2", None),
        ("alice", "a-dead", Some("2026-07-30 12:00:00")),
        ("bob", "b-dead", Some("2026-07-30 12:00:00")),
        ("carol", "c-live", None),
        // Not on the roster at all — must never appear.
        ("dave", "d-live", None),
    ])
    .await;

    let got = registered_devices(&conn, &roster(&["alice", "bob", "carol"]))
        .await
        .unwrap();

    let mut want: HashSet<(String, String)> = HashSet::new();
    want.insert(("alice".into(), "a-live-1".into()));
    want.insert(("alice".into(), "a-live-2".into()));
    want.insert(("carol".into(), "c-live".into()));
    assert_eq!(got, want, "only live devices of rostered users");
}

/// A user whose every device is revoked contributes nothing — but is not an
/// error. Reconcile must then treat all of their leaves as removable.
#[tokio::test]
async fn fully_revoked_user_contributes_no_devices() {
    let conn = db_with_devices(&[
        ("alice", "a1", Some("2026-07-30 12:00:00")),
        ("alice", "a2", Some("2026-07-30 12:00:00")),
    ])
    .await;

    let got = registered_devices(&conn, &roster(&["alice"]))
        .await
        .unwrap();
    assert!(
        got.is_empty(),
        "every device revoked ⇒ no registered devices; got {got:?}"
    );
}

/// Revoke-then-re-enrol: the replacement device is a NEW `device_id`, so the
/// old leaf must be droppable while the new one is admitted. Getting this
/// backwards would lock a user out of their own re-enrolled device.
#[tokio::test]
async fn reenrolled_device_replaces_the_revoked_one() {
    let conn = db_with_devices(&[
        ("alice", "old-device", Some("2026-07-30 12:00:00")),
        ("alice", "new-device", None),
    ])
    .await;

    let got = registered_devices(&conn, &roster(&["alice"]))
        .await
        .unwrap();
    assert_eq!(
        got,
        HashSet::from([("alice".to_string(), "new-device".to_string())]),
        "only the re-enrolled device; got {got:?}"
    );
}

/// The ids are BOUND, not interpolated. Previously they were spliced into
/// the SQL behind a character filter that silently *mangled* anything else —
/// a user_id containing a quote became a different id, so the query answered
/// a question nobody asked. Binding makes it match exactly or not at all,
/// which is what makes the `revoked_at IS NULL` predicate trustworthy.
#[tokio::test]
async fn ids_are_bound_not_interpolated() {
    let weird = "alice' OR '1'='1";
    let conn = db_with_devices(&[
        (weird, "weird-live", None),
        (weird, "weird-dead", Some("2026-07-30 12:00:00")),
        ("bob", "b-live", None),
    ])
    .await;

    let got = registered_devices(&conn, &roster(&[weird])).await.unwrap();

    assert_eq!(
        got,
        HashSet::from([(weird.to_string(), "weird-live".to_string())]),
        "an id with a quote must match itself exactly — and still honour the \
         revocation filter — never widen the query. got {got:?}"
    );
}

/// An empty roster must not build `IN ()` (a syntax error) nor fall through
/// to an unfiltered scan that would report every device in the table.
#[tokio::test]
async fn empty_roster_short_circuits() {
    let conn = db_with_devices(&[("alice", "a1", None)]).await;
    let got = registered_devices(&conn, &roster(&[])).await.unwrap();
    assert!(got.is_empty(), "empty roster ⇒ empty result; got {got:?}");
}
