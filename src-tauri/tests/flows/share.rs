//! Sharing into Pollis from another app (#2100), end to end through the real send pipeline and
//! the in-process DS.
//!
//! The "extension" is `pollis_core::share::session` on the app's own keystore, data dir and
//! config, exactly what `pollis_share_open` builds in the iOS share extension: a second
//! `AppState` with its own database connection and its own descriptor on `state.lock`.

use std::time::Duration;

use crate::harness::{wipe, world, writable_remote, TestClient, TEST_PIN};
use pollis_core::commands::messages::test_hooks;
use pollis_core::share::send::{deliver_share, enqueue_share, target_message_id, ShareRequest, ShareStatus};
use pollis_core::share::session::{self, Factor, Params, Status, Unlock};
use pollis_core::share::targets::{share_targets, TargetKind};
use serde_json::json;
use serial_test::serial;

fn wrong_pin() -> &'static str {
    if TEST_PIN == "1111" {
        "2222"
    } else {
        "1111"
    }
}

/// R2 is not in the harness: uploads resolve the ring key and seal as in production, then
/// skip the PUT.
struct StubUploads;

impl StubUploads {
    fn on() -> Self {
        test_hooks::stub_attachment_uploads(true);
        Self
    }
}

impl Drop for StubUploads {
    fn drop(&mut self) {
        test_hooks::stub_attachment_uploads(false);
    }
}

fn request(targets: &[&str], text: &str) -> ShareRequest {
    ShareRequest {
        share_id: ulid::Ulid::new().to_string(),
        targets: targets.iter().map(|t| t.to_string()).collect(),
        text: text.into(),
        attachments: Vec::new(),
        link_preview: None,
    }
}

async fn open_extension(client: &TestClient) -> Status {
    client.activate();
    session::open(Params {
        keystore: client.state.keystore.clone(),
        config: world().await.config.clone(),
        data_dir: client.state.local_data_dir(),
    })
    .await
}

async fn copies(client: &TestClient, conversation: &str, is_dm: bool, text: &str) -> usize {
    let rows = if is_dm {
        client.fetch_dm_messages(conversation).await
    } else {
        client.fetch_channel_messages(conversation).await
    };
    rows.iter().filter(|m| m["content"].as_str() == Some(text)).count()
}

async fn envelopes_with_id(id: &str) -> i64 {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM message_envelope WHERE id = ?1", libsql::params![id.to_string()])
        .await
        .expect("count envelopes");
    rows.next().await.expect("row").expect("a row").get(0).expect("count")
}

/// Alice and bob share a DM and a group whose general channel both are in.
async fn alice_and_bob() -> (TestClient, TestClient, String, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_p = alice.sign_up("share-alice@test.local").await;
    let bob_p = bob.sign_up("share-bob@test.local").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    alice.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;

    let group = alice.create_group("Share").await;
    let channel = alice.general_channel_id(&group).await;
    alice.invite(&group, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("bob has an invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.process_commits_for(&channel).await;
    (alice, bob, dm, channel)
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn targets_leave_out_requests_blocked_dms_and_voice_channels() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut mallory = TestClient::new().await;
    let a = alice.sign_up("targets-alice@test.local").await;
    let b = bob.sign_up("targets-bob@test.local").await;
    let c = carol.sign_up("targets-carol@test.local").await;
    let m = mallory.sign_up("targets-mallory@test.local").await;

    let with_bob = alice.create_dm(&[a.id.as_str(), b.id.as_str()]).await;
    bob.accept_dm_request(&with_bob).await;
    // Carol's DM is still a request on alice's side.
    let from_carol = carol.create_dm(&[c.id.as_str(), a.id.as_str()]).await;
    let with_mallory = alice.create_dm(&[a.id.as_str(), m.id.as_str()]).await;
    alice.block(&m.id).await;

    let group = alice.create_group("Targets").await;
    let general = alice.general_channel_id(&group).await;
    let voice: serde_json::Value = alice
        .invoke_json(
            "create_channel",
            json!({ "groupId": group, "name": "lounge", "description": null,
                    "channelType": "voice", "creatorId": alice.user_id() }),
        )
        .await;
    let voice = voice["id"].as_str().expect("voice channel id").to_string();
    // Activity puts the channel first.
    alice.send_channel_message(&general, "hello").await;

    alice.activate();
    let all = share_targets(&alice.state, None, None).await.expect("share_targets");
    let ids: Vec<&str> = all.iter().map(|t| t.conversation_id.as_str()).collect();
    assert_eq!(ids[0], pollis_core::share::targets::VAULT_TARGET, "the Vault first: {ids:?}");
    assert_eq!(ids.get(1).copied(), Some(general.as_str()), "then the most recent: {ids:?}");
    assert!(ids.contains(&with_bob.as_str()), "{ids:?}");
    for (id, why) in [(&from_carol, "a request"), (&with_mallory, "a blocked DM"), (&voice, "a voice channel")] {
        assert!(!ids.contains(&id.as_str()), "{why} is no target: {ids:?}");
    }
    let bob_row = all.iter().find(|t| t.conversation_id == with_bob).unwrap();
    assert_eq!(bob_row.kind, TargetKind::Dm);
    assert_eq!(bob_row.avatar_user_id.as_deref(), Some(b.id.as_str()));
    let found = share_targets(&alice.state, Some(b.username.clone()), None).await.unwrap();
    assert_eq!(found.iter().map(|t| t.conversation_id.as_str()).collect::<Vec<_>>(), [with_bob.as_str()]);

    // The send path refuses what the picker never offers, storing nothing.
    let refused = enqueue_share(&alice.state, &request(&[&with_bob, &from_carol], "nope")).await;
    assert!(refused.is_err(), "a request is refused");
    assert_eq!(copies(&alice, &with_bob, true, "nope").await, 0, "nothing stored for the valid target either");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn one_share_reaches_every_target_once_with_its_own_file_keys() {
    let _stub = StubUploads::on();
    let (alice, bob, dm, channel) = alice_and_bob().await;

    // Bigger than two upload chunks, so the streamed store and upload both cross boundaries.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.jpg");
    let bytes: Vec<u8> = (0..(9 * 1024 * 1024 + 77)).map(|i| (i % 253) as u8).collect();
    std::fs::write(&path, &bytes).unwrap();
    let sha = {
        use sha2::Digest;
        hex::encode(sha2::Sha256::digest(&bytes))
    };

    let mut req = request(&[&dm, &channel], "look at this");
    req.attachments = vec![serde_json::from_value(json!({
        "path": format!("file://{}", path.display()),
        "filename": "photo.jpg",
        "contentType": "application/octet-stream",
    }))
    .unwrap()];
    alice.activate();
    let queued = enqueue_share(&alice.state, &req).await.expect("enqueue");
    assert_eq!(queued.len(), 2);
    let results = deliver_share(&alice.state, &queued, |_| {}).await;
    assert!(results.iter().all(|r| r.status == ShareStatus::Sent), "{results:?}");

    // Each target's row: its own message id, its own object and file key, the same bytes.
    let mut objects = Vec::new();
    for q in &queued {
        assert_eq!(q.message_id, target_message_id(&req.share_id, &q.conversation_id).unwrap());
        let stored: String = {
            let guard = alice.state.local_db.lock().await;
            guard
                .as_ref()
                .expect("db")
                .conn()
                .query_row(
                    "SELECT content FROM message WHERE id = ?1 AND conversation_id = ?2 AND send_state IS NULL",
                    [&q.message_id, &q.conversation_id],
                    |r| r.get(0),
                )
                .unwrap_or_else(|e| panic!("alice holds {} delivered: {e}", q.message_id))
        };
        let content: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(content["_txt"], "look at this");
        let att = &content["_att"][0];
        assert_eq!(att["sha"], sha.as_str(), "the plaintext digest survives the chunked path");
        assert_eq!(att["size"], bytes.len());
        objects.push(att["hash"].as_str().unwrap().to_string());
        assert_eq!(envelopes_with_id(&q.message_id).await, 1);
    }
    assert_ne!(objects[0], objects[1], "no object is reused across conversations");

    // Bob gets one message in each conversation.
    for (conv, is_dm) in [(&dm, true), (&channel, false)] {
        let rows = if is_dm { bob.fetch_dm_messages(conv).await } else { bob.fetch_channel_messages(conv).await };
        let n = rows
            .iter()
            .filter(|m| queued.iter().any(|q| m["id"] == q.message_id.as_str()))
            .count();
        assert_eq!(n, 1, "bob holds the share once in {conv}: {rows:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn running_a_share_again_never_sends_it_twice() {
    let (alice, bob, dm, channel) = alice_and_bob().await;
    let req = request(&[&dm, &channel], "only once");
    alice.activate();
    for _ in 0..2 {
        let queued = enqueue_share(&alice.state, &req).await.expect("enqueue");
        let results = deliver_share(&alice.state, &queued, |_| {}).await;
        assert!(results.iter().all(|r| r.status == ShareStatus::Sent), "{results:?}");
    }
    assert_eq!(copies(&bob, &dm, true, "only once").await, 1);
    assert_eq!(copies(&bob, &channel, false, "only once").await, 1);
    assert_eq!(copies(&alice, &dm, true, "only once").await, 1, "one row on the sender too");
    for conv in [&dm, &channel] {
        assert_eq!(envelopes_with_id(&target_message_id(&req.share_id, conv).unwrap()).await, 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_extension_sends_and_the_app_finishes_what_it_left() {
    let (alice, bob, dm, channel) = alice_and_bob().await;

    assert!(matches!(open_extension(&alice).await, Status::Locked { attempts_remaining: 10, .. }));
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::Unlocked);
    let rows = session::targets(None, None).await.expect("the extension lists targets");
    assert!(rows.iter().any(|t| t.conversation_id == dm));

    // A whole share from the extension, the app running alongside.
    let sent = session::send(request(&[&dm], "from the sheet"), |_| {}, |_| {}).await.expect("send");
    assert_eq!(sent[0].status, ShareStatus::Sent);

    // The extension is dismissed the moment every row is stored: nothing is delivered by it.
    let req = request(&[&dm, &channel], "finished by the app");
    let left = session::send(req.clone(), |_| session::close(), |_| {}).await.expect("stored");
    assert!(left.iter().all(|r| r.status == ShareStatus::Queued), "{left:?}");
    assert_eq!(copies(&bob, &dm, true, "finished by the app").await, 0);

    // The app's next drain delivers each, once.
    alice.flush_outbox().await;
    alice.flush_outbox().await;
    assert_eq!(copies(&bob, &dm, true, "from the sheet").await, 1);
    assert_eq!(copies(&bob, &dm, true, "finished by the app").await, 1);
    assert_eq!(copies(&bob, &channel, false, "finished by the app").await, 1);
    for conv in [&dm, &channel] {
        assert_eq!(envelopes_with_id(&target_message_id(&req.share_id, conv).unwrap()).await, 1);
    }

    // Both directions still decrypt: neither process rolled the other's MLS state back.
    bob.send_channel_message(&dm, "still here").await;
    assert_eq!(copies(&alice, &dm, true, "still here").await, 1);
    alice.send_channel_message(&dm, "and back").await;
    assert_eq!(copies(&bob, &dm, true, "and back").await, 1);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_extension_suspends_without_holding_the_database() {
    let (alice, _bob, dm, _channel) = alice_and_bob().await;
    open_extension(&alice).await;
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::Unlocked);
    assert!(session::suspend(Duration::from_secs(4)), "nothing held while suspended");
    assert!(session::targets(None, None).await.is_err(), "the database is closed");
    session::resume();
    let rows = session::targets(None, None).await.expect("reopened on resume");
    assert!(rows.iter().any(|t| t.conversation_id == dm));
    session::close();
    assert!(session::targets(None, None).await.is_err(), "closed for good");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_extension_and_the_app_count_against_one_pin_lockout() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("share-lockout@test.local").await;

    assert!(matches!(open_extension(&alice).await, Status::Locked { attempts_remaining: 10, .. }));
    for left in [9, 8, 7] {
        assert_eq!(
            session::unlock(Factor::Pin(wrong_pin().into())).await,
            Unlock::Incorrect { attempts_remaining: left }
        );
    }
    // The app sees the extension's misses.
    let uid = alice.user_id().to_string();
    alice.invoke_json("lock", json!({})).await;
    let err = alice
        .invoke_try("unlock", json!({ "userId": uid, "pin": wrong_pin() }))
        .await
        .expect_err("a wrong PIN");
    assert!(err.contains("6 attempts remaining"), "{err}");
    // And the extension sees the app's.
    assert!(matches!(open_extension(&alice).await, Status::Locked { attempts_remaining: 6, .. }));
    // A right PIN anywhere forgives them all.
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::Unlocked);
    session::close();
    assert!(matches!(open_extension(&alice).await, Status::Locked { attempts_remaining: 10, .. }));

    // Ten misses in the extension lock the account out on this device, app included.
    for _ in 0..9 {
        assert!(matches!(session::unlock(Factor::Pin(wrong_pin().into())).await, Unlock::Incorrect { .. }));
    }
    assert_eq!(session::unlock(Factor::Pin(wrong_pin().into())).await, Unlock::LockedOut);
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::LockedOut, "even the right PIN");
    session::close();
    let snap = alice.invoke_json("get_unlock_state", json!({})).await;
    assert_eq!(snap["pin_set"], false, "the app's keys went with the lockout: {snap}");
    assert_eq!(open_extension(&alice).await, Status::SignedOut, "nothing left to unlock");
    pollis_core::share::session::close();
    // This device's history is gone by design; recovery is the Secret Key.
    crate::harness::expect_losses();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn biometric_unlock_in_the_extension_never_touches_the_pin_counter() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("share-biometric@test.local").await;
    let key = alice.enable_biometric_unlock().await;

    assert!(matches!(
        open_extension(&alice).await,
        Status::Locked { biometric: true, attempts_remaining: 10, .. }
    ));
    let wrong = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode([0x5au8; 32])
    };
    assert_eq!(session::unlock(Factor::DeviceKey(wrong)).await, Unlock::BiometricFailed);
    assert_eq!(session::unlock(Factor::Pin(wrong_pin().into())).await, Unlock::Incorrect { attempts_remaining: 9 });
    assert_eq!(session::unlock(Factor::DeviceKey(key)).await, Unlock::Unlocked);
    session::close();
    assert!(
        matches!(open_extension(&alice).await, Status::Locked { attempts_remaining: 9, .. }),
        "a biometric unlock forgives nothing"
    );
    session::close();
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn signed_out_the_extension_offers_nothing() {
    wipe().await;
    let _ = std::fs::remove_file(pollis_core::db::local::dirs_path().join("accounts.json"));
    let nobody = TestClient::new().await;
    assert_eq!(open_extension(&nobody).await, Status::SignedOut);
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::NoSession);
    assert!(session::targets(None, None).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_vault_is_a_target_like_any_chat() {
    let (alice, bob, dm, _channel) = alice_and_bob().await;
    alice.activate();
    let rows = share_targets(&alice.state, None, None).await.expect("targets");
    assert_eq!(rows[0].kind, TargetKind::Vault, "the Vault comes first");
    assert_eq!(rows[0].conversation_id, pollis_core::share::targets::VAULT_TARGET);

    let req = request(&[pollis_core::share::targets::VAULT_TARGET, &dm], "keep this");
    for _ in 0..2 {
        let queued = enqueue_share(&alice.state, &req).await.expect("enqueue");
        let results = deliver_share(&alice.state, &queued, |_| {}).await;
        assert!(results.iter().all(|r| r.status == ShareStatus::Sent), "{results:?}");
    }
    let vault = alice
        .invoke_json("get_vault_messages", json!({ "userId": alice.user_id() }))
        .await;
    let saved: Vec<_> = vault.as_array().unwrap().iter().filter(|e| e["content"] == "keep this").collect();
    assert_eq!(saved.len(), 1, "saved once: {vault}");
    assert_eq!(
        saved[0]["id"].as_str().unwrap(),
        target_message_id(&req.share_id, pollis_core::share::targets::VAULT_TARGET).unwrap()
    );
    assert_eq!(copies(&bob, &dm, true, "keep this").await, 1);

    // Six targets with the Vault among them is still over the cap.
    let six = request(&[pollis_core::share::targets::VAULT_TARGET, "a", "b", "c", "d", "e"], "x");
    assert!(enqueue_share(&alice.state, &six).await.is_err());

    // From the extension too, interrupted and finished by the app.
    open_extension(&alice).await;
    assert_eq!(session::unlock(Factor::Pin(TEST_PIN.into())).await, Unlock::Unlocked);
    let left = request(&[pollis_core::share::targets::VAULT_TARGET], "saved later");
    let out = session::send(left.clone(), |_| session::close(), |_| {}).await.expect("stored");
    assert_eq!(out[0].status, ShareStatus::Queued);
    alice.flush_outbox().await;
    let vault = alice
        .invoke_json("get_vault_messages", json!({ "userId": alice.user_id() }))
        .await;
    let later: Vec<_> = vault.as_array().unwrap().iter().filter(|e| e["content"] == "saved later").collect();
    assert_eq!(later.len(), 1, "{vault}");
    assert!(later[0].get("send_state").is_none());
}
