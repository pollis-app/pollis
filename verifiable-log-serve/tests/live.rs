//! Gate for live lazy-cache serving mode (`serve live`).
//!
//! Everything here runs against a LOCAL libSQL fixture file (a temp SQLite DB
//! seeded with `mls_commit_log` rows) — no network, no prod DB. Covers:
//! (a) live `/v1/sth/latest.json` and `/verify/group/<id>` return correct data;
//! (b) within a large TTL a second request does NOT rebuild (mutate the fixture,
//!     prove the response is unchanged until the TTL elapses / `ttl=0`);
//! (c) single-flight: many concurrent requests in the cold/stale window cause
//!     exactly one DB pull (asserted via the rebuild counter);
//! (d) a forked / epoch-regressed group yields `chain_valid = false`;
//! (e) the `/verify/group` endpoint carries the CORS header (+ OPTIONS preflight);
//! (f) the in-memory `verify_group_in_bundle` and the URL-based `verify_group`
//!     agree for the same data.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use verifiable_log::{Entry, Sth, VerifiableLog};
use verifiable_log_builder::CommitLeaf;
use verifiable_log_serve::bundle::{Bundle, ConsistencyCheck, InclusionCheck};
use verifiable_log_serve::group::{verify_group, verify_group_in_bundle, GroupReport};
use verifiable_log_serve::{layout, DevServer, LiveServer, Manifest};

const TS: u64 = 1_700_000_000_000;

fn signing_key() -> SigningKey {
    SigningKey::from_bytes(&[5u8; 32])
}

// ---------------------------------------------------------------------------
// Fixture DB helpers (local libSQL file, no network).
// ---------------------------------------------------------------------------

struct Row {
    seq: i64,
    conv: &'static str,
    epoch: i64,
    data: &'static str,
}

fn row(seq: i64, conv: &'static str, epoch: i64, data: &'static str) -> Row {
    Row { seq, conv, epoch, data }
}

/// Drive an async block on a throwaway current-thread runtime. Keeps the test
/// functions plain `#[test]` so they never nest inside the `LiveServer`'s own
/// runtime (which lives on its worker threads).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

/// Create a fresh local libSQL file with the real `mls_commit_log` shape (no
/// UNIQUE index, so forks/regressions can be seeded as a buggy server would).
fn seed_db(path: &Path, rows: &[Row]) {
    block_on(async {
        let db = libsql::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute(
            "CREATE TABLE mls_commit_log (\
                seq INTEGER PRIMARY KEY AUTOINCREMENT, \
                conversation_id TEXT NOT NULL, \
                epoch INTEGER NOT NULL, \
                sender_id TEXT NOT NULL, \
                commit_data BLOB NOT NULL, \
                created_at TEXT NOT NULL, \
                added_user_id TEXT, added_device_ids TEXT)",
            (),
        )
        .await
        .unwrap();
        insert(&conn, rows).await;
    });
}

/// Append more rows to an existing fixture DB (used to mutate it mid-test).
fn insert_rows(path: &Path, rows: &[Row]) {
    block_on(async {
        let db = libsql::Builder::new_local(path).build().await.unwrap();
        let conn = db.connect().unwrap();
        insert(&conn, rows).await;
    });
}

async fn insert(conn: &libsql::Connection, rows: &[Row]) {
    for r in rows {
        conn.execute(
            "INSERT INTO mls_commit_log \
                (seq, conversation_id, epoch, sender_id, commit_data, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![
                r.seq,
                r.conv.to_string(),
                r.epoch,
                format!("u-{}", r.conv),
                r.data.as_bytes().to_vec(),
                "2026-01-01T00:00:00Z".to_string()
            ],
        )
        .await
        .unwrap();
    }
}

/// Two healthy conversations interleaved in seq order.
fn healthy_rows() -> Vec<Row> {
    vec![
        row(1, "conv-a", 0, "a0"),
        row(2, "conv-b", 0, "b0"),
        row(3, "conv-a", 1, "a1"),
        row(4, "conv-b", 1, "b1"),
        row(5, "conv-a", 2, "a2"),
    ]
}

// ---------------------------------------------------------------------------
// HTTP helpers.
// ---------------------------------------------------------------------------

fn get_json<T: serde::de::DeserializeOwned>(base: &str, path: &str) -> T {
    let body = ureq::get(&format!("{base}{path}"))
        .call()
        .unwrap()
        .into_string()
        .unwrap();
    serde_json::from_str(&body).unwrap()
}

fn http_group(base: &str, id: &str) -> (u16, Option<String>, GroupReport) {
    let resp = ureq::get(&format!("{base}/verify/group/{id}")).call().unwrap();
    let status = resp.status();
    let cors = resp.header("Access-Control-Allow-Origin").map(str::to_string);
    let report: GroupReport = serde_json::from_str(&resp.into_string().unwrap()).unwrap();
    (status, cors, report)
}

// ---------------------------------------------------------------------------
// Planted-bundle helpers (for the read-time verifier — forks/regressions that
// the builder would reject, so they are built WITHOUT the invariant, exactly as
// a buggy/malicious server might have published them).
// ---------------------------------------------------------------------------

fn leaf(conv: &str, epoch: u64, seq: i64, commit: &str) -> CommitLeaf {
    CommitLeaf {
        conversation_id: conv.to_string(),
        epoch,
        sender_id: format!("u-{conv}"),
        seq,
        commit_sha256: hex::encode(distinct_hash(commit)),
    }
}

fn distinct_hash(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, b) in s.bytes().enumerate() {
        out[i % 32] ^= b.wrapping_add(i as u8);
    }
    out
}

/// Build a [`Bundle`] directly from leaves, in order, with NO `CommitLogInvariant`
/// — so forks/regressions get published and it is the read-time verifier that
/// must catch them. Optionally also writes the static `/v1` tree under `root`.
fn plant_bundle(leaves: &[CommitLeaf], root: Option<&Path>) -> Bundle {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let mut log = VerifiableLog::new();
    let entries: Vec<Entry> = leaves.iter().map(|l| l.to_entry().unwrap()).collect();
    for e in &entries {
        log.append(e.clone()).unwrap();
    }

    let n = log.size();
    let mut sths: Vec<Sth> = Vec::new();
    let mut midpoint = None;
    if n >= 2 {
        let m = n / 2;
        midpoint = Some(m);
        sths.push(Sth::create(&key, m as u64, log.root_at(m).unwrap(), TS));
    }
    sths.push(log.signed_tree_head(&key, TS));
    let final_index = sths.len() - 1;

    let inclusion = (0..n)
        .map(|i| InclusionCheck {
            entry: entries[i].clone(),
            proof: log.inclusion_proof(i).unwrap(),
            sth_index: final_index,
        })
        .collect();

    let consistency = midpoint
        .map(|m| {
            vec![ConsistencyCheck {
                old_index: 0,
                new_index: final_index,
                proof: log.consistency_proof(m, n).unwrap(),
            }]
        })
        .unwrap_or_default();

    let bundle = Bundle {
        public_key: hex::encode(key.verifying_key().to_bytes()),
        sths,
        entries,
        enforce_unique: vec!["mls-commit-log".to_string()],
        inclusion,
        consistency,
    };
    if let Some(root) = root {
        layout::generate(&bundle, root).unwrap();
    }
    bundle
}

/// Healthy + forked (conv-c) + epoch-regressed (conv-d) groups, interleaved.
fn mixed_leaves() -> Vec<CommitLeaf> {
    vec![
        leaf("conv-a", 0, 1, "a0"),
        leaf("conv-b", 0, 2, "b0"),
        leaf("conv-a", 1, 3, "a1"),
        leaf("conv-b", 1, 4, "b1"),
        leaf("conv-a", 2, 5, "a2"),
        leaf("conv-c", 0, 6, "c0"),
        leaf("conv-c", 0, 7, "c0-EVIL"),
        leaf("conv-d", 0, 8, "d0"),
        leaf("conv-d", 5, 9, "d5"),
        leaf("conv-d", 3, 10, "d3"),
    ]
}

// ---------------------------------------------------------------------------
// (a) Live correctness over HTTP.
// ---------------------------------------------------------------------------

#[test]
fn live_serves_latest_and_group_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("commits.db");
    seed_db(&db, &healthy_rows());

    let key = signing_key();
    let server = LiveServer::spawn(
        db.to_str().unwrap().to_string(),
        0,
        Duration::from_secs(60),
        key.clone(),
    )
    .unwrap();
    let base = server.base_url();

    // /v1/sth/latest.json reflects all 5 commits and verifies under the key.
    let sth: Sth = get_json(&base, "/v1/sth/latest.json");
    assert_eq!(sth.tree_size, 5);
    assert!(sth.verify(&key.verifying_key()), "latest STH must verify");

    // /v1/index.json discovery manifest.
    let manifest: Manifest = get_json(&base, "/v1/index.json");
    assert_eq!(manifest.entry_count, 5);
    assert_eq!(manifest.latest_tree_size, Some(5));

    // /verify/group/<id> for a healthy group.
    let (status, cors, report) = http_group(&base, "conv-a");
    assert_eq!(status, 200);
    assert_eq!(cors.as_deref(), Some("*"));
    assert!(report.found);
    assert!(report.chain_valid, "violations: {:?}", report.violations);
    let epochs: Vec<u64> = report.commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1, 2]);
    assert!(report.commits.iter().all(|c| c.included));

    server.shutdown();
}

// ---------------------------------------------------------------------------
// (b) Within TTL: no rebuild; the cache is served untouched even after the DB
//     changes. With ttl=0 the change shows up.
// ---------------------------------------------------------------------------

#[test]
fn within_ttl_does_not_rebuild_then_refreshes() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("commits.db");
    seed_db(&db, &healthy_rows());
    let db_url = db.to_str().unwrap().to_string();

    // Large TTL: the first request builds, the rest serve the cache.
    let server =
        LiveServer::spawn(db_url.clone(), 0, Duration::from_secs(3600), signing_key()).unwrap();
    let base = server.base_url();

    let (_, _, first) = http_group(&base, "conv-a");
    assert_eq!(first.commits.len(), 3, "conv-a starts with 3 commits");
    assert_eq!(server.rebuild_count(), 1, "first request triggers one build");

    // Mutate the fixture: add conv-a epoch 3. Within the TTL this must NOT show.
    insert_rows(&db, &[row(6, "conv-a", 3, "a3")]);

    let (_, _, again) = http_group(&base, "conv-a");
    assert_eq!(again.commits.len(), 3, "within TTL the cache is served untouched");
    assert_eq!(server.rebuild_count(), 1, "no second DB pull within TTL");
    // Also via the moving STH pointer: tree_size unchanged within the TTL.
    let sth: Sth = get_json(&base, "/v1/sth/latest.json");
    assert_eq!(sth.tree_size, 5);
    assert_eq!(server.rebuild_count(), 1);
    server.shutdown();

    // A fresh server with ttl=0 rebuilds every request and sees the mutation.
    let live0 = LiveServer::spawn(db_url, 0, Duration::ZERO, signing_key()).unwrap();
    let (_, _, fresh) = http_group(&live0.base_url(), "conv-a");
    assert_eq!(fresh.commits.len(), 4, "ttl=0 reflects the added commit");
    let epochs: Vec<u64> = fresh.commits.iter().map(|c| c.epoch).collect();
    assert_eq!(epochs, vec![0, 1, 2, 3]);
    live0.shutdown();
}

// ---------------------------------------------------------------------------
// (c) Single-flight: many concurrent requests in the cold window cause exactly
//     one DB pull.
// ---------------------------------------------------------------------------

#[test]
fn concurrent_requests_trigger_exactly_one_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("commits.db");
    seed_db(&db, &healthy_rows());

    // Cold cache + a large TTL: the first request to win the rebuild lock builds,
    // every other concurrent request must serve its result, not pull again.
    let server = LiveServer::spawn(
        db.to_str().unwrap().to_string(),
        0,
        Duration::from_secs(3600),
        signing_key(),
    )
    .unwrap();
    let base = Arc::new(server.base_url());

    let handles: Vec<_> = (0..32)
        .map(|_| {
            let base = base.clone();
            std::thread::spawn(move || {
                let resp = ureq::get(&format!("{base}/v1/sth/latest.json")).call().unwrap();
                assert_eq!(resp.status(), 200);
                resp.into_string().unwrap();
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }

    assert_eq!(
        server.rebuild_count(),
        1,
        "single-flight: exactly one DB pull for a burst in the stale window"
    );

    server.shutdown();
}

// ---------------------------------------------------------------------------
// (d) A forked / epoch-regressed group yields chain_valid = false.
// ---------------------------------------------------------------------------

#[test]
fn forked_and_regressed_groups_are_invalid() {
    let bundle = plant_bundle(&mixed_leaves(), None);

    // Fork (conv-c): two commits at the same epoch.
    let forked = verify_group_in_bundle(&bundle, "conv-c");
    assert!(forked.found);
    assert!(!forked.chain_valid, "a forked group must fail");
    assert!(
        forked.violations.iter().any(|v| v.contains("fork")),
        "expected a fork violation, got: {:?}",
        forked.violations
    );

    // Epoch regression (conv-d): 0 -> 5 -> 3.
    let regressed = verify_group_in_bundle(&bundle, "conv-d");
    assert!(!regressed.chain_valid, "an epoch-regressed group must fail");
    assert!(
        regressed.violations.iter().any(|v| v.contains("regression")),
        "expected a regression violation, got: {:?}",
        regressed.violations
    );

    // The healthy group in the same bundle still verifies.
    let healthy = verify_group_in_bundle(&bundle, "conv-a");
    assert!(healthy.chain_valid, "violations: {:?}", healthy.violations);
}

// ---------------------------------------------------------------------------
// (e) CORS header present on the live verify endpoint (+ OPTIONS preflight).
// ---------------------------------------------------------------------------

#[test]
fn live_verify_group_has_cors() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("commits.db");
    seed_db(&db, &healthy_rows());

    let server = LiveServer::spawn(
        db.to_str().unwrap().to_string(),
        0,
        Duration::from_secs(60),
        signing_key(),
    )
    .unwrap();
    let base = server.base_url();

    let (_, cors, _) = http_group(&base, "conv-a");
    assert_eq!(cors.as_deref(), Some("*"));

    let pre = ureq::request("OPTIONS", &format!("{base}/verify/group/conv-a"))
        .call()
        .unwrap();
    assert_eq!(pre.status(), 204);
    assert_eq!(pre.header("Access-Control-Allow-Origin"), Some("*"));
    assert!(pre.header("Access-Control-Allow-Methods").is_some());

    server.shutdown();
}

// ---------------------------------------------------------------------------
// (f) The in-memory verifier and the URL-based verifier agree.
// ---------------------------------------------------------------------------

#[test]
fn in_memory_and_url_verify_agree() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = plant_bundle(&mixed_leaves(), Some(dir.path()));
    let server = DevServer::spawn(dir.path().to_path_buf(), 0).unwrap();
    let base = server.base_url();

    for id in ["conv-a", "conv-b", "conv-c", "conv-d", "conv-missing"] {
        let over_http = verify_group(&base, id).unwrap();
        let in_memory = verify_group_in_bundle(&bundle, id);
        assert_eq!(over_http, in_memory, "verdicts must agree for `{id}`");
    }

    server.shutdown();
}
