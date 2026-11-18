//! #2005 — App approvals on the DS: who may store one, that `seq` moves by
//! exactly one, that stored records never change, who reads them, the
//! runtime's report, and that everything goes with the bot. The DS verifies no
//! record signature (it holds no admin key); the runtime does, and its refusals
//! are in the flows suite (`src-tauri/tests/flows/bot_apps.rs`).

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use libsql::Connection;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use pollis_api::bot_apps::*;
use pollis_api::bots::*;
use pollis_delivery::bot_apps;
use pollis_delivery::bots::{self, CreateBotOutcome, PairingRequestOutcome};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use serde_json::json;
use sha2::{Digest, Sha256};
use tower::ServiceExt as _;

mod common;

const NOW: u64 = 1_900_000_000;

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    OsRng.fill_bytes(&mut out);
    out
}

struct Dev {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
}

struct World {
    db: common::TempDb,
    c: pollis_delivery::db::ConnGuard,
    alice: Dev,
    bob: Dev,
    carol: Dev,
    bot: String,
    host: Dev,
}

async fn exec(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut r = c.query(sql, ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn device(c: &Connection, user_id: &str, device_id: &str) -> Dev {
    let key = common::gen_signing_key();
    c.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3) \
         ON CONFLICT(device_id) DO UPDATE SET mls_signature_pub_pq = excluded.mls_signature_pub_pq",
        libsql::params![device_id, user_id, key.verifying_key().encode().to_vec()],
    )
    .await
    .unwrap();
    Dev { user_id: user_id.into(), device_id: device_id.into(), key }
}

/// alice: admin of g1. bob: member of g1. carol: admin of g2 only. A bot in g1 with one host.
async fn world() -> World {
    let db = common::TempDb::open("delivery.db").await;
    let c = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&c).await.expect("schema");
    for sql in [
        "INSERT INTO users (id, email, username) VALUES ('alice', 'a@x.com', 'alice')",
        "INSERT INTO users (id, email, username) VALUES ('bob', 'b@x.com', 'bob')",
        "INSERT INTO users (id, email, username) VALUES ('carol', 'c@x.com', 'carol')",
        "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'One', 'alice')",
        "INSERT INTO conversation (id, kind) VALUES ('g2', 'group')",
        "INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Two', 'carol')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'carol', 'admin')",
    ] {
        exec(&c, sql).await;
    }
    let alice = device(&c, "alice", "alice-d").await;
    let bob = device(&c, "bob", "bob-d").await;
    let carol = device(&c, "carol", "carol-d").await;

    let secret = random::<32>();
    let request = BotPairingRequestBody {
        code_lookup: b64(&random::<32>()),
        poll_verifier: b64(&Sha256::digest(secret)),
        ephemeral_pub: b64(&random::<32>()),
        device_id: format!("host-{}", ulid::Ulid::new()),
        device_name: Some("rack-1".into()),
        target: PairingTarget::NewBot { account_id_pub: b64(&[7u8; 1312]) },
    };
    let PairingRequestOutcome::Created { request_id, .. } = bots::apply_pairing_request(&c, &request, NOW).await.unwrap()
    else {
        panic!("pairing request refused");
    };
    let create = CreateBotBody {
        group_id: "g1".into(),
        request_id,
        ephemeral_pub: request.ephemeral_pub.clone(),
        name: "Triage".into(),
        description: None,
        user_id: None,
    };
    let (bot, host_id) = match bots::apply_create_bot(&c, "alice", "alice-d", &create, NOW).await.unwrap() {
        CreateBotOutcome::Created { bot_user_id, device_id, .. } => (bot_user_id, device_id),
        other => panic!("bot creation refused: {other:?}"),
    };
    let host = device(&c, &bot, &host_id).await;
    World { db, c, alice, bob, carol, bot, host }
}

fn signed(path: &str, who: &Dev, body: &serde_json::Value) -> Request<Body> {
    let bytes = serde_json::to_vec(body).unwrap();
    let ts = pollis_delivery::util::now_unix() as i64;
    let msg = pollis_delivery::auth::canonical_message("POST", path, ts, &bytes);
    let sig = b64(&who.key.sign(&msg).encode());
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", who.user_id.clone())
        .header("X-Pollis-Device", who.device_id.clone())
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(bytes))
        .unwrap()
}

async fn call(w: &World, who: &Dev, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = build_router_with_state(AppState::new(Arc::clone(&w.db), true))
        .oneshot(signed(path, who, &body))
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

fn approve_body(bot: &str, seq: i64) -> serde_json::Value {
    json!({
        "bot_user_id": bot,
        "seq": seq,
        "kind": "production",
        "record": b64(format!("signed record {seq}").as_bytes()),
        "manifest": "{\"name\":\"triage\",\"version\":\"1.0.0\"}",
    })
}

fn body_of(v: serde_json::Value) -> ApproveBotAppBody {
    serde_json::from_value(v).unwrap()
}

#[tokio::test]
async fn only_a_current_admin_of_the_bots_group_stores_an_approval() {
    let w = world().await;
    let (s, v) = call(&w, &w.bob, "/v1/bots/app/approve", approve_body(&w.bot, 1)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "a member who is not an admin: {v}");
    let (s, _) = call(&w, &w.carol, "/v1/bots/app/approve", approve_body(&w.bot, 1)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "an admin of another group");
    let (s, _) = call(&w, &w.host, "/v1/bots/app/approve", approve_body(&w.bot, 1)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the bot approves nothing for itself");
    let (s, v) = call(&w, &w.alice, "/v1/bots/app/approve", approve_body(&w.bot, 1)).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["outcome"], "approved");
    assert_eq!(
        count(&w.c, "SELECT COUNT(*) FROM security_event WHERE kind = 'bot_app_approved'").await,
        2,
        "on the admin and on the bot"
    );

    // Demoted: no longer.
    exec(&w.c, "UPDATE group_member SET role = 'member' WHERE group_id = 'g1' AND user_id = 'alice'").await;
    let (s, _) = call(&w, &w.alice, "/v1/bots/app/approve", approve_body(&w.bot, 2)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn seq_moves_by_exactly_one_and_a_stale_approval_stores_nothing() {
    let w = world().await;
    let ok = |r: Result<ApproveBotAppResponse, WriteOutcome>| r.unwrap();
    let a = |seq| body_of(approve_body(&w.bot, seq));
    assert_eq!(
        ok(bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &a(2)).await.unwrap()),
        ApproveBotAppResponse::Stale { latest: 0 }
    );
    assert_eq!(ok(bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &a(1)).await.unwrap()), ApproveBotAppResponse::Approved);
    assert_eq!(
        ok(bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &a(1)).await.unwrap()),
        ApproveBotAppResponse::Stale { latest: 1 },
        "two admins approving at once: the second reloads"
    );
    assert_eq!(ok(bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &a(2)).await.unwrap()), ApproveBotAppResponse::Approved);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot_app_approval").await, 2);
}

#[tokio::test]
async fn the_schema_refuses_a_skipped_seq_an_edit_a_delete_and_a_non_bot() {
    let w = world().await;
    let insert = |seq: i64, who: &str| {
        format!(
            "INSERT INTO bot_app_approval (installation_id, seq, kind, record, manifest, approver_user_id, approver_device_id) \
             VALUES ('{who}', {seq}, 'production', 'cmVj', '{{}}', 'alice', 'alice-d')"
        )
    };
    assert!(w.c.execute(&insert(2, &w.bot), ()).await.is_err(), "a skipped seq");
    assert!(w.c.execute(&insert(1, "alice"), ()).await.is_err(), "not a bot");
    exec(&w.c, &insert(1, &w.bot)).await;
    assert!(w.c.execute(&insert(1, &w.bot), ()).await.is_err(), "a repeated seq");
    assert!(
        w.c.execute("UPDATE bot_app_approval SET record = 'b3RoZXI='", ()).await.is_err(),
        "a stored record never changes"
    );
    assert!(
        w.c.execute("DELETE FROM bot_app_approval", ()).await.is_err(),
        "a record goes only with its bot (the DS cannot roll an installation back by deleting)"
    );
    assert!(
        w.c.execute(
            &format!(
                "INSERT INTO bot_app_approval (installation_id, seq, kind, record, manifest, approver_user_id, approver_device_id) \
                 VALUES ('{}', 2, 'debug', 'cmVj', '{{}}', 'alice', 'alice-d')",
                w.bot
            ),
            ()
        )
        .await
        .is_err(),
        "an unknown kind"
    );
    assert!(
        w.c.execute(
            "INSERT INTO bot_app_report (installation_id, state, reason, detail, device_id) VALUES ('alice', 'running', '', '', 'd')",
            ()
        )
        .await
        .is_err(),
        "a report for a person"
    );
}

#[tokio::test]
async fn members_read_approvals_and_only_admins_read_the_runtimes_report() {
    let w = world().await;
    let (s, _) = call(&w, &w.alice, "/v1/bots/app/approve", approve_body(&w.bot, 1)).await;
    assert_eq!(s, StatusCode::OK);
    let report = json!({
        "installation_id": w.bot,
        "state": "awaiting_approval",
        "reason": "hash_mismatch",
        "detail": "a new version",
        "app_hash": "ab".repeat(32),
        "manifest": "{\"name\":\"triage\",\"version\":\"2.0.0\"}",
        "source": "https://apps.example/triage.wasm",
    });
    let (s, v) = call(&w, &w.host, "/v1/bots/app/report", report.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, _) = call(&w, &w.alice, "/v1/bots/app/report", report.clone()).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "only the bot reports for itself");

    let read = json!({ "group_id": "g1" });
    let (s, v) = call(&w, &w.alice, "/v1/read/bot-apps", read.clone()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["apps"][0]["approval"]["seq"], 1);
    assert_eq!(v["apps"][0]["report"]["reason"], "hash_mismatch");

    let (s, v) = call(&w, &w.bob, "/v1/read/bot-apps", read.clone()).await;
    assert_eq!(s, StatusCode::OK, "a member reads the approved manifest (slash commands, the marks)");
    assert_eq!(v["apps"][0]["approval"]["seq"], 1);
    assert!(v["apps"][0]["report"].is_null(), "but not the runtime's report");

    let (s, v) = call(&w, &w.host, "/v1/read/bot-apps", json!({ "group_id": "g1", "bot_user_id": w.bot })).await;
    assert_eq!(s, StatusCode::OK, "the bot's own host reads its approval");
    assert_eq!(v["apps"].as_array().unwrap().len(), 1);

    let (s, _) = call(&w, &w.carol, "/v1/read/bot-apps", read).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "not a member of the group");
}

#[tokio::test]
async fn a_report_is_validated() {
    let w = world().await;
    let base = ApproveBotAppBody {
        bot_user_id: w.bot.clone(),
        seq: 1,
        kind: AppApprovalKind::Development,
        record: "not base64!".into(),
        manifest: "{}".into(),
        user_id: None,
    };
    assert!(matches!(
        bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &base).await.unwrap(),
        Err(WriteOutcome::Invalid(_))
    ));
    let report = |hash: &str| BotAppReportBody {
        installation_id: w.bot.clone(),
        state: BotAppRunState::Refused,
        reason: "bad_signature".into(),
        detail: String::new(),
        app_hash: Some(hash.into()),
        manifest: None,
        source: None,
        developer_key: None,
    };
    assert!(matches!(
        bot_apps::apply_report_bot_app(&w.c, &w.bot, &w.host.device_id, &report("xyz")).await.unwrap(),
        WriteOutcome::Invalid(_)
    ));
    assert!(matches!(
        bot_apps::apply_report_bot_app(&w.c, &w.bot, &w.host.device_id, &report(&"0".repeat(64))).await.unwrap(),
        WriteOutcome::Ok
    ));
    assert!(matches!(
        bot_apps::apply_report_bot_app(&w.c, &w.bot, &w.host.device_id, &report(&"1".repeat(64))).await.unwrap(),
        WriteOutcome::Ok
    ));
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot_app_report").await, 1, "one report per installation, replaced");
}

#[tokio::test]
async fn approvals_and_reports_go_with_the_bot() {
    let w = world().await;
    bot_apps::apply_approve_bot_app(&w.c, "alice", "alice-d", &body_of(approve_body(&w.bot, 1)))
        .await
        .unwrap()
        .unwrap();
    exec(
        &w.c,
        &format!("INSERT INTO bot_app_report (installation_id, state, reason, detail, device_id) VALUES ('{}', 'running', '', '', 'd')", w.bot),
    )
    .await;
    let tx = pollis_delivery::db::write_tx(&w.c).await.unwrap();
    bots::delete_bot_in_tx(&tx, &w.bot).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot_app_approval").await, 0);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot_app_report").await, 0);
}
