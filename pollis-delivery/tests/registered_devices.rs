//! #679 — a revoked device must not count as registered.
//!
//! Revocation tombstones the row, so it must be excluded by the predicate, not absence.

use std::collections::HashSet;

use pollis_delivery::account_reads::registered_device_pairs;

mod common;

/// Seeds against the shipped schema (#875) so column changes break these tests.
async fn db_with_devices(rows: &[(&str, &str, Option<&str>)]) -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.expect("conn");
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    for (user_id, _, _) in rows {
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

/// Re-enrolment is a new `device_id`: the old leaf is droppable, the new one admitted.
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

#[tokio::test]
async fn mixed_users_yield_only_live_devices() {
    let db = db_with_devices(&[
        ("alice", "a-live-1", None),
        ("alice", "a-live-2", None),
        ("alice", "a-dead", Some("2026-07-30 12:00:00")),
        ("bob", "b-dead", Some("2026-07-30 12:00:00")),
        ("carol", "c-live", None),
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

/// A user with every device revoked contributes nothing, without error.
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

/// The ids are bound, not interpolated, so they match exactly or not at all.
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

/// An empty roster must not build `IN ()` nor fall through to an unfiltered scan.
#[tokio::test]
async fn empty_roster_short_circuits() {
    let db = db_with_devices(&[("alice", "a1", None)]).await;
    let got = registered(&db, &[]).await;
    assert!(got.is_empty(), "empty roster ⇒ empty result; got {got:?}");
}
