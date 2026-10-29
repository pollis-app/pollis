//! #1412: a device whose KeyPackage pool runs dry is left out of new
//! conversations, and must still end up in every one of them.
//! These play a pre-#1571 build (no last-resort package), still in the stores.

use crate::harness::{enroll_second_device, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

const CONVERSATIONS: usize = 6;

/// Pool only, no last-resort package (pre-#1571).
async fn as_pre_last_resort_build(client: &TestClient) {
    let (user, device) = identity(client).await;
    crate::harness::writable_remote()
        .await
        .conn()
        .await
        .expect("remote conn")
        .execute(
            "DELETE FROM mls_key_package WHERE user_id = ?1 AND device_id = ?2 AND last_resort = 1",
            libsql::params![user, device],
        )
        .await
        .expect("drop the last-resort package");
}

async fn identity(client: &TestClient) -> (String, String) {
    let device = client
        .state
        .device_id
        .lock()
        .await
        .clone()
        .expect("device id set");
    (client.user_id().to_string(), device)
}

async fn tree_has(client: &TestClient, conversation_id: &str, who: &(String, String)) -> bool {
    pollis_lib::commands::mls::local_tree_members(&client.state, conversation_id)
        .await
        .contains(who)
}

async fn read_channel(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .invoke_json(
            "ingest_channel_envelopes",
            json!({ "userId": client.user_id(), "channelId": channel_id }),
        )
        .await;
    let page = client
        .invoke_json("read_channel_messages", json!({ "channelId": channel_id, "limit": 50 }))
        .await;
    page["messages"]
        .as_array()
        .expect("messages array")
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// `alice_d2` runs nothing while alice joins [`CONVERSATIONS`] groups on `alice_d1`.
async fn strand_second_device(
    bob: &TestClient,
    alice_d1: &TestClient,
    alice_username: &str,
) -> Vec<String> {
    let mut groups = Vec::with_capacity(CONVERSATIONS);
    for i in 0..CONVERSATIONS {
        let group = bob.create_group(&format!("Group {i}")).await;
        bob.invite(&group, alice_username).await;
        let invite = alice_d1
            .first_pending_invite()
            .await
            .expect("alice_d1 sees the invite");
        alice_d1
            .accept_invite(invite["id"].as_str().expect("invite id"))
            .await;
        groups.push(group);
    }
    groups
}

/// If this ever stops holding, the scenario no longer exercises #1412.
async fn assert_stranded(bob: &TestClient, groups: &[String], d2: &(String, String)) {
    bob.sweep().await;
    let last = groups.last().expect("groups");
    assert!(
        !tree_has(bob, last, d2).await,
        "precondition: alice_d2's pool should have run dry before the last group, \
         leaving it without a leaf there"
    );
    assert!(
        tree_has(bob, &groups[0], d2).await,
        "precondition: alice_d2 was added to the first group while it still had packages"
    );
}

/// Leaf in bob's tree (the one sealing for it), and messages both ways.
async fn assert_full_member_everywhere(
    bob: &TestClient,
    alice_d2: &TestClient,
    groups: &[String],
    d2: &(String, String),
) {
    bob.sweep().await;
    for (i, group) in groups.iter().enumerate() {
        assert!(
            tree_has(bob, group, d2).await,
            "group {i}: alice_d2 must be in the tree"
        );
        let channel = bob.general_channel_id(group).await;
        let from_bob = format!("bob in group {i}");
        bob.send_channel_message(&channel, &from_bob).await;
        let got = read_channel(alice_d2, &channel).await;
        assert!(
            got.contains(&from_bob),
            "group {i}: alice_d2 must read bob's message, got {got:?}"
        );

        let from_d2 = format!("alice_d2 in group {i}");
        alice_d2.send_channel_message(&channel, &from_d2).await;
        let got = read_channel(bob, &channel).await;
        assert!(
            got.contains(&from_d2),
            "group {i}: bob must read alice_d2's message, got {got:?}"
        );
    }
}

/// Desktop shape: cold launch / unlock runs the sweep.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_whose_pool_ran_dry_joins_every_conversation_on_its_next_sweep() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("bob@test.local").await;
    let d2 = identity(&alice_d2).await;
    as_pre_last_resort_build(&alice_d2).await;

    let groups = strand_second_device(&bob, &alice_d1, &alice.username).await;
    assert_stranded(&bob, &groups, &d2).await;

    alice_d2.sweep().await;

    assert_full_member_everywhere(&bob, &alice_d2, &groups, &d2).await;

    let next = bob.create_group("After recovery").await;
    bob.invite(&next, &alice.username).await;
    assert!(
        tree_has(&bob, &next, &d2).await,
        "after coming back, alice_d2's pool must be refilled so a new group can add it"
    );

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}

/// Mobile shape: a per-conversation ingest, no sweep.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_whose_pool_ran_dry_joins_a_conversation_when_it_ingests_it() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("bob@test.local").await;
    let d2 = identity(&alice_d2).await;
    as_pre_last_resort_build(&alice_d2).await;

    let groups = strand_second_device(&bob, &alice_d1, &alice.username).await;
    assert_stranded(&bob, &groups, &d2).await;

    for group in &groups {
        let channel = bob.general_channel_id(group).await;
        read_channel(&alice_d2, &channel).await;
    }

    assert_full_member_everywhere(&bob, &alice_d2, &groups, &d2).await;

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}

/// A current member re-adds the device once it has a KeyPackage (old-build recovery).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_re_adds_a_missing_device_once_it_publishes_key_packages() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("bob@test.local").await;
    let d2 = identity(&alice_d2).await;
    as_pre_last_resort_build(&alice_d2).await;

    let groups = strand_second_device(&bob, &alice_d1, &alice.username).await;
    assert_stranded(&bob, &groups, &d2).await;

    crate::harness::republish_key_packages(&alice_d2).await;

    bob.sweep().await;
    let last = groups.last().expect("groups");
    assert!(
        tree_has(&bob, last, &d2).await,
        "bob's sweep must re-add alice_d2 once it has a KeyPackage again"
    );

    alice_d2.poll().await;
    let channel = bob.general_channel_id(last).await;
    bob.send_channel_message(&channel, "welcome back").await;
    let got = read_channel(&alice_d2, &channel).await;
    assert!(got.contains(&"welcome back".to_string()), "got {got:?}");

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}

/// DM shape (#1410): opening each DM on `alice_d2` must make it a member.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_whose_pool_ran_dry_joins_every_dm_when_it_opens_them() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let d2 = identity(&alice_d2).await;
    as_pre_last_resort_build(&alice_d2).await;

    let mut peers: Vec<TestClient> = Vec::with_capacity(CONVERSATIONS);
    let mut dms: Vec<String> = Vec::with_capacity(CONVERSATIONS);
    for i in 0..CONVERSATIONS {
        let mut peer = TestClient::new().await;
        let me = peer.sign_up(&format!("peer{i}@test.local")).await;
        let dm = peer.create_dm(&[me.id.as_str(), alice.id.as_str()]).await;
        alice_d1.accept_dm_request(&dm).await;
        peers.push(peer);
        dms.push(dm);
    }

    let (last_peer, last_dm) = (peers.last().expect("peers"), dms.last().expect("dms"));
    last_peer.fetch_dm_messages(last_dm).await;
    assert!(
        !tree_has(last_peer, last_dm, &d2).await,
        "precondition: alice_d2's pool should have run dry before the last DM"
    );

    for (i, (peer, dm)) in peers.iter().zip(&dms).enumerate() {
        alice_d2.fetch_dm_messages(dm).await;
        peer.fetch_dm_messages(dm).await;
        assert!(tree_has(peer, dm, &d2).await, "dm {i}: alice_d2 must be in the tree");

        let from_peer = format!("peer {i} to alice");
        peer.send_channel_message(dm, &from_peer).await;
        let got: Vec<String> = alice_d2
            .fetch_dm_messages(dm)
            .await
            .iter()
            .filter_map(|m| m["content"].as_str().map(str::to_string))
            .collect();
        assert!(got.contains(&from_peer), "dm {i}: alice_d2 must read it, got {got:?}");
    }

    drop(alice_d1);
    drop(alice_d2);
    drop(peers);
}

async fn unclaimed(who: &(String, String)) -> i64 {
    let conn = crate::harness::writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND last_resort = 0",
            libsql::params![who.0.clone(), who.1.clone()],
        )
        .await
        .expect("count");
    rows.next().await.expect("row").expect("row").get::<i64>(0).expect("count")
}

/// Burnt claims drain the pool with no Welcome; the drain must refill once regardless.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_pool_burnt_without_welcomes_is_refilled_by_the_next_drain() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let me = identity(&alice).await;
    assert_eq!(unclaimed(&me).await, 5, "sign-in publishes a full pool");

    // Five burnt claims: claimed packages, no Welcome.
    crate::harness::writable_remote()
        .await
        .conn()
        .await
        .expect("remote conn")
        .execute(
            "UPDATE mls_key_package SET claimed = 1 WHERE user_id = ?1 AND device_id = ?2",
            libsql::params![me.0.clone(), me.1.clone()],
        )
        .await
        .expect("burn the pool");
    assert_eq!(unclaimed(&me).await, 0);

    tokio::join!(alice.poll(), alice.poll(), alice.poll());
    alice.poll().await;

    assert_eq!(
        unclaimed(&me).await,
        5,
        "the drain must refill a pool it reports empty — exactly to the target"
    );
}
