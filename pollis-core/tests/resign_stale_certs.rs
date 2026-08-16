//! #685 (Part C) — `resign_stale_device_certs` must not re-sign a REVOKED
//! device's cert. A revoked device can never rejoin the tree, so re-signing its
//! cert is wasted work and would resurrect a valid-looking cert for a device
//! that was deliberately retired.
//!
//! These live in an integration binary rather than pollis-core's `--lib` tests
//! deliberately (identical reasoning to `revoked_device_reconcile.rs`): libsql's
//! local backend calls `sqlite3_config` on first use, which fails once
//! rusqlite/SQLCipher — which `LocalDb` uses — has already initialised SQLite in
//! the same process. An integration test gets its own process and never loads
//! rusqlite, so the real candidate query runs against a real DB.
//!
//! The exercised predicate is [`pollis_core::commands::mls::stale_cert_candidates`],
//! extracted from `resign_stale_device_certs` so it can be driven without the
//! OS-keystore account-key signing and DS write the rest of that function does.

use pollis_core::commands::mls::stale_cert_candidates;

/// The SHIPPED remote schema. #875: this used to declare its own four-column
/// `user_device`, which is a different table from the one the candidate query
/// runs against in production (no `pq_capable`, no cert columns, no
/// `idx_user_device_user`).
async fn db() -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .expect("in-memory libsql");
    let conn = db.connect().expect("connect");
    for sql in pollis_schema::main_scripts() {
        conn.execute_batch(sql).await.expect("schema");
    }
    // The `Database` must outlive the `Connection` (libsql's local backend
    // panics otherwise); this is a one-per-test in-memory file, so leak it.
    std::mem::forget(db);
    // `user_device.user_id` is a real foreign key; libsql's local backend
    // enforces it. Give the devices an owner.
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice');",
    )
    .await
    .expect("seed user");
    conn
}

/// Insert one device. `cert_v` is `cert_identity_version` (None → NULL),
/// `revoked` toggles the tombstone, `has_pubs` controls whether both leaf pub
/// columns are present (a NULL leaf pub is skipped independently of revocation).
async fn add_device(
    conn: &libsql::Connection,
    device_id: &str,
    revoked: bool,
    cert_v: Option<i64>,
    has_pubs: bool,
) {
    let pubs: Option<Vec<u8>> = if has_pubs { Some(vec![1u8, 2, 3]) } else { None };
    conn.execute(
        "INSERT INTO user_device \
             (device_id, user_id, revoked_at, cert_identity_version, mls_signature_pub, mls_signature_pub_pq) \
         VALUES (?1, 'alice', ?2, ?3, ?4, ?4)",
        libsql::params![
            device_id.to_string(),
            if revoked { Some("2026-07-30 12:00:00".to_string()) } else { None },
            cert_v,
            pubs,
        ],
    )
    .await
    .expect("insert device");
}

async fn candidate_ids(conn: &libsql::Connection, identity_version: i64) -> Vec<String> {
    let mut ids: Vec<String> = stale_cert_candidates(conn, "alice", identity_version)
        .await
        .expect("stale_cert_candidates")
        .into_iter()
        .map(|(did, _, _)| did)
        .collect();
    ids.sort();
    ids
}

/// The #685 regression itself. The account has rotated to identity version 2, so
/// every device certed below 2 is stale. A stale but LIVE device is a candidate;
/// a stale REVOKED device is NOT. Before the fix the query returned both revoked
/// devices too, so `resign_stale_device_certs` re-signed a retired device's cert.
#[tokio::test]
async fn revoked_device_is_never_a_resign_candidate() {
    let conn = db().await;
    // Stale (cert_v 1 < 2) and live → the one legitimate candidate.
    add_device(&conn, "d-live-stale", false, Some(1), true).await;
    // Stale (cert_v 1 < 2) but REVOKED → must be excluded by the fix.
    add_device(&conn, "d-revoked-stale", true, Some(1), true).await;
    // Never-certed (cert_v NULL) and REVOKED → also excluded.
    add_device(&conn, "d-revoked-nullcert", true, None, true).await;
    // Live but already at the current version → not stale, excluded.
    add_device(&conn, "d-live-current", false, Some(2), true).await;
    // Live and stale but missing leaf pubs → skipped for the pre-existing reason.
    add_device(&conn, "d-live-nopub", false, Some(1), false).await;

    assert_eq!(
        candidate_ids(&conn, 2).await,
        vec!["d-live-stale".to_string()],
        "only the live, stale device is a re-sign candidate — a revoked device \
         (stale OR never-certed) must never be re-signed (#685)"
    );
}

/// Revocation is the ONLY thing separating the two otherwise-identical stale
/// devices: flip the tombstone off and the formerly-revoked device becomes a
/// candidate, proving the `revoked_at IS NULL` predicate is what excludes it (not
/// some other column).
#[tokio::test]
async fn the_revoked_filter_is_the_deciding_predicate() {
    let conn = db().await;
    add_device(&conn, "d-live-stale", false, Some(1), true).await;
    // Same shape, only difference is the tombstone → currently excluded.
    add_device(&conn, "d-was-revoked", true, Some(1), true).await;
    assert_eq!(candidate_ids(&conn, 2).await, vec!["d-live-stale".to_string()]);

    // Un-revoke it; now it is an ordinary stale-live device and must appear.
    conn.execute(
        "UPDATE user_device SET revoked_at = NULL WHERE device_id = 'd-was-revoked'",
        (),
    )
    .await
    .unwrap();
    assert_eq!(
        candidate_ids(&conn, 2).await,
        vec!["d-live-stale".to_string(), "d-was-revoked".to_string()],
        "clearing the tombstone must make the device a candidate again — the \
         revocation filter is the only thing that was excluding it"
    );
}
