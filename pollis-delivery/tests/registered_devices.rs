//! #679 — a revoked device must not count as registered.
//!
//! These tests moved here from `pollis-core/tests/revoked_device_reconcile.rs`
//! in #987, along with the query they cover: the client no longer holds a
//! database credential, so `registered_devices` is now
//! `POST /v1/read/registered-devices` and the predicate that decides whether a
//! tombstoned device is "registered" lives in this crate.
//!
//! What is being protected has not changed. `revoke_device` TOMBSTONES the row
//! (`revoked_at`) rather than deleting it, precisely so other members can tell
//! "revoked, drop the rejoin" from "not replicated yet, retry" — so the
//! revocation must be excluded by the PREDICATE, never by the row's absence.
//! Before the #679 fix the query returned both devices, which left the revoked
//! leaf in `desired` and therefore out of `to_remove`: the revoked device kept
//! its leaf and went on decrypting.

use std::collections::HashSet;

use pollis_delivery::account_reads::registered_device_pairs;

mod common;

/// The `user_device` shape the query reads, including the #372 tombstone column
/// added by migration 000004. Seeds against the SHIPPED schema rather than a
/// hand-rolled table (#875), so a migration that changes the column set breaks
/// these tests rather than sliding past them.
async fn db_with_devices(rows: &[(&str, &str, Option<&str>)]) -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.expect("conn");
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    for (user_id, _, _) in rows {
        // `user_device.user_id` is a real foreign key; give each device an owner.
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
    db
}

async fn registered(db: &common::TempDb, roster: &[&str]) -> HashSet<(String, String)> {
    let conn = db.conn().await.expect("conn");
    let ids: Vec<String> = roster.iter().map(|s| s.to_string()).collect();
    registered_device_pairs(&conn, &ids)
        .await
        .expect("registered devices")
        .into_iter()
        .map(|p| (p.user_id, p.device_id))
        .collect()
}

/// The #679 regression itself.
#[tokio::test]
async fn revoked_device_is_not_registered() {
    let db = db_with_devices(&[
        ("alice", "live-device", None),
        ("alice", "revoked-device", Some("2026-07-30 12:00:00")),
    ])
    .await;

    let got = registered(&db, &["alice"]).await;

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
    let db = db_with_devices(&[
        ("alice", "a-live-1", None),
        ("alice", "a-live-2", None),
        ("alice", "a-dead", Some("2026-07-30 12:00:00")),
        ("bob", "b-dead", Some("2026-07-30 12:00:00")),
        ("carol", "c-live", None),
        // Not on the roster at all — must never appear.
        ("dave", "d-live", None),
    ])
    .await;

    let got = registered(&db, &["alice", "bob", "carol"]).await;

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
    let db = db_with_devices(&[
        ("alice", "a1", Some("2026-07-30 12:00:00")),
        ("alice", "a2", Some("2026-07-30 12:00:00")),
    ])
    .await;

    let got = registered(&db, &["alice"]).await;
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
    let db = db_with_devices(&[
        ("alice", "old-device", Some("2026-07-30 12:00:00")),
        ("alice", "new-device", None),
    ])
    .await;

    let got = registered(&db, &["alice"]).await;
    assert_eq!(
        got,
        HashSet::from([("alice".to_string(), "new-device".to_string())]),
        "only the re-enrolled device; got {got:?}"
    );
}

/// The ids are BOUND, not interpolated. Previously they were spliced into the
/// SQL behind a character filter that silently *mangled* anything else — a
/// user_id containing a quote became a different id, so the query answered a
/// question nobody asked. Binding makes it match exactly or not at all, which is
/// what makes the `revoked_at IS NULL` predicate trustworthy.
#[tokio::test]
async fn ids_are_bound_not_interpolated() {
    let weird = "alice' OR '1'='1";
    let db = db_with_devices(&[
        (weird, "weird-live", None),
        (weird, "weird-dead", Some("2026-07-30 12:00:00")),
        ("bob", "b-live", None),
    ])
    .await;

    let got = registered(&db, &[weird]).await;

    assert_eq!(
        got,
        HashSet::from([(weird.to_string(), "weird-live".to_string())]),
        "an id with a quote must match itself exactly — and still honour the \
         revocation filter — never widen the query. got {got:?}"
    );
}

/// An empty roster must not build `IN ()` (a syntax error) nor fall through to
/// an unfiltered scan that would report every device in the table.
#[tokio::test]
async fn empty_roster_short_circuits() {
    let db = db_with_devices(&[("alice", "a1", None)]).await;
    let got = registered(&db, &[]).await;
    assert!(got.is_empty(), "empty roster ⇒ empty result; got {got:?}");
}
