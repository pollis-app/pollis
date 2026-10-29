//! #685 (Part C) — `resign_stale_device_certs` must not re-sign a REVOKED
//! device's cert.

use pollis_api::account_reads::DeviceRow;
use pollis_core::commands::mls::stale_cert_candidates;

/// One `user_device` row as the DS reports it.
fn device(device_id: &str, revoked: bool, cert_v: Option<i64>, has_pubs: bool) -> DeviceRow {
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
        .map(|d| d.device_id)
        .collect();
    ids.sort();
    ids
}

#[test]
fn revoked_device_is_never_a_resign_candidate() {
    let rows = vec![
        device("d-live-stale", false, Some(1), true),
        device("d-revoked-stale", true, Some(1), true),
        device("d-revoked-nullcert", true, None, true),
        device("d-live-current", false, Some(2), true),
        // Missing leaf pubs: nothing to sign over yet.
        device("d-live-nopub", false, Some(1), false),
    ];

    assert_eq!(
        candidate_ids(&rows, 2),
        vec!["d-live-stale".to_string()],
        "only the live, stale device is a re-sign candidate — a revoked device \
         (stale OR never-certed) must never be re-signed (#685)"
    );
}

#[test]
fn the_revoked_filter_is_the_deciding_predicate() {
    let mut rows = vec![
        device("d-live-stale", false, Some(1), true),
        device("d-was-revoked", true, Some(1), true),
    ];
    assert_eq!(candidate_ids(&rows, 2), vec!["d-live-stale".to_string()]);

    rows[1].revoked_at = None;
    assert_eq!(
        candidate_ids(&rows, 2),
        vec!["d-live-stale".to_string(), "d-was-revoked".to_string()],
        "clearing the tombstone must make the device a candidate again — the \
         revocation filter is the only thing that was excluding it"
    );
}

/// A never-certified device is stale by definition.
#[test]
fn a_never_certified_live_device_is_stale() {
    let rows = vec![device("d-never-certed", false, None, true)];
    assert_eq!(candidate_ids(&rows, 1), vec!["d-never-certed".to_string()]);
}
