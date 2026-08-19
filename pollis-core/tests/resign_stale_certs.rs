//! #685 (Part C) — `resign_stale_device_certs` must not re-sign a REVOKED
//! device's cert. A revoked device can never rejoin the tree, so re-signing its
//! cert is wasted work and would resurrect a valid-looking cert for a device
//! that was deliberately retired.
//!
//! The exercised predicate is
//! [`pollis_core::commands::mls::stale_cert_candidates`], extracted from
//! `resign_stale_device_certs` so it can be driven without the OS-keystore
//! account-key signing and DS write the rest of that function does.
//!
//! # Why this is no longer a database test (#987)
//!
//! It used to build an in-memory libsql database, seed `user_device`, and run
//! the candidate SELECT — and it had to live in its own integration binary to do
//! it, because libsql's local backend calls `sqlite3_config` on first use, which
//! fails once rusqlite/SQLCipher has already initialised SQLite in the same
//! process.
//!
//! Since #987 the client holds no database credential: the ROWS arrive from
//! `POST /v1/read/devices` and the predicate is pure. So the fixture is a list
//! of rows rather than a schema, and what is asserted is the same thing it
//! always was — that revocation, and nothing else, is what excludes a device.
//! The row-fetching half (`revoked_at IS NULL` in the query) is covered
//! server-side by `pollis-delivery/tests/registered_devices.rs`; this covers the
//! second, independent exclusion the client applies on top of it.

use pollis_api::account_reads::DeviceRow;
use pollis_core::commands::mls::stale_cert_candidates;

/// One `user_device` row as the DS reports it. `cert_v` is
/// `cert_identity_version` (None → never certified), `revoked` toggles the
/// tombstone, `has_pubs` controls whether both leaf pub columns are present (a
/// NULL leaf pub is skipped independently of revocation).
fn device(device_id: &str, revoked: bool, cert_v: Option<i64>, has_pubs: bool) -> DeviceRow {
    // base64 of a placeholder leaf pub — the predicate only decodes it, never
    // interprets it.
    let pubs = has_pubs.then(|| "AQID".to_string());
    DeviceRow {
        device_id: device_id.to_string(),
        device_name: None,
        created_at: None,
        last_seen: None,
        revoked_at: revoked.then(|| "2026-07-30 12:00:00".to_string()),
        device_cert: None,
        cert_issued_at: None,
        cert_identity_version: cert_v,
        mls_signature_pub: pubs.clone(),
        mls_signature_pub_pq: pubs,
    }
}

fn candidate_ids(rows: &[DeviceRow], identity_version: i64) -> Vec<String> {
    let mut ids: Vec<String> = stale_cert_candidates(rows, identity_version)
        .expect("stale_cert_candidates")
        .into_iter()
        .map(|(did, _, _)| did)
        .collect();
    ids.sort();
    ids
}

/// The #685 regression itself. The account has rotated to identity version 2, so
/// every device certed below 2 is stale. A stale but LIVE device is a candidate;
/// a stale REVOKED device is NOT. Before the fix both revoked devices came back
/// too, so `resign_stale_device_certs` re-signed a retired device's cert.
#[test]
fn revoked_device_is_never_a_resign_candidate() {
    let rows = vec![
        // Stale (cert_v 1 < 2) and live → the one legitimate candidate.
        device("d-live-stale", false, Some(1), true),
        // Stale (cert_v 1 < 2) but REVOKED → must be excluded by the fix.
        device("d-revoked-stale", true, Some(1), true),
        // Never-certed (cert_v NULL) and REVOKED → also excluded.
        device("d-revoked-nullcert", true, None, true),
        // Live but already at the current version → not stale, excluded.
        device("d-live-current", false, Some(2), true),
        // Live and stale but missing leaf pubs → skipped for the pre-existing
        // reason (nothing to sign over yet).
        device("d-live-nopub", false, Some(1), false),
    ];

    assert_eq!(
        candidate_ids(&rows, 2),
        vec!["d-live-stale".to_string()],
        "only the live, stale device is a re-sign candidate — a revoked device \
         (stale OR never-certed) must never be re-signed (#685)"
    );
}

/// Revocation is the ONLY thing separating the two otherwise-identical stale
/// devices: clear the tombstone and the formerly-revoked device becomes a
/// candidate, proving the revocation check is what excludes it (not some other
/// column).
#[test]
fn the_revoked_filter_is_the_deciding_predicate() {
    let mut rows = vec![
        device("d-live-stale", false, Some(1), true),
        // Same shape, only difference is the tombstone → currently excluded.
        device("d-was-revoked", true, Some(1), true),
    ];
    assert_eq!(candidate_ids(&rows, 2), vec!["d-live-stale".to_string()]);

    // Un-revoke it; now it is an ordinary stale-live device and must appear.
    rows[1].revoked_at = None;
    assert_eq!(
        candidate_ids(&rows, 2),
        vec!["d-live-stale".to_string(), "d-was-revoked".to_string()],
        "clearing the tombstone must make the device a candidate again — the \
         revocation filter is the only thing that was excluding it"
    );
}

/// A never-certified device is stale by definition. Asserted separately because
/// the SQL expressed it as `cert_identity_version IS NULL OR < ?`, and the pure
/// predicate has to keep the NULL leg — dropping it would silently strand every
/// device that has never had a cert.
#[test]
fn a_never_certified_live_device_is_stale() {
    let rows = vec![device("d-never-certed", false, None, true)];
    assert_eq!(candidate_ids(&rows, 1), vec!["d-never-certed".to_string()]);
}
