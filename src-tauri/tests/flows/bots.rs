//! Bots (#1182), end to end through the in-process DS: pairing a bot onto a
//! host, a second host through a verified host approval, revoking a host,
//! deleting the bot (and its group), every add-host refusal, and the
//! unattended path's refusal of a human account.

use std::time::Duration;

use base64::Engine as _;
use serde_json::json;
use serial_test::serial;

use pollis_core::commands::bots::{
    self, ApprovalRefusal, HostApproval, HostApprovalOutcome, HOST_APPROVAL_VERSION, UNATTENDED_REFUSED,
};
use pollis_lib::commands::transparency::AuditStatus;

use crate::harness::{
    pair_new_bot, pairing_row, plant_host_approval, swap_pairing_ephemeral, wipe, writable_remote, BotHostClient,
    TestClient,
};

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

async fn device_of(client: &TestClient) -> String {
    client.state.device_id.lock().await.clone().expect("device id")
}

async fn join(admin: &TestClient, member: &TestClient, group_id: &str, username: &str) {
    admin.invite(group_id, username).await;
    let invite = member.first_pending_invite().await.expect("pending invite");
    member.accept_invite(invite["id"].as_str().expect("invite id")).await;
    member.poll().await;
    admin.poll().await;
}

async fn members(client: &TestClient, group_id: &str) -> Vec<serde_json::Value> {
    client
        .invoke_json("get_group_members", json!({ "groupId": group_id, "requesterId": client.user_id() }))
        .await
        .as_array()
        .expect("members")
        .clone()
}

async fn tree(client: &TestClient, group_id: &str) -> Vec<(String, String)> {
    pollis_lib::commands::mls::local_tree_members(&client.state, group_id).await
}

/// File an add-host request on a fresh host and return it with its handle.
async fn file_add_host(bot_user_id: &str, name: &str) -> (BotHostClient, bots::PairingHandle) {
    let host = BotHostClient::new().await;
    let handle = bots::start_add_host_pairing(&host.state, bot_user_id, Some(name.to_string()))
        .await
        .expect("start_add_host_pairing");
    (host, handle)
}

async fn users_row_count(user_id: &str) -> i64 {
    let conn = writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query("SELECT COUNT(*) FROM users WHERE id = ?1", libsql::params![user_id.to_string()])
        .await
        .expect("count users");
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_bot_is_paired_gains_a_verified_second_host_loses_a_revoked_one_and_is_deleted() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let group = alice.create_group("Ops").await;
    join(&alice, &bob, &group, &bob_p.username).await;
    let general = alice.general_channel_id(&group).await;

    // Pair: the host prints a code, the admin types it, the host joins.
    let (host1, created) = pair_new_bot(&alice, &group, "Alerts").await;
    let bot = host1.bot_user_id().to_string();
    assert_eq!(created["userId"], bot);
    assert_eq!(created["displayName"], "Alerts");
    assert_eq!(created["hostCount"], 1);
    assert!(
        tree(&alice, &group).await.contains(&(bot.clone(), host1.device_id().to_string())),
        "create_bot's reconcile admits the host"
    );
    bob.poll().await;
    let as_seen_by_bob = members(&bob, &group).await;
    let bot_row = as_seen_by_bob.iter().find(|m| m["user_id"] == bot).expect("the bot is a member");
    assert_eq!(bot_row["is_bot"], true);
    assert!(as_seen_by_bob.iter().filter(|m| m["user_id"] != bot).all(|m| m["is_bot"] == false));

    alice.send_channel_message(&general, "first alert").await;
    assert!(host1.read_channel(&general).await.unwrap().contains(&"first alert".to_string()));

    // A second host: its code, the admin's signed approval, the first host's verified wrap.
    let (mut host2, handle2) = file_add_host(&bot, "host-2").await;
    let added = alice
        .invoke_try("add_bot_host", json!({ "botUserId": bot, "code": handle2.code }))
        .await
        .expect("add_bot_host");
    assert_eq!(added["deviceId"], handle2.device_id);
    let outcomes = host1.handle_approvals().await;
    assert!(
        matches!(&outcomes[..], [HostApprovalOutcome::Wrapped { request_id, .. }] if *request_id == handle2.request_id),
        "the only host wraps at once: {outcomes:?}"
    );
    // A member's reconcile must skip a bot it may not claim for, not fail.
    bob.sweep().await;
    host2.finish_pairing(&handle2, Duration::from_secs(2)).await;
    assert_eq!(host2.bot_user_id(), bot);
    assert_ne!(host2.device_id(), host1.device_id(), "two hosts are two leaves");

    alice.send_channel_message(&general, "second alert").await;
    assert!(host2.read_channel(&general).await.unwrap().contains(&"second alert".to_string()));
    assert!(host1.read_channel(&general).await.unwrap().contains(&"second alert".to_string()));

    let detail = alice.invoke_json("get_bot", json!({ "botUserId": bot })).await;
    assert_eq!(detail["hosts"].as_array().unwrap().len(), 2);
    assert_eq!(detail["groupBotCap"], 25);
    assert_eq!(detail["groupId"], group);

    // Revoke host 1: its leaf leaves, and it cannot read what is sent next.
    alice
        .invoke_try("revoke_bot_host", json!({ "botUserId": bot, "deviceId": host1.device_id() }))
        .await
        .expect("revoke_bot_host");
    assert!(!tree(&alice, &group).await.contains(&(bot.clone(), host1.device_id().to_string())));
    alice.send_channel_message(&general, "after the revoke").await;
    assert!(host2.read_channel(&general).await.unwrap().contains(&"after the revoke".to_string()));
    let revoked_view = host1.read_channel(&general).await.unwrap_or_default();
    assert!(!revoked_view.contains(&"after the revoke".to_string()), "a revoked host must not decrypt");
    {
        let ciphertexts = crate::harness::envelope_ciphertexts(&general).await;
        let guard = host1.state.local_db.lock().await;
        let db = guard.as_ref().expect("host 1 db");
        for ct in ciphertexts {
            if let Some((plain, _)) = pollis_lib::commands::mls::try_mls_decrypt(db.conn(), &group, &ct) {
                assert!(!String::from_utf8_lossy(&plain).contains("after the revoke"));
            }
        }
    }
    let detail = alice.invoke_json("get_bot", json!({ "botUserId": bot })).await;
    assert_eq!(detail["hosts"].as_array().unwrap().len(), 1);

    // Delete: the account goes, and its leaves leave the tree.
    alice.invoke_try("delete_bot", json!({ "botUserId": bot })).await.expect("delete_bot");
    assert_eq!(users_row_count(&bot).await, 0);
    assert!(tree(&alice, &group).await.iter().all(|(u, _)| *u != bot));
    let listed = alice.invoke_json("list_group_bots", json!({ "groupId": group })).await;
    assert!(listed.as_array().unwrap().is_empty());
    assert!(members(&alice, &group).await.iter().all(|m| m["user_id"] != bot));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_bot_restarts_unattended_and_goes_with_its_group() {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let group = alice.create_group("Ops").await;
    let general = alice.general_channel_id(&group).await;
    let (host, _) = pair_new_bot(&alice, &group, "Alerts").await;
    let bot = host.bot_user_id().to_string();
    alice.send_channel_message(&general, "before the reboot").await;

    // The host process restarts: same keystore and disk, no PIN.
    let restarted = host.restarted().await;
    drop(host);
    let session = bots::start_unattended(&restarted.state, &bot).await.expect("unattended start");
    assert_eq!(session.group_id, group);
    let mut restarted = restarted;
    restarted.session = Some(session);
    alice.send_channel_message(&general, "after the reboot").await;
    let read = restarted.read_channel(&general).await.unwrap();
    assert!(read.contains(&"before the reboot".to_string()) && read.contains(&"after the reboot".to_string()));

    // Deleting the group deletes its bot.
    alice
        .invoke_try("delete_group", json!({ "groupId": group, "requesterId": alice.user_id() }))
        .await
        .expect("delete_group");
    assert_eq!(users_row_count(&bot).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_human_account_cannot_take_the_unattended_path() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;

    // Its own device: it has a PIN.
    let err = bots::start_unattended(&alice.state, &alice_p.id).await.unwrap_err().to_string();
    assert!(err.starts_with(UNATTENDED_REFUSED), "{err}");

    // A machine with no installation for it.
    let elsewhere = BotHostClient::new().await;
    let err = bots::start_unattended(&elsewhere.state, &alice_p.id).await.unwrap_err().to_string();
    assert!(err.starts_with(UNATTENDED_REFUSED), "{err}");

    // An installation slot planted with the person's real account key: refused,
    // whether the DS answers "human" or refuses the transplanted device.
    let seed = alice.state.unlock.lock().await.as_ref().expect("unlocked").account_id_key.to_vec();
    let record = json!({
        "v": 1,
        "bot_user_id": alice_p.id,
        "group_id": "01JGROUP0000000000000000000",
        "device_id": device_of(&alice).await,
        "account_seed": b64(&seed),
        "db_key": b64(&[7u8; 32]),
    });
    let planted = BotHostClient::new().await;
    planted
        .keystore
        .store_for_user("bot_installation", &alice_p.id, record.to_string().as_bytes())
        .await
        .unwrap();
    let err = bots::start_unattended(&planted.state, &alice_p.id).await.unwrap_err().to_string();
    assert!(err.starts_with(UNATTENDED_REFUSED), "{err}");
    assert!(planted.state.unlock.lock().await.is_none(), "a refused start leaves nothing unlocked");
    assert!(planted.state.local_db.lock().await.is_none());
}

/// A group with an admin (alice), a second admin (carol), a member (bob), a
/// bot on one host, and an open add-host request.
struct Fixture {
    alice: TestClient,
    bob: TestClient,
    carol: TestClient,
    group: String,
    host1: BotHostClient,
    host2: BotHostClient,
    handle2: bots::PairingHandle,
}

async fn fixture() -> Fixture {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let group = alice.create_group("Ops").await;
    join(&alice, &bob, &group, &bob_p.username).await;
    join(&alice, &carol, &group, &carol_p.username).await;
    alice.set_member_role(&group, &carol_p.id, "admin").await;
    let (host1, _) = pair_new_bot(&alice, &group, "Alerts").await;
    // The host sees everyone's current leaves.
    host1.read_channel(&alice.general_channel_id(&group).await).await.unwrap();
    let (host2, handle2) = file_add_host(host1.bot_user_id(), "host-2").await;
    Fixture { alice, bob, carol, group, host1, host2, handle2 }
}

impl Fixture {
    /// An approval of the open request, as `signer` would sign it; `edit` changes fields first.
    async fn approval_by(&self, signer: &TestClient, edit: impl FnOnce(&mut HostApproval)) -> (String, HostApproval) {
        let (_, _, ephemeral) = pairing_row(&self.handle2.request_id).await;
        let t = now();
        let mut a = HostApproval {
            approver_device_id: device_of(signer).await,
            approver_user_id: signer.user_id().to_string(),
            bot_user_id: self.host1.bot_user_id().to_string(),
            expires_at: t + 600,
            group_id: self.group.clone(),
            issued_at: t,
            new_host_ephemeral_pub: b64(&ephemeral),
            request_id: self.handle2.request_id.clone(),
            v: HOST_APPROVAL_VERSION,
        };
        edit(&mut a);
        let signed = bots::sign_approval(&signer.state, signer.user_id(), &device_of(signer).await, &a)
            .await
            .expect("sign approval");
        (signed, a)
    }

    /// Plant `signed` as the request's approval, run host 1, and return its one outcome.
    async fn host1_on(&self, signed: &str, a: &HostApproval) -> HostApprovalOutcome {
        plant_host_approval(&self.handle2.request_id, signed, &a.approver_user_id, &a.approver_device_id).await;
        let mut outcomes = self.host1.handle_approvals().await;
        assert_eq!(outcomes.len(), 1, "{outcomes:?}");
        outcomes.remove(0)
    }

    async fn assert_nothing_wrapped(&self) {
        let (status, wrapped, _) = pairing_row(&self.handle2.request_id).await;
        assert_eq!((status.as_str(), wrapped), ("pending", false), "nothing may be wrapped");
    }
}

fn refused(outcome: &HostApprovalOutcome) -> &ApprovalRefusal {
    match outcome {
        HostApprovalOutcome::Refused { reason, .. } => reason,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_host_refuses_every_approval_it_cannot_verify_and_wraps_nothing() {
    let f = fixture().await;

    // Signed by a member who is not an admin.
    let (signed, a) = f.approval_by(&f.bob, |_| {}).await;
    assert!(matches!(refused(&f.host1_on(&signed, &a).await), ApprovalRefusal::NotAdmin(_)));
    f.assert_nothing_wrapped().await;

    // Expired.
    let (signed, a) = f
        .approval_by(&f.alice, |a| {
            a.issued_at -= 1200;
            a.expires_at = a.issued_at + 600;
        })
        .await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::Expired);
    f.assert_nothing_wrapped().await;

    // A lifetime past ten minutes.
    let (signed, a) = f.approval_by(&f.alice, |a| a.expires_at = a.issued_at + 3600).await;
    assert!(matches!(refused(&f.host1_on(&signed, &a).await), ApprovalRefusal::Malformed(_)));
    f.assert_nothing_wrapped().await;

    // Naming another bot, or another group.
    let (signed, a) = f.approval_by(&f.alice, |a| a.bot_user_id = "01JOTHERBOT000000000000000".into()).await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::WrongBot);
    let (signed, a) = f.approval_by(&f.alice, |a| a.group_id = "01JOTHERGROUP00000000000000".into()).await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::WrongGroup);
    f.assert_nothing_wrapped().await;

    // Naming a different ephemeral key than the request's.
    let (signed, a) = f.approval_by(&f.alice, |a| a.new_host_ephemeral_pub = b64(&[9u8; 32])).await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::Tampered);
    f.assert_nothing_wrapped().await;

    // A forged device: alice's name, a device that is no leaf of hers.
    let (signed, mut a) = f.approval_by(&f.alice, |_| {}).await;
    a.approver_device_id = "01JFORGEDDEVICE00000000000".into();
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::RequestMismatch);
    let (signed, a) = f
        .approval_by(&f.alice, |a| a.approver_device_id = "01JFORGEDDEVICE00000000000".into())
        .await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::ApproverNotInGroup);
    // Alice's real device named, but signed by another key (bob's device).
    let (_, a) = f.approval_by(&f.alice, |_| {}).await;
    let forged = bots::sign_approval(&f.bob.state, f.bob.user_id(), &device_of(&f.bob).await, &a).await.unwrap();
    assert_eq!(refused(&f.host1_on(&forged, &a).await), &ApprovalRefusal::BadSignature);
    f.assert_nothing_wrapped().await;

    // The transparency log disagrees with the approver's key.
    let (signed, a) = f.approval_by(&f.alice, |_| {}).await;
    plant_host_approval(&f.handle2.request_id, &signed, &a.approver_user_id, &a.approver_device_id).await;
    let outcomes = f.host1.handle_approvals_with(AuditStatus::Alarm).await;
    assert_eq!(refused(&outcomes[0]), &ApprovalRefusal::TransparencyAlarm);
    f.assert_nothing_wrapped().await;

    // A tampered DS: the admin approved one key, the request now carries another.
    let (signed, a) = f.approval_by(&f.alice, |_| {}).await;
    let (_, _, honest) = pairing_row(&f.handle2.request_id).await;
    swap_pairing_ephemeral(&f.handle2.request_id, [5u8; 32]).await;
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::Tampered);
    f.assert_nothing_wrapped().await;
    swap_pairing_ephemeral(&f.handle2.request_id, honest.try_into().unwrap()).await;

    // A removed admin: carol approves, then leaves the group.
    let (signed, a) = f.approval_by(&f.carol, |_| {}).await;
    f.alice.remove_member(&f.group, f.carol.user_id()).await;
    f.host1.read_channel(&f.alice.general_channel_id(&f.group).await).await.unwrap();
    assert!(matches!(
        refused(&f.host1_on(&signed, &a).await),
        ApprovalRefusal::ApproverNotInGroup | ApprovalRefusal::NotAdmin(_)
    ));
    f.assert_nothing_wrapped().await;

    // The honest approval is accepted; replayed afterwards, it is refused.
    let (signed, a) = f.approval_by(&f.alice, |_| {}).await;
    assert!(matches!(f.host1_on(&signed, &a).await, HostApprovalOutcome::Wrapped { .. }));
    assert_eq!(refused(&f.host1_on(&signed, &a).await), &ApprovalRefusal::Replayed);
    let _ = &f.host2;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_admin_demoted_before_the_wrap_gets_none() {
    let f = fixture().await;
    let (signed, a) = f.approval_by(&f.carol, |_| {}).await;
    f.alice.set_member_role(&f.group, f.carol.user_id(), "member").await;
    assert!(matches!(refused(&f.host1_on(&signed, &a).await), ApprovalRefusal::NotAdmin(_)));
    f.assert_nothing_wrapped().await;
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_admin_side_refuses_a_wrong_code() {
    let f = fixture().await;
    let bot = f.host1.bot_user_id().to_string();

    // A code with no request behind it.
    let err = f
        .alice
        .invoke_try("add_bot_host", json!({ "botUserId": bot, "code": "00000000" }))
        .await
        .unwrap_err();
    assert!(err.contains(bots::PAIRING_CODE_MISMATCH), "{err}");

    // The DS swaps the request's key: the typed code no longer re-derives.
    swap_pairing_ephemeral(&f.handle2.request_id, [5u8; 32]).await;
    let err = f
        .alice
        .invoke_try("add_bot_host", json!({ "botUserId": bot, "code": f.handle2.code }))
        .await
        .unwrap_err();
    assert!(err.contains(bots::PAIRING_CODE_MISMATCH), "{err}");

    // A new-bot code typed into the add-host field is refused too.
    let fresh = BotHostClient::new().await;
    let handle = bots::start_new_bot_pairing(&fresh.state, None).await.unwrap();
    let err = f
        .alice
        .invoke_try("add_bot_host", json!({ "botUserId": bot, "code": handle.code }))
        .await
        .unwrap_err();
    assert!(err.contains(bots::PAIRING_CODE_MISMATCH), "{err}");

    // A member who is not an admin cannot create a bot.
    let handle = bots::start_new_bot_pairing(&fresh.state, None).await.unwrap();
    let err = f
        .bob
        .invoke_try("create_bot", json!({ "groupId": f.group, "code": handle.code, "displayName": "X" }))
        .await
        .unwrap_err();
    assert!(!err.is_empty());
}
