//! The DS's send, catch-up, commit and Welcome paths over the REMOTE hrana path
//! (`patches/libsql`), against a real `sqld`, with main and log on separate
//! servers as in production.
//!
//! The rest of the suite runs on local libsql, which never reaches the hrana
//! client code the DS uses against Turso. `#[ignore]`: needs a `sqld` binary
//! (see `support/sqld.rs`):
//!
//! ```text
//! cargo test -p pollis-delivery --test remote_sqld -- --ignored --test-threads=1
//! ```

use std::sync::Arc;

use axum::http::StatusCode;
use base64::Engine as _;
use pollis_delivery::commit::{
    fetch_commits, head_epoch, submit_commit_with, SubmitBody, SubmitResponse, SubmitVerdict,
    WelcomeBody,
};
use pollis_delivery::leaf_cert::LeafCheck;

mod common;
#[path = "support/seq_invariants.rs"]
mod seq_invariants;
#[path = "support/sqld.rs"]
mod sqld;
use sqld::RemoteDb;

const CAP: &str = "MB8lyVjsEQamFuSzLTRZBVKGFQQ8nXoBjHFF7kvNuAo=";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Main and log on two servers; alice (device a1, signing key returned) is in
/// group `g` (channel `c`) and DM `d` with bob.
struct Fixture {
    main: RemoteDb,
    log: RemoteDb,
    state: pollis_delivery::AppState,
    alice: ml_dsa::SigningKey<ml_dsa::MlDsa44>,
}

async fn fixture() -> Fixture {
    let main = RemoteDb::main_db().await;
    let log = RemoteDb::log_db().await;
    let alice = common::gen_signing_key();
    let pub_pq = {
        use ml_dsa::Keypair as _;
        alice.verifying_key().encode().to_vec()
    };
    let c = main.db.conn().await.unwrap();
    c.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('alice','alice@x','alice');\
         INSERT INTO users (id, email, username) VALUES ('bob','bob@x','bob');\
         INSERT INTO user_device (user_id, device_id) VALUES ('bob','b1');\
         INSERT INTO conversation (id, kind) VALUES ('g','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('g','G','alice');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g','alice','admin');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('g','bob','member');\
         INSERT INTO conversation (id, kind) VALUES ('c','channel');\
         INSERT INTO channels (id, group_id, name) VALUES ('c','g','general');\
         INSERT INTO conversation (id, kind) VALUES ('d','dm');\
         INSERT INTO dm_channel (id, created_by) VALUES ('d','alice');\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) \
             VALUES ('d','alice','alice',datetime('now'));\
         INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) \
             VALUES ('d','bob','alice',datetime('now'));",
    )
    .await
    .unwrap();
    c.execute(
        "INSERT INTO user_device (user_id, device_id, mls_signature_pub_pq) VALUES ('alice','a1',?1)",
        libsql::params![pub_pq],
    )
    .await
    .unwrap();
    drop(c);
    let state = pollis_delivery::AppState::new_with_log_db(
        Arc::clone(&main.db),
        Arc::clone(&log.db),
        true,
    );
    Fixture {
        main,
        log,
        state,
        alice,
    }
}

impl Fixture {
    async fn call(&self, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
        common::send_signed(&self.state, path, body, "alice", "a1", &self.alice).await
    }

    fn reset(&self) {
        self.main.proxy.reset();
        self.log.proxy.reset();
    }

    /// No request since the last reset carried a hrana `describe` (#2178).
    fn assert_no_describe(&self, what: &str) {
        for (db, proxy) in [("main", &self.main.proxy), ("log", &self.log.proxy)] {
            let describes: Vec<_> = proxy.log().into_iter().filter(|f| f.describe).collect();
            assert!(describes.is_empty(), "{what}: {db} sent describes: {describes:?}");
        }
    }

    fn round_trips(&self) -> (usize, usize) {
        (self.main.proxy.round_trips(), self.log.proxy.round_trips())
    }
}

fn send_body(id: &str, conversation_id: &str) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "conversation_id": conversation_id,
        "sender_id": "sealed",
        "ciphertext": "mls:00",
        "sent_at": chrono::Utc::now().to_rfc3339(),
        "sealed": 1,
        "generation": 0,
        "epoch": 0,
        "delete_token_hash": CAP,
    })
}

fn catch_up_body(conversation_id: &str) -> serde_json::Value {
    serde_json::json!({
        "conversation_id": conversation_id,
        "want_envelopes": true,
        "want_dm": true,
        "want_membership_version": true,
        "user_id": "alice",
        "device_id": "a1",
    })
}

/// A channel and a DM send land and come back on catch-up, over hrana, with no
/// read anywhere paying a describe.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn sends_and_catch_ups_work_over_hrana() {
    let fx = fixture().await;
    // Warm the device-key cache, so the counts below are the endpoint's own.
    fx.call("/v1/conversations/catch-up", catch_up_body("c")).await;

    for (conv, id) in [("c", "m-chan"), ("d", "m-dm")] {
        fx.reset();
        let (status, body) = fx.call("/v1/messages/send", send_body(id, conv)).await;
        assert_eq!(status, StatusCode::OK, "send to {conv}: {body}");
        fx.assert_no_describe("send");
        let (main, log) = fx.round_trips();
        println!("REMOTE send ({conv}): main_rt={main} log_rt={log}");

        // A replay is answered from the stored row, and writes nothing.
        let (status, _) = fx.call("/v1/messages/send", send_body(id, conv)).await;
        assert_eq!(status, StatusCode::OK, "replay to {conv}");

        fx.reset();
        let (status, body) = fx.call("/v1/conversations/catch-up", catch_up_body(conv)).await;
        assert_eq!(status, StatusCode::OK, "catch-up {conv}: {body}");
        fx.assert_no_describe("catch-up");
        let (main, log) = fx.round_trips();
        println!("REMOTE catch-up ({conv}): main_rt={main} log_rt={log}");
        assert_eq!(body["authorized"], true, "{body}");
        let envelopes = body["envelopes"].as_array().expect("envelopes");
        assert_eq!(envelopes.len(), 1, "{body}");
        assert_eq!(envelopes[0]["id"], id, "{body}");
        assert_eq!(envelopes[0]["seq"], 1, "{body}");
    }

    // A non-member's catch-up is refused without an envelope read.
    let (status, body) = fx.call("/v1/conversations/catch-up", catch_up_body("nowhere")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authorized"], false, "{body}");
}

fn commit_body(conv: &str, epoch: i64, sender: &str, welcome_for: Option<&str>) -> SubmitBody {
    SubmitBody {
        conversation_id: conv.to_string(),
        generation: 0,
        based_on_epoch: epoch,
        closes_epoch: None,
        sender_id: sender.to_string(),
        commit: b64(format!("commit-{conv}-{epoch}-{sender}").as_bytes()),
        added_user_id: None,
        added_device_ids: None,
        group_info: Some(b64(b"group-info")),
        welcomes: welcome_for
            .map(|user| {
                vec![WelcomeBody {
                    recipient_id: user.to_string(),
                    recipient_device_id: "b1".to_string(),
                    welcome: b64(b"welcome-blob"),
                }]
            })
            .unwrap_or_default(),
    }
}

async fn submit(fx: &Fixture, body: &SubmitBody) -> SubmitResponse {
    let main = fx.main.db.conn().await.unwrap();
    let log = fx.log.db.conn().await.unwrap();
    match submit_commit_with(&main, &log, body, LeafCheck::Skip)
        .await
        .expect("submit")
    {
        SubmitVerdict::Response(r) => r,
        other => panic!("expected a CAS response, got {other:?}"),
    }
}

/// The commit CAS over hrana: the head is accepted with its Welcome in the same
/// transaction, a stale submit takes the Rejected branch (whose head and missing
/// commits are SELECTs inside the write transaction) and writes nothing, and a
/// gap is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn commit_accept_and_reject_work_over_hrana() {
    let fx = fixture().await;

    fx.reset();
    match submit(&fx, &commit_body("g", 0, "alice", Some("bob"))).await {
        SubmitResponse::Accepted { epoch, .. } => assert_eq!(epoch, 0),
        other => panic!("expected Accepted, got {other:?}"),
    }
    fx.assert_no_describe("commit accept");

    fx.reset();
    match submit(&fx, &commit_body("g", 0, "bob", None)).await {
        SubmitResponse::Rejected { head, missing, .. } => {
            assert_eq!(head, 1);
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].epoch, 0);
        }
        other => panic!("expected Rejected, got {other:?}"),
    }
    fx.assert_no_describe("commit reject");
    let rejected_sql: Vec<String> = fx.log.proxy.log().into_iter().flat_map(|f| f.sql).collect();
    assert!(
        rejected_sql.iter().any(|s| s.starts_with("ROLLBACK")),
        "the Rejected branch rolls back: {rejected_sql:?}"
    );

    match submit(&fx, &commit_body("g", 5, "alice", None)).await {
        SubmitResponse::Rejected { head, .. } => assert_eq!(head, 1),
        other => panic!("expected Rejected, got {other:?}"),
    }

    let log = fx.log.db.conn().await.unwrap();
    assert_eq!(head_epoch(&log, "g").await.unwrap(), 1);
    let commits = fetch_commits(&log, "g", 0, 0).await.unwrap();
    assert_eq!(commits.len(), 1, "exactly the one accepted commit");
    assert_eq!(commits[0].sender_id, "alice");
}

/// The Welcome the accepted commit carried is fetched by its device over hrana.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn a_welcome_is_fetched_over_hrana() {
    let fx = fixture().await;
    let bob = common::gen_signing_key();
    let pub_pq = {
        use ml_dsa::Keypair as _;
        bob.verifying_key().encode().to_vec()
    };
    fx.main
        .db
        .conn()
        .await
        .unwrap()
        .execute(
            "UPDATE user_device SET mls_signature_pub_pq = ?1 WHERE user_id = 'bob' AND device_id = 'b1'",
            libsql::params![pub_pq],
        )
        .await
        .unwrap();

    match submit(&fx, &commit_body("g", 0, "alice", Some("bob"))).await {
        SubmitResponse::Accepted { .. } => {}
        other => panic!("expected Accepted, got {other:?}"),
    }

    fx.reset();
    let (status, body) = common::send_signed(
        &fx.state,
        "/v1/welcomes/fetch",
        serde_json::json!({ "user_id": "bob", "device_id": "b1" }),
        "bob",
        "b1",
        &bob,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    fx.assert_no_describe("welcome fetch");
    let welcomes = body["welcomes"].as_array().expect("welcomes");
    assert_eq!(welcomes.len(), 1, "{body}");
}

/// #2179 fault injection on the commit path: the COMMIT reaches the server and
/// is applied, then the socket dies before the answer comes back. The DS
/// answers 503 (a transient fault), the next request goes out on a NEW socket,
/// and the log holds exactly the submitter's commit at that epoch, byte for
/// byte, so the client's reconcile (`our_commit_is_canonical`) adopts it; a
/// resubmission is refused with that same commit as the head. Never a fork.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn a_commit_whose_answer_is_lost_is_adopted_never_forked() {
    use axum::response::IntoResponse as _;
    let fx = fixture().await;
    let body = commit_body("g", 0, "alice", None);

    // Warm the log database's socket pool.
    fx.log.db.conn().await.unwrap().query("SELECT 1", ()).await.unwrap();
    let sockets_before = fx.log.proxy.sockets();

    fx.log.proxy.cut_after_request_containing(r#""sql":"COMMIT""#);
    let main = fx.main.db.conn().await.unwrap();
    let log = fx.log.db.conn().await.unwrap();
    let err = submit_commit_with(&main, &log, &body, LeafCheck::Skip)
        .await
        .expect_err("the COMMIT's answer never arrives");
    assert!(fx.log.proxy.did_cut(), "the cut happened after the COMMIT went out");
    assert!(
        pollis_delivery::error::is_transient_db_error(&err),
        "a lost answer is transient, so the client retries: {err:#}"
    );
    // What the handler's `?` does with it.
    let response = pollis_delivery::error::AppError::from(err).into_response();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop((main, log));

    // The commit landed: exactly one, the submitter's own bytes.
    let log = fx.log.db.conn().await.unwrap();
    let commits = fetch_commits(&log, "g", 0, 0).await.expect("a read after the fault");
    assert!(
        fx.log.proxy.sockets() > sockets_before,
        "the request after the fault opened a new socket"
    );
    assert_eq!(commits.len(), 1, "{commits:?}");
    assert_eq!(commits[0].commit, body.commit, "the stored commit is the submitter's own");
    drop(log);

    match submit(&fx, &body).await {
        SubmitResponse::Rejected { head, missing, .. } => {
            assert_eq!(head, 1);
            assert_eq!(missing.len(), 1);
            assert_eq!(missing[0].commit, body.commit, "the head is the client's own commit");
        }
        other => panic!("a resubmit must not land a second commit at epoch 0: {other:?}"),
    }
    let log = fx.log.db.conn().await.unwrap();
    assert_eq!(fetch_commits(&log, "g", 0, 0).await.unwrap().len(), 1, "no fork");
}

/// #2179: requests after a lull reuse the database's warm socket.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn sends_after_a_lull_reuse_the_warm_socket() {
    let fx = fixture().await;
    fx.call("/v1/conversations/catch-up", catch_up_body("c")).await;
    let (main_before, log_before) = (fx.main.proxy.sockets(), fx.log.proxy.sockets());
    for i in 0..2 {
        // Past the 3 s a parked connection may idle: its stream is discarded.
        tokio::time::sleep(std::time::Duration::from_millis(3200)).await;
        let (status, body) = fx
            .call("/v1/messages/send", send_body(&format!("lull-{i}"), "c"))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    assert_eq!(fx.main.proxy.sockets(), main_before, "no new main socket after a lull");
    assert_eq!(fx.log.proxy.sockets(), log_before, "no new log socket after a lull");
}

/// #2180's delivery-sequence invariants, over hrana on a real sqld: the
/// concurrency is the server's, not a local file's.
#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "needs a local sqld; see support/sqld.rs"]
async fn delivery_sequence_invariants_hold_over_hrana() {
    let r = RemoteDb::single_db().await;
    let db = r.db.as_ref();
    seq_invariants::an_insert_takes_the_next_counter_value(db).await;
    seq_invariants::concurrent_sends_to_one_conversation_are_dense_and_unique(Arc::clone(&r.db), 24).await;
    seq_invariants::the_counter_outlives_gc_emptying_a_conversation(db).await;
    seq_invariants::a_teardown_restarts_the_sequence_with_its_cursors(db).await;
    seq_invariants::a_failed_insert_consumes_no_seq(db).await;
    seq_invariants::an_explicit_seq_must_be_the_counter(db).await;
    seq_invariants::a_multi_row_insert_gets_consecutive_seqs(db).await;
    seq_invariants::old_and_new_ds_writers_interleave_safely(Arc::clone(&r.db), 12).await;
    seq_invariants::without_the_triggers_a_send_fails_closed(db).await;
}
