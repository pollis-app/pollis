//! #1182 — the bot endpoints and every DS refusal for a bot, each tried from
//! the side that would break it. The schema's own refusals are in
//! `bot_schema.rs`; these are the DS layer above them.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use libsql::Connection;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use pollis_api::bots::*;
use pollis_delivery::bots::{self, CreateBotOutcome, PairingRequestOutcome, WrapOutcome};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
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
}

async fn exec(c: &Connection, sql: &str) {
    c.execute(sql, ()).await.unwrap_or_else(|e| panic!("{sql}: {e}"));
}

async fn count(c: &Connection, sql: &str) -> i64 {
    let mut r = c.query(sql, ()).await.unwrap();
    r.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

/// A device with a published signing key, so it can sign through the router.
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

/// alice: admin of g1. bob: member of g1. carol: admin of g2 only.
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
        "INSERT INTO conversation (id, kind) VALUES ('ch1', 'channel')",
        "INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'General')",
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
    World { db, c, alice, bob, carol }
}

/// A filed pairing request and what its host keeps.
struct Pairing {
    request_id: String,
    code_lookup: String,
    ephemeral_pub: String,
    poll_secret: String,
    device_id: String,
}

fn pairing_body(target: PairingTarget) -> (BotPairingRequestBody, [u8; 32]) {
    let secret = random::<32>();
    let verifier: [u8; 32] = Sha256::digest(secret).into();
    let body = BotPairingRequestBody {
        code_lookup: b64(&random::<32>()),
        poll_verifier: b64(&verifier),
        ephemeral_pub: b64(&random::<32>()),
        device_id: format!("host-{}", ulid::Ulid::new()),
        device_name: Some("rack-1".into()),
        target,
    };
    (body, secret)
}

fn new_bot_target() -> PairingTarget {
    PairingTarget::NewBot { account_id_pub: b64(&[7u8; 1312]) }
}

async fn file(c: &Connection, target: PairingTarget) -> Pairing {
    let (body, secret) = pairing_body(target);
    let PairingRequestOutcome::Created { request_id, .. } = bots::apply_pairing_request(c, &body, NOW).await.unwrap()
    else {
        panic!("pairing request refused");
    };
    Pairing {
        request_id,
        code_lookup: body.code_lookup,
        ephemeral_pub: body.ephemeral_pub,
        poll_secret: b64(&secret),
        device_id: body.device_id,
    }
}

fn create_body(p: &Pairing, group: &str) -> CreateBotBody {
    CreateBotBody {
        group_id: group.into(),
        request_id: p.request_id.clone(),
        ephemeral_pub: p.ephemeral_pub.clone(),
        name: "Grafana Alerts".into(),
        description: Some("Posts alerts".into()),
        user_id: None,
    }
}

/// A bot in g1 created by alice; returns `(bot_user_id, host device id)`.
async fn make_bot(w: &World) -> (String, String) {
    let p = file(&w.c, new_bot_target()).await;
    match bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), NOW).await.unwrap() {
        CreateBotOutcome::Created { bot_user_id, device_id, .. } => (bot_user_id, device_id),
        other => panic!("bot creation refused: {other:?}"),
    }
}

/// The bot's host, given a signing key as it would have after publishing its cert.
async fn bot_host(w: &World, bot: &str, device_id: &str) -> Dev {
    device(&w.c, bot, device_id).await
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

async fn respond(w: &World, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = build_router_with_state(AppState::new(Arc::clone(&w.db), true)).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

async fn call(w: &World, who: &Dev, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    respond(w, signed(path, who, &body)).await
}

async fn call_open(w: &World, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    respond(w, req).await
}

// ================================================================ pairing

#[tokio::test]
async fn a_pairing_request_is_validated_and_its_code_is_unique_while_open() {
    let w = world().await;
    let (mut body, _) = pairing_body(new_bot_target());
    body.ephemeral_pub = b64(&[1u8; 31]);
    assert!(matches!(bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(), PairingRequestOutcome::Invalid(_)));

    let (mut body, _) = pairing_body(PairingTarget::NewBot { account_id_pub: b64(&[7u8; 32]) });
    body.device_id = "d".into();
    assert!(
        matches!(bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(), PairingRequestOutcome::Invalid(_)),
        "an account key that is not ML-DSA-44 is refused"
    );

    let (body, _) = pairing_body(new_bot_target());
    assert!(matches!(bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(), PairingRequestOutcome::Created { .. }));
    assert_eq!(
        bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(),
        PairingRequestOutcome::CodeTaken
    );
    // Once it lapses, its code is free again.
    let later = NOW + bots::PAIRING_TTL_SECS + 1;
    assert!(matches!(bots::apply_pairing_request(&w.c, &body, later).await.unwrap(), PairingRequestOutcome::Created { .. }));
}

#[tokio::test]
async fn a_pairing_request_for_an_unknown_bot_or_a_human_is_refused() {
    let w = world().await;
    for target in ["nobody", "alice"] {
        let (body, _) = pairing_body(PairingTarget::ExistingBot { bot_user_id: target.into() });
        assert_eq!(
            bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(),
            PairingRequestOutcome::Invalid("unknown bot")
        );
    }
}

#[tokio::test]
async fn open_pairing_requests_are_capped() {
    let w = world().await;
    exec(
        &w.c,
        &format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {}) \
             INSERT INTO bot_pairing_request (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, account_id_pub, expires_at) \
             SELECT 'r' || i, 'create', randomblob(32), zeroblob(32), zeroblob(32), 'd' || i, zeroblob(1312), {} FROM n",
            bots::MAX_OPEN_PAIRINGS,
            NOW + 60
        ),
    )
    .await;
    let (body, _) = pairing_body(new_bot_target());
    assert_eq!(bots::apply_pairing_request(&w.c, &body, NOW).await.unwrap(), PairingRequestOutcome::Full);
}

#[tokio::test]
async fn the_pairing_endpoints_need_no_credential_and_answer_one_refusal() {
    let w = world().await;
    let (body, secret) = pairing_body(new_bot_target());
    let (s, v) = call_open(&w, "/v1/bots/pairing/request", serde_json::to_value(&body).unwrap()).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let request_id = v["request_id"].as_str().unwrap().to_string();

    let (s, v) = call_open(&w, "/v1/bots/pairing/status", serde_json::json!({ "request_id": request_id, "poll_secret": b64(&secret) })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["status"], "pending");
    for (id, secret) in [(request_id.as_str(), b64(&[0u8; 32])), ("nope", b64(&secret))] {
        let (s, _) = call_open(&w, "/v1/bots/pairing/status", serde_json::json!({ "request_id": id, "poll_secret": secret })).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "a wrong secret and an unknown id answer alike");
    }
}

#[tokio::test]
async fn only_an_admin_looks_a_pairing_up_and_only_through_the_bots_group() {
    let w = world().await;
    let p = file(&w.c, new_bot_target()).await;
    let lookup = |group: &str, who: &str| BotPairingLookupBody {
        group_id: group.into(),
        code_lookup: p.code_lookup.clone(),
        user_id: Some(who.into()),
    };
    assert!(bots::apply_pairing_lookup(&w.c, Some("bob"), &lookup("g1", "bob"), NOW).await.unwrap().is_err());
    assert!(bots::apply_pairing_lookup(&w.c, Some("alice"), &lookup("g2", "alice"), NOW).await.unwrap().is_err());
    let found = bots::apply_pairing_lookup(&w.c, Some("alice"), &lookup("g1", "alice"), NOW).await.unwrap().unwrap().unwrap();
    assert_eq!(found.ephemeral_pub, p.ephemeral_pub);
    assert_eq!(found.request_id, p.request_id);
    let late = NOW + bots::PAIRING_TTL_SECS;
    assert!(bots::apply_pairing_lookup(&w.c, Some("alice"), &lookup("g1", "alice"), late).await.unwrap().unwrap().is_none());

    // An add-host request is visible to the admins of its bot's group only.
    let (bot, _) = make_bot(&w).await;
    let add = file(&w.c, PairingTarget::ExistingBot { bot_user_id: bot.clone() }).await;
    let by = |group: &str, who: &str| BotPairingLookupBody {
        group_id: group.into(),
        code_lookup: add.code_lookup.clone(),
        user_id: Some(who.into()),
    };
    assert!(bots::apply_pairing_lookup(&w.c, Some("carol"), &by("g2", "carol"), NOW).await.unwrap().unwrap().is_none());
    let found = bots::apply_pairing_lookup(&w.c, Some("alice"), &by("g1", "alice"), NOW).await.unwrap().unwrap().unwrap();
    assert_eq!(found.target, PairingTarget::ExistingBot { bot_user_id: bot });
}

// ================================================================ creation

#[tokio::test]
async fn an_admin_creates_a_bot_in_one_transaction() {
    let w = world().await;
    let p = file(&w.c, new_bot_target()).await;
    let CreateBotOutcome::Created { bot_user_id: bot, device_id, group_id } =
        bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), NOW).await.unwrap()
    else {
        panic!("refused");
    };
    assert_eq!(group_id, "g1");
    assert_eq!(device_id, p.device_id);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM users WHERE id = '{bot}' AND kind = 'bot' AND email = 'bot-{}@bot.invalid' AND preferred_name = 'Grafana Alerts'", bot.to_lowercase())).await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM group_member WHERE user_id = '{bot}' AND group_id = 'g1' AND role = 'member'")).await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM bot WHERE user_id = '{bot}' AND created_by = 'alice'")).await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM user_device WHERE user_id = '{bot}' AND device_id = '{device_id}'")).await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM account_key_log WHERE user_id = '{bot}' AND identity_version = 1")).await, 1, "published like any account's key");
    assert_eq!(count(&w.c, &String::from("SELECT COUNT(*) FROM security_event WHERE user_id = 'alice' AND kind = 'bot_created'")).await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM security_event WHERE user_id = '{bot}' AND kind = 'bot_created'")).await, 1);

    // The host reads the outcome with its secret.
    let status = bots::apply_pairing_status(
        &w.c,
        &BotPairingStatusBody { request_id: p.request_id.clone(), poll_secret: p.poll_secret.clone() },
        NOW + 1,
    )
    .await
    .unwrap();
    assert_eq!(status, Some(BotPairingStatus::Created { bot_user_id: bot, group_id: "g1".into() }));
}

#[tokio::test]
async fn bot_creation_refuses_a_non_admin_a_swapped_key_and_a_spent_request() {
    let w = world().await;
    let p = file(&w.c, new_bot_target()).await;
    assert_eq!(bots::apply_create_bot(&w.c, "bob", "bob-d", &create_body(&p, "g1"), NOW).await.unwrap(), CreateBotOutcome::Forbidden);
    assert_eq!(bots::apply_create_bot(&w.c, "carol", "carol-d", &create_body(&p, "g1"), NOW).await.unwrap(), CreateBotOutcome::Forbidden);

    let mut swapped = create_body(&p, "g1");
    swapped.ephemeral_pub = b64(&random::<32>());
    assert_eq!(bots::apply_create_bot(&w.c, "alice", "alice-d", &swapped, NOW).await.unwrap(), CreateBotOutcome::Gone);

    let late = NOW + bots::PAIRING_TTL_SECS;
    assert_eq!(bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), late).await.unwrap(), CreateBotOutcome::Gone);

    assert!(matches!(bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), NOW).await.unwrap(), CreateBotOutcome::Created { .. }));
    assert_eq!(
        bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), NOW).await.unwrap(),
        CreateBotOutcome::Gone,
        "a request creates one bot"
    );
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot").await, 1);
}

#[tokio::test]
async fn a_group_holds_at_most_the_bot_limit() {
    let w = world().await;
    for _ in 0..MAX_BOTS_PER_GROUP {
        make_bot(&w).await;
    }
    let p = file(&w.c, new_bot_target()).await;
    assert_eq!(
        bots::apply_create_bot(&w.c, "alice", "alice-d", &create_body(&p, "g1"), NOW).await.unwrap(),
        CreateBotOutcome::LimitReached
    );
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot").await, i64::from(MAX_BOTS_PER_GROUP));
}

#[tokio::test]
async fn the_bots_page_lists_bots_and_hosts_for_admins_only() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let body = |who: &str| ListBotsBody { group_id: "g1".into(), user_id: Some(who.into()) };
    assert!(bots::apply_list_bots(&w.c, Some("bob"), &body("bob")).await.unwrap().is_none());
    let list = bots::apply_list_bots(&w.c, Some("alice"), &body("alice")).await.unwrap().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].user_id, bot);
    assert_eq!(list[0].preferred_name.as_deref(), Some("Grafana Alerts"));
    assert_eq!(list[0].hosts.len(), 1);
    assert_eq!(list[0].hosts[0].device_id, host);
    assert!(!list[0].hosts[0].certified, "no cert until the host publishes one");
}

// ================================================================ deletion

#[tokio::test]
async fn an_admin_deletes_a_bot_through_account_deletion() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    bots::apply_claim_trigger(&w.c, &bot, &host, "env-1", NOW).await.unwrap().unwrap();
    assert!(matches!(bots::apply_delete_bot(&w.c, "bob", "bob-d", &bot).await.unwrap(), WriteOutcome::Forbidden));
    assert!(matches!(bots::apply_delete_bot(&w.c, "carol", "carol-d", &bot).await.unwrap(), WriteOutcome::Forbidden));
    assert!(matches!(bots::apply_delete_bot(&w.c, "alice", "alice-d", &bot).await.unwrap(), WriteOutcome::Ok));
    for table in ["users WHERE id", "bot WHERE user_id", "group_member WHERE user_id", "user_device WHERE user_id", "bot_trigger_claim WHERE installation_id"] {
        assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM {table} = '{bot}'")).await, 0, "{table}");
    }
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM groups WHERE id = 'g1'").await, 1, "the group stays");
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM account_log_purge WHERE user_id = '{bot}'")).await, 1, "its commit-log half is owed");
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM security_event WHERE user_id = 'alice' AND kind = 'bot_deleted'").await, 1);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM account_key_log").await, 1, "the transparency log keeps its key");
}

#[tokio::test]
async fn a_bot_may_delete_its_own_account() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let body = pollis_api::account::DeleteAccountBody { user_id: Some(bot.clone()), withdraw: Vec::new() };
    let (outcome, _) = pollis_delivery::account::apply_delete_account(&w.c, Some(&bot), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot").await, 0);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM groups WHERE id = 'g1'").await, 1);
}

#[tokio::test]
async fn deleting_a_group_deletes_its_bots() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let body = pollis_api::groups::DeleteGroupBody { group_id: "g1".into(), requester_id: None };
    let (outcome, _) = pollis_delivery::groups::apply_delete_group(&w.c, Some("alice"), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM users WHERE id = '{bot}'")).await, 0);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM user_device WHERE user_id = '{bot}'")).await, 0);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot").await, 0);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM groups WHERE id = 'g1'").await, 0);
}

#[tokio::test]
async fn the_last_person_leaving_takes_the_groups_bots_with_it() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    exec(&w.c, "DELETE FROM group_member WHERE user_id = 'bob'").await;
    let body = pollis_api::groups::LeaveGroupBody { group_id: "g1".into(), user_id: None };
    let (outcome, dead) = pollis_delivery::groups::apply_leave_group(&w.c, Some("alice"), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert!(dead.contains(&"g1".to_string()));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM users WHERE id = '{bot}'")).await, 0);
}

#[tokio::test]
async fn account_deletion_never_hands_a_group_to_a_bot() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    // alice is the only admin; bob and the bot remain. bob is promoted, never the bot.
    let body = pollis_api::account::DeleteAccountBody { user_id: None, withdraw: Vec::new() };
    let (outcome, _) = pollis_delivery::account::apply_delete_account(&w.c, Some("alice"), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM group_member WHERE user_id = 'bob' AND role = 'admin'").await, 1);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM groups WHERE id = 'g1' AND owner_id = 'bob'").await, 1);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM bot WHERE user_id = '{bot}' AND created_by IS NULL")).await, 1, "the bot stays, its creator pointer blanked");

    // bob, the last person, leaves by deleting his account: the bot goes with the group.
    let (outcome, _) = pollis_delivery::account::apply_delete_account(&w.c, Some("bob"), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot").await, 0);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM groups WHERE id = 'g1'").await, 0);
}

// ================================================================ adding a host

async fn approve(w: &World, who: &str, device: &str, bot: &str, request: &str) -> WriteOutcome {
    let body = ApproveBotHostBody { bot_user_id: bot.into(), request_id: request.into(), host_approval: b64(b"signed approval") };
    bots::apply_approve_bot_host(&w.c, who, device, &body, NOW).await.unwrap()
}

fn wrap(request: &str) -> WrapBotHostBody {
    WrapBotHostBody { request_id: request.into(), wrapped_account_key: b64(&[5u8; 92]) }
}

#[tokio::test]
async fn a_host_approval_is_accepted_only_from_a_current_admin_for_that_bots_request() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let add = file(&w.c, PairingTarget::ExistingBot { bot_user_id: bot.clone() }).await;
    assert!(matches!(approve(&w, "bob", "bob-d", &bot, &add.request_id).await, WriteOutcome::Forbidden));
    assert!(matches!(approve(&w, "carol", "carol-d", &bot, &add.request_id).await, WriteOutcome::Forbidden));

    // A request belonging to another bot, or a new-bot request, cannot be approved through this bot.
    let (other_bot, _) = make_bot(&w).await;
    let other = file(&w.c, PairingTarget::ExistingBot { bot_user_id: other_bot }).await;
    assert!(matches!(approve(&w, "alice", "alice-d", &bot, &other.request_id).await, WriteOutcome::Forbidden));
    let fresh = file(&w.c, new_bot_target()).await;
    assert!(matches!(approve(&w, "alice", "alice-d", &bot, &fresh.request_id).await, WriteOutcome::Forbidden));

    assert!(matches!(approve(&w, "alice", "alice-d", &bot, &add.request_id).await, WriteOutcome::Ok));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM security_event WHERE user_id = '{bot}' AND kind = 'bot_host_approved'")).await, 1);
}

#[tokio::test]
async fn the_bots_hosts_read_its_approvals_and_the_first_wrap_wins() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let add = file(&w.c, PairingTarget::ExistingBot { bot_user_id: bot.clone() }).await;
    assert!(bots::apply_pending_bot_hosts(&w.c, &bot, NOW).await.unwrap().unwrap().is_empty(), "nothing before an approval");
    assert!(bots::apply_pending_bot_hosts(&w.c, "alice", NOW).await.unwrap().is_none(), "only a bot reads these");
    assert_eq!(bots::apply_wrap_bot_host(&w.c, &bot, &host, &wrap(&add.request_id), NOW).await.unwrap(), WrapOutcome::Forbidden, "no wrap without an approval");

    approve(&w, "alice", "alice-d", &bot, &add.request_id).await;
    let pending = bots::apply_pending_bot_hosts(&w.c, &bot, NOW).await.unwrap().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].ephemeral_pub, add.ephemeral_pub);
    assert_eq!(pending[0].approver_user_id, "alice");
    assert_eq!(pending[0].approver_device_id, "alice-d");

    // Another bot cannot wrap for this one.
    let (other_bot, other_host) = make_bot(&w).await;
    assert_eq!(bots::apply_wrap_bot_host(&w.c, &other_bot, &other_host, &wrap(&add.request_id), NOW).await.unwrap(), WrapOutcome::Forbidden);

    assert_eq!(bots::apply_wrap_bot_host(&w.c, &bot, &host, &wrap(&add.request_id), NOW).await.unwrap(), WrapOutcome::Wrapped);
    assert_eq!(bots::apply_wrap_bot_host(&w.c, &bot, &host, &wrap(&add.request_id), NOW).await.unwrap(), WrapOutcome::Gone, "exactly one host wraps");
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM user_device WHERE user_id = '{bot}' AND device_id = '{}'", add.device_id)).await, 1);
    assert!(bots::apply_pending_bot_hosts(&w.c, &bot, NOW).await.unwrap().unwrap().is_empty());

    let status = bots::apply_pairing_status(
        &w.c,
        &BotPairingStatusBody { request_id: add.request_id.clone(), poll_secret: add.poll_secret.clone() },
        NOW,
    )
    .await
    .unwrap();
    assert_eq!(
        status,
        Some(BotPairingStatus::Wrapped {
            bot_user_id: bot,
            group_id: "g1".into(),
            wrapped_account_key: b64(&[5u8; 92]),
            wrapped_by_device_id: host,
        })
    );
}

#[tokio::test]
async fn a_wrap_is_refused_once_the_approver_is_no_longer_an_admin() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let add = file(&w.c, PairingTarget::ExistingBot { bot_user_id: bot.clone() }).await;
    approve(&w, "alice", "alice-d", &bot, &add.request_id).await;
    exec(&w.c, "UPDATE group_member SET role = 'admin' WHERE user_id = 'bob'").await;
    exec(&w.c, "UPDATE group_member SET role = 'member' WHERE user_id = 'alice'").await;
    assert_eq!(bots::apply_wrap_bot_host(&w.c, &bot, &host, &wrap(&add.request_id), NOW).await.unwrap(), WrapOutcome::Forbidden);
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM user_device WHERE device_id = '{}'", add.device_id)).await, 0);
}

#[tokio::test]
async fn an_admin_of_the_bots_group_revokes_its_host() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let body = || pollis_api::account::RevokeDeviceBody { device_id: host.clone(), user_id: Some(bot.clone()) };
    for outsider in ["bob", "carol"] {
        let outcome = pollis_delivery::account::apply_revoke_device(&w.c, Some(outsider), &body()).await.unwrap();
        assert!(matches!(outcome, WriteOutcome::Forbidden), "{outsider}");
    }
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM user_device WHERE revoked_at IS NOT NULL").await, 0);
    let outcome = pollis_delivery::account::apply_revoke_device(&w.c, Some("alice"), &body()).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Ok));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM user_device WHERE device_id = '{host}' AND revoked_at IS NOT NULL")).await, 1);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM security_event WHERE kind = 'bot_host_revoked'").await, 2, "on the admin and on the bot");
}

// ================================================================ trigger claims

#[tokio::test]
async fn the_first_host_to_claim_a_trigger_wins() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    assert!(bots::apply_claim_trigger(&w.c, &bot, &host, "env-1", NOW).await.unwrap().unwrap());
    assert!(!bots::apply_claim_trigger(&w.c, &bot, "host-2", "env-1", NOW).await.unwrap().unwrap());
    assert!(bots::apply_claim_trigger(&w.c, &bot, &host, "env-1", NOW).await.unwrap().unwrap(), "a retry by the winner still wins");
    assert!(bots::apply_claim_trigger(&w.c, &bot, "host-2", "env-2", NOW).await.unwrap().unwrap());
    assert!(bots::apply_claim_trigger(&w.c, "alice", "alice-d", "env-3", NOW).await.unwrap().is_err(), "only a bot claims");
    assert!(bots::apply_claim_trigger(&w.c, &bot, &host, "", NOW).await.unwrap().is_err());
}

#[tokio::test]
async fn a_trigger_claim_is_device_signed_for_the_signers_own_installation() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let host = bot_host(&w, &bot, &host).await;
    let (s, v) = call(&w, &host, "/v1/bots/triggers/claim", serde_json::json!({ "installation_id": bot, "trigger_key": "env-1" })).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["won"], true);
    let (s, _) = call(&w, &w.alice, "/v1/bots/triggers/claim", serde_json::json!({ "installation_id": bot, "trigger_key": "env-2" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "nobody claims for another installation");
    let (s, _) = call_open(&w, "/v1/bots/triggers/claim", serde_json::json!({ "installation_id": bot, "trigger_key": "env-3" })).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_retention_sweep_drops_lapsed_requests_and_old_claims() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    file(&w.c, new_bot_target()).await;
    bots::apply_claim_trigger(&w.c, &bot, &host, "old", NOW).await.unwrap().unwrap();
    bots::apply_claim_trigger(&w.c, &bot, &host, "new", NOW + bots::TRIGGER_CLAIM_RETENTION_SECS).await.unwrap().unwrap();
    let (requests, claims) =
        bots::sweep_expired(&w.c, NOW + bots::TRIGGER_CLAIM_RETENTION_SECS).await.unwrap();
    assert_eq!(requests, 2, "the open request and the spent create request");
    assert_eq!(claims, 1);
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM bot_trigger_claim WHERE trigger_key = 'new'").await, 1);
}

// ================================================================ refusals

#[tokio::test]
async fn refusal_no_otp_for_a_bot_address() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let cfg = pollis_delivery::otp::OtpConfig { dev_otp: Some("000000".into()), ..Default::default() };
    let otp = pollis_delivery::otp::OtpStore::default();
    let sessions = pollis_delivery::session::SessionStore::default();
    for email in [bots::bot_email(&bot), "someone@bot.invalid".to_string()] {
        pollis_delivery::otp::process_request_otp(&w.c, &otp, &cfg, &email).await.unwrap();
        let verified = pollis_delivery::otp::apply_verify_otp(&w.c, &otp, &sessions, &cfg, &email, "000000", "dev-x").await.unwrap();
        assert!(matches!(verified, pollis_delivery::otp::VerifyOtpResult::InvalidCode), "{email}");
    }
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM users").await, 4, "no account was created");
}

#[tokio::test]
async fn refusal_no_email_change_for_a_bot_or_to_a_bot_address() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let host = bot_host(&w, &bot, &host).await;
    let (s, _) = call(&w, &host, "/v1/auth/request-email-change-otp", serde_json::json!({ "new_email": "x@y.com" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let store = pollis_delivery::email_change::EmailChangeStore::default();
    let cfg = pollis_delivery::otp::OtpConfig { dev_otp: Some("000000".into()), ..Default::default() };
    for (who, to) in [(bot.as_str(), "x@y.com"), ("alice", "alice@bot.invalid")] {
        let outcome = pollis_delivery::email_change::apply_verify_email_change(&w.c, &store, &cfg, who, to, "000000", Some("000000")).await.unwrap();
        assert_eq!(outcome, pollis_delivery::email_change::EmailChangeOutcome::Mismatch, "{who} -> {to}");
    }
}

#[tokio::test]
async fn refusal_no_directory_hit_by_username_or_email() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let username: String = {
        let mut r = w.c.query("SELECT username FROM users WHERE id = ?1", libsql::params![bot.clone()]).await.unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };
    for identifier in [username, bots::bot_email(&bot)] {
        assert!(pollis_delivery::directory::user_by_identifier(&w.c, &identifier).await.unwrap().is_none(), "{identifier}");
    }
    assert!(pollis_delivery::directory::user_by_identifier(&w.c, "bob").await.unwrap().is_some(), "people are still found");
}

#[tokio::test]
async fn refusal_profile_reads_never_return_a_bots_email() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let (s, v) = call(&w, &w.bob, "/v1/directory/users", serde_json::json!({ "user_ids": [bot], "identifier": bots::bot_email(&bot) })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["users"].as_array().unwrap().len(), 1, "hydrated by id");
    assert_eq!(v["users"][0]["is_bot"], true, "the client can badge it");
    assert!(!v.to_string().contains("bot.invalid"), "{v}");
    let (s, v) = call(&w, &w.bob, "/v1/directory/members", serde_json::json!({ "group_ids": ["g1"] })).await;
    assert_eq!(s, StatusCode::OK);
    let members = v["groups"][0]["members"].as_array().unwrap();
    let flags: Vec<(String, bool)> = members
        .iter()
        .map(|m| (m["user_id"].as_str().unwrap().to_string(), m["is_bot"].as_bool().unwrap()))
        .collect();
    assert!(flags.contains(&(bot.clone(), true)), "{flags:?}");
    assert!(flags.contains(&("bob".to_string(), false)), "{flags:?}");
    assert!(!v.to_string().contains("bot.invalid"), "{v}");
}

#[tokio::test]
async fn refusal_no_dm_with_a_bot_in_either_direction() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let dm = |creator: &str, other: &str| pollis_api::profile::CreateDmBody {
        id: ulid::Ulid::new().to_string(),
        creator_id: creator.into(),
        member_ids: vec![creator.into(), other.into()],
        created_at: "2026-11-18T00:00:00Z".into(),
    };
    for (creator, other) in [("alice", bot.as_str()), (bot.as_str(), "alice")] {
        let outcome = pollis_delivery::profile::apply_create_dm(&w.c, Some(creator), &dm(creator, other)).await.unwrap();
        assert!(matches!(outcome, pollis_delivery::profile::DmOutcome::Forbidden), "{creator} -> {other}");
    }
    // Added to an existing DM.
    let ok = pollis_delivery::profile::apply_create_dm(&w.c, Some("alice"), &dm("alice", "bob")).await.unwrap();
    let pollis_delivery::profile::DmOutcome::Created { id, .. } = ok else { panic!("{ok:?}") };
    let add = pollis_api::profile::AddDmMemberBody { dm_channel_id: id, user_id: bot.clone(), added_by: "alice".into(), added_at: "now".into() };
    assert!(matches!(pollis_delivery::profile::apply_add_dm_member(&w.c, Some("alice"), &add).await.unwrap(), WriteOutcome::Forbidden));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM dm_channel_member WHERE user_id = '{bot}'")).await, 0);
}

#[tokio::test]
async fn refusal_a_bot_creates_no_group() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let body = pollis_api::groups::CreateGroupBody {
        id: ulid::Ulid::new().to_string(),
        name: "Mine".into(),
        description: None,
        owner_id: None,
        default_text_channel_id: None,
        default_voice_channel_id: None,
        created_at: "2026-11-18T00:00:00Z".into(),
    };
    assert!(matches!(pollis_delivery::groups::apply_create_group(&w.c, Some(&bot), &body).await.unwrap(), WriteOutcome::Forbidden));
}

#[tokio::test]
async fn refusal_no_invite_redemption_join_request_or_invite_for_a_bot() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let redeem = pollis_api::groups::RedeemInviteLinkBody {
        token: "t".into(),
        user_id: None,
        attempt_id: ulid::Ulid::new().to_string(),
        now: "now".into(),
    };
    assert!(matches!(
        pollis_delivery::groups::apply_redeem_invite_link(&w.c, Some(&bot), &redeem).await.unwrap(),
        pollis_delivery::groups::RedeemOutcome::Rejected
    ));
    let join = pollis_api::groups::CreateJoinRequestBody { id: "j1".into(), group_id: "g2".into(), requester_id: None };
    assert!(matches!(
        pollis_delivery::groups::apply_create_join_request(&w.c, Some(&bot), &join).await.unwrap(),
        pollis_delivery::groups::JoinRequestOutcome::Forbidden
    ));
    let username: String = {
        let mut r = w.c.query("SELECT username FROM users WHERE id = ?1", libsql::params![bot.clone()]).await.unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };
    let invite = pollis_api::groups::CreateInviteBody { id: "i1".into(), group_id: "g2".into(), inviter_id: None, invitee_identifier: username };
    assert!(matches!(
        pollis_delivery::groups::apply_create_invite(&w.c, Some("carol"), &invite).await.unwrap(),
        pollis_delivery::groups::InviteOutcome::NoSuchUser
    ));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM group_member WHERE user_id = '{bot}'")).await, 1, "still only in its own group");
}

#[tokio::test]
async fn refusal_a_bot_is_never_made_admin_removed_or_left() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let role = pollis_api::groups::SetMemberRoleBody { group_id: "g1".into(), user_id: bot.clone(), role: "admin".into(), requester_id: None };
    assert!(matches!(pollis_delivery::groups::apply_set_member_role(&w.c, Some("alice"), &role).await.unwrap(), WriteOutcome::Forbidden));
    let remove = pollis_api::groups::RemoveMemberBody { group_id: "g1".into(), user_id: bot.clone(), requester_id: None };
    assert!(matches!(pollis_delivery::groups::apply_remove_member(&w.c, Some("alice"), &remove).await.unwrap(), WriteOutcome::Forbidden));
    let leave = pollis_api::groups::LeaveGroupBody { group_id: "g1".into(), user_id: None };
    let (outcome, _) = pollis_delivery::groups::apply_leave_group(&w.c, Some(&bot), &leave).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Forbidden));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM group_member WHERE user_id = '{bot}' AND role = 'member'")).await, 1);
}

#[tokio::test]
async fn refusal_no_push_token_for_a_bot() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let body = pollis_api::devices::PushTokenBody {
        token: "ExponentPushToken[abc]".into(),
        platform: "ios".into(),
        updated_at: "now".into(),
        user_id: None,
        caps: Vec::new(),
    };
    let outcome = pollis_delivery::devices::apply_register_push_token(&w.c, Some(&bot), Some(&host), &body).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Forbidden));
    assert_eq!(count(&w.c, "SELECT COUNT(*) FROM push_token").await, 0);
}

#[tokio::test]
async fn refusal_a_bots_key_packages_are_claimed_only_by_admins_of_its_group() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let body = pollis_api::devices::ClaimKeyPackageBody { target_user_id: bot.clone(), target_device_id: None, ciphersuite: None };
    for outsider in ["bob", "carol"] {
        let outcome = pollis_delivery::devices::apply_claim_key_package(&w.c, Some(outsider), &body).await.unwrap();
        assert!(matches!(outcome, pollis_delivery::devices::ClaimOutcome::Forbidden), "{outsider}");
    }
    // An admin reaches the pool (empty here, so no package rather than a refusal).
    let outcome = pollis_delivery::devices::apply_claim_key_package(&w.c, Some("alice"), &body).await.unwrap();
    assert!(matches!(outcome, pollis_delivery::devices::ClaimOutcome::NoKeyPackage));
}

#[tokio::test]
async fn refusal_a_bot_neither_shows_nor_accepts_a_device_link() {
    let w = world().await;
    let (bot, host) = make_bot(&w).await;
    let host = bot_host(&w, &bot, &host).await;
    let (s, _) = call(&w, &host, "/v1/link/create", serde_json::json!({ "link_id": "l1", "claim_verifier": b64(&[0u8; 32]) })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&w, &host, "/v1/link/accept-offer", serde_json::json!({ "link_id": "l2" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn refusal_a_bot_neither_resets_nor_rotates_its_identity() {
    let w = world().await;
    let (bot, _) = make_bot(&w).await;
    let reset = pollis_api::account::ResetRecoverBody { current_device_id: None, user_id: None };
    let (outcome, _) = pollis_delivery::account::apply_reset_recover(&w.c, Some(&bot), &reset).await.unwrap();
    assert!(matches!(outcome, WriteOutcome::Forbidden));
    let rotate = pollis_api::account::RotateIdentityBody {
        based_on_version: 1,
        account_id_pub: b64(&[1u8; 1312]),
        salt: b64(b"s"),
        nonce: b64(b"n"),
        wrapped_key: b64(b"w"),
        user_id: None,
    };
    let (outcome, _) = pollis_delivery::account::apply_rotate_identity(
        &w.c,
        Some(&bot),
        &rotate,
        &pollis_delivery::writes::GateCredential::None,
    )
    .await
    .unwrap();
    assert!(matches!(outcome, pollis_delivery::account::RotateOutcome::Forbidden));
    assert_eq!(count(&w.c, &format!("SELECT COUNT(*) FROM group_member WHERE user_id = '{bot}'")).await, 1);
}

#[tokio::test]
async fn the_bot_endpoints_require_a_device_signature() {
    let w = world().await;
    let p = file(&w.c, new_bot_target()).await;
    let body = serde_json::to_value(create_body(&p, "g1")).unwrap();
    let (s, _) = call_open(&w, "/v1/bots/create", body.clone()).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, v) = call(&w, &w.alice, "/v1/bots/create", body).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "ok");
    let bot = v["bot_user_id"].as_str().unwrap().to_string();

    let (s, v) = call(&w, &w.alice, "/v1/read/bots", serde_json::json!({ "group_id": "g1" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["limit"], MAX_BOTS_PER_GROUP);
    let (s, _) = call(&w, &w.carol, "/v1/bots/delete", serde_json::json!({ "bot_user_id": bot })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&w, &w.alice, "/v1/bots/delete", serde_json::json!({ "bot_user_id": bot })).await;
    assert_eq!(s, StatusCode::OK);
}
