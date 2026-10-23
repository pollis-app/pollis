//! #1412: a device whose KeyPackage pool runs dry is left out of new
//! conversations, and must still end up in every one of them.
//!
//! The pool is 5 packages. Every conversation that adds a device claims one, and
//! the device only tops the pool back up when it runs (processing Welcomes, or
//! signing in). So a backgrounded phone added to a sixth conversation before it
//! wakes has nothing left to claim, and reconcile skips it — the conversation
//! commits without its leaf and there is no Welcome for it.
//!
//! Before the fix nothing ever put that device in: the sweep backstop only
//! removed stale leaves, and the device's own catch-up never visited a
//! conversation it held no group for. These tests pin both recovery halves:
//!
//! * the stranded device itself, coming back (desktop cold launch / unlock =
//!   the sweep; mobile foreground = a per-conversation ingest), joins every
//!   conversation it belongs to and is missing from, and refills its pool;
//! * a CURRENT member, on its own sweep, re-adds a missing device the moment it
//!   has a KeyPackage again — which is how a device running an older build (one
//!   that only republishes its pool at sign-in) is recovered with no update.

use crate::harness::{enroll_second_device, wipe, TestClient};
use serde_json::json;
use serial_test::serial;

/// One more than the pool size, so the last conversation finds it empty.
const CONVERSATIONS: usize = 6;

/// `(user_id, device_id)` of `client`.
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

/// Whether `client`'s local tree for `conversation_id` holds `who`'s leaf.
async fn tree_has(client: &TestClient, conversation_id: &str, who: &(String, String)) -> bool {
    pollis_lib::commands::mls::local_tree_members(&client.state, conversation_id)
        .await
        .contains(who)
}

/// Decrypted message bodies `client` holds for `channel_id`, after ingesting.
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

/// Alice runs two devices; `alice_d2` is "backgrounded" — it executes nothing
/// at all while bob puts alice in [`CONVERSATIONS`] new groups, which alice
/// accepts on `alice_d1`. Returns the groups, oldest first.
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

/// The failure, stated: the device holds no leaf in the last group. If this
/// ever stops holding, the scenario no longer exercises #1412.
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

/// Every group: the stranded device holds a leaf (in bob's tree, which is the
/// one that seals for it), reads bob's message, and bob reads its reply.
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

/// The device itself recovers when it comes back — the desktop shape: cold
/// launch / unlock / reconnect runs the sweep.
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

    let groups = strand_second_device(&bob, &alice_d1, &alice.username).await;
    assert_stranded(&bob, &groups, &d2).await;

    alice_d2.sweep().await;

    assert_full_member_everywhere(&bob, &alice_d2, &groups, &d2).await;

    // And the pool is back up, so the next conversation does not strand it.
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

/// The mobile shape: a phone returning to the foreground does not run the
/// sweep, it ingests each conversation in its list. That alone must be enough.
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

/// A current member re-adds the missing device as soon as it has a KeyPackage
/// again, without the device doing anything but publishing one. This is the
/// path that recovers a device on an older build, whose only refill is the pool
/// it republishes when it signs in.
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

    let groups = strand_second_device(&bob, &alice_d1, &alice.username).await;
    assert_stranded(&bob, &groups, &d2).await;

    // Sign-in on an old build: a fresh pool, nothing else.
    crate::harness::republish_key_packages(&alice_d2).await;

    // Bob's next sweep is what notices.
    bob.sweep().await;
    let last = groups.last().expect("groups");
    assert!(
        tree_has(&bob, last, &d2).await,
        "bob's sweep must re-add alice_d2 once it has a KeyPackage again"
    );

    // The Welcome is waiting; the device reads from then on.
    alice_d2.poll().await;
    let channel = bob.general_channel_id(last).await;
    bob.send_channel_message(&channel, "welcome back").await;
    let got = read_channel(&alice_d2, &channel).await;
    assert!(got.contains(&"welcome back".to_string()), "got {got:?}");

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}

/// The DM shape of the same failure — #1412 was found investigating a DM that a
/// second device never showed (#1410). Six people each open a DM with alice,
/// who accepts on `alice_d1`; the sixth finds `alice_d2`'s pool empty. Opening
/// each DM on `alice_d2` (what a phone's foreground pass does) must make it a
/// full member of every one.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_whose_pool_ran_dry_joins_every_dm_when_it_opens_them() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let d2 = identity(&alice_d2).await;

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

/// The unclaimed packages of `(user_id, device_id)`, counted server-side.
async fn unclaimed(who: &(String, String)) -> i64 {
    let conn = crate::harness::writable_remote().await.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0",
            libsql::params![who.0.clone(), who.1.clone()],
        )
        .await
        .expect("count");
    rows.next().await.expect("row").expect("row").get::<i64>(0).expect("count")
}

/// Packages leave the pool without a Welcome ever reaching the device — a
/// claim whose commit lost its epoch race is burnt, and so is a leaf the
/// committer refused. Refilling only after processing a Welcome let exactly
/// those drain a device to zero unseen. The drain reports the count now, so the
/// next one refills, Welcomes or not — and only once, however many drains run.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_pool_burnt_without_welcomes_is_refilled_by_the_next_drain() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let me = identity(&alice).await;
    assert_eq!(unclaimed(&me).await, 5, "sign-in publishes a full pool");

    // Server-side stand-in for five burnt claims: lost-race commits leave the
    // package claimed and write no Welcome.
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

    // Several drains at once, as a foreground pass makes them.
    tokio::join!(alice.poll(), alice.poll(), alice.poll());
    alice.poll().await;

    assert_eq!(
        unclaimed(&me).await,
        5,
        "the drain must refill a pool it reports empty — exactly to the target"
    );
}
