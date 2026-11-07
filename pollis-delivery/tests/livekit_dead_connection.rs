//! #1751 — a nudge that lands on a dead pooled connection is retried on a fresh
//! one. Its own test binary: the LiveKit client is process-wide, and another
//! test resetting it would drop the very connection this one needs pooled.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use pollis_delivery::broker::{room_send_data, BrokerConfig, SendOutcome};
use pollis_delivery::AppState;

mod common;

use common::fake_livekit::livekit_with_a_dead_pooled_connection;

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
        .await
        .expect("schema");
    db
}

fn livekit_broker(url: &str) -> BrokerConfig {
    BrokerConfig {
        livekit_api_key: Some("APIkey".into()),
        livekit_api_secret: Some("secret-secret-secret-secret".into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
}

/// The #1751 shape: the second nudge lands on a pooled connection that is dead.
/// Before, it timed out and so did every later one until a redeploy; now it is
/// retried on a fresh connection and delivered.
#[tokio::test(flavor = "multi_thread")]
async fn a_nudge_on_a_dead_pooled_connection_is_retried_on_a_fresh_one() {
    let (url, accepted, bodies) = livekit_with_a_dead_pooled_connection().await;
    let db = fresh_db().await;
    let state = AppState::new(db.arc(), false).with_broker_config(livekit_broker(&url));
    let payload = serde_json::json!({ "type": "inbox_refresh" });

    assert_eq!(room_send_data(&state, "inbox-alice", &payload).await, Ok(SendOutcome::Delivered));
    // Let the connection settle back into the pool, as it would between real nudges.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let started = Instant::now();
    let second = room_send_data(&state, "inbox-alice", &payload).await;
    let elapsed = started.elapsed();
    assert_eq!(second, Ok(SendOutcome::Delivered), "the retry on a fresh connection must deliver");
    assert!(
        accepted.load(Ordering::SeqCst) >= 2,
        "the retry must open a NEW connection, not reuse the dead one"
    );
    assert!(elapsed < Duration::from_secs(15), "one deadline plus a quick retry; took {elapsed:?}");

    // A third nudge goes straight through: the dead connection is gone for good.
    let started = Instant::now();
    assert_eq!(room_send_data(&state, "inbox-alice", &payload).await, Ok(SendOutcome::Delivered));
    assert!(started.elapsed() < Duration::from_secs(2), "no second outage after the reset");

    let snap = state.livekit_health.snapshot(pollis_delivery::util::now_unix());
    assert_eq!(snap["ok"], 3);
    assert_eq!(snap["ok_on_retry"], 1);
    assert_eq!(snap["retries"], 1);
    assert_eq!(snap["consecutive_failures"], 0);

    // The retry resent the SAME nudge: same nonce, so LiveKit can drop a duplicate.
    let bodies = bodies.lock().unwrap().clone();
    let nonces: Vec<&str> = bodies.iter().filter_map(|b| b["nonce"].as_str()).collect();
    assert_eq!(nonces.len(), bodies.len(), "every SendData carries a nonce: {bodies:?}");
    use base64::Engine as _;
    for n in &nonces {
        let raw = base64::engine::general_purpose::STANDARD.decode(n).expect("base64 nonce");
        assert_eq!(raw.len(), 16, "LiveKit takes a 16-byte nonce");
    }
    assert_ne!(nonces[0], nonces[nonces.len() - 1], "distinct nudges get distinct nonces");
}

