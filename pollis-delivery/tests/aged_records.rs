//! Aged-record retention (#762): `security_event` and `push_token` were retained
//! forever — not by decision, but because nothing ever deleted from them.
//!
//! These bounds are keyed on the row's OWN timestamp, which is safe here in a way
//! it is emphatically not for envelopes. #688 removed the envelope TTL because an
//! envelope's age says nothing about whether its recipient collected it, so a
//! clock could delete undelivered mail. An audit row's age *is* the thing being
//! bounded, and a push token's `updated_at` is its own liveness signal — clients
//! re-register on launch. So there is no analogous data-loss trap, and these
//! tests pin that the bound only ever removes rows that are genuinely past it.

use libsql::Connection;
use pollis_delivery::messages::sweep_aged_records;

const SCHEMA: &str = "\
CREATE TABLE security_event (\
  id TEXT PRIMARY KEY, user_id TEXT NOT NULL, kind TEXT NOT NULL, \
  device_id TEXT, created_at TEXT NOT NULL, metadata TEXT);\
CREATE TABLE push_token (\
  token TEXT PRIMARY KEY, user_id TEXT NOT NULL, platform TEXT NOT NULL, \
  updated_at TEXT NOT NULL);";

async fn conn() -> Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let c = db.connect().unwrap();
    c.execute_batch(SCHEMA).await.unwrap();
    c
}

/// Insert a security event stamped `days_ago` in the past.
async fn seed_event(c: &Connection, id: &str, days_ago: i64) {
    c.execute(
        "INSERT INTO security_event (id, user_id, kind, created_at) \
         VALUES (?1, 'u1', 'device_added', datetime('now', ?2))",
        libsql::params![id.to_string(), format!("-{days_ago} days")],
    )
    .await
    .unwrap();
}

async fn seed_token(c: &Connection, token: &str, days_ago: i64) {
    c.execute(
        "INSERT INTO push_token (token, user_id, platform, updated_at) \
         VALUES (?1, 'u1', 'ios', datetime('now', ?2))",
        libsql::params![token.to_string(), format!("-{days_ago} days")],
    )
    .await
    .unwrap();
}

async fn count(c: &Connection, table: &str) -> i64 {
    let mut rows = c.query(&format!("SELECT COUNT(*) FROM {table}"), ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// The defaults (90 / 180 days) remove what is past them and nothing else.
#[tokio::test]
async fn defaults_reap_only_rows_past_the_bound() {
    let c = conn().await;
    seed_event(&c, "old", 120).await;
    seed_event(&c, "fresh", 10).await;
    seed_token(&c, "stale", 200).await;
    seed_token(&c, "live", 30).await;

    let (events, tokens) = sweep_aged_records(&c, 90, 180).await.unwrap();
    assert_eq!((events, tokens), (1, 1), "exactly one of each should age out");
    assert_eq!(count(&c, "security_event").await, 1);
    assert_eq!(count(&c, "push_token").await, 1);

    // And it is the RIGHT one that survived, not merely the right count.
    let mut rows = c.query("SELECT id FROM security_event", ()).await.unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<String>(0).unwrap(), "fresh");
    let mut rows = c.query("SELECT token FROM push_token", ()).await.unwrap();
    assert_eq!(rows.next().await.unwrap().unwrap().get::<String>(0).unwrap(), "live");
}

/// A row exactly at the boundary is kept. The bound is "older than N days", and
/// off-by-one here silently deletes a day's audit trail.
#[tokio::test]
async fn a_row_at_the_boundary_is_kept() {
    let c = conn().await;
    seed_event(&c, "boundary", 89).await;
    seed_token(&c, "boundary", 179).await;
    let (events, tokens) = sweep_aged_records(&c, 90, 180).await.unwrap();
    assert_eq!((events, tokens), (0, 0));
    assert_eq!(count(&c, "security_event").await, 1);
    assert_eq!(count(&c, "push_token").await, 1);
}

/// `0` disables a bound — the operator who would rather keep the trail than bound
/// it must be able to say so. Each bound is independent, so disabling one must not
/// quietly disable the other.
#[tokio::test]
async fn zero_disables_each_bound_independently() {
    let c = conn().await;
    seed_event(&c, "ancient", 5000).await;
    seed_token(&c, "ancient", 5000).await;

    let (events, tokens) = sweep_aged_records(&c, 0, 365).await.unwrap();
    assert_eq!(events, 0, "0 must retain security events forever");
    assert_eq!(tokens, 1, "the other bound must still apply independently");
    assert_eq!(count(&c, "security_event").await, 1);
    assert_eq!(count(&c, "push_token").await, 0);
}

/// An empty database is a no-op, not an error — the sweep runs on a timer against
/// a DS that may have no traffic at all.
#[tokio::test]
async fn empty_tables_are_a_no_op() {
    let c = conn().await;
    assert_eq!(sweep_aged_records(&c, 90, 180).await.unwrap(), (0, 0));
}
