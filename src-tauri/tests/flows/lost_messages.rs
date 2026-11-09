//! A lost message is visible, and only a real loss is (#1312).
//!
//! Not a loss: messages sent before joining, and this device's own control frames.

use crate::harness::{
    arm_ds_fault, clear_ds_fault, ds_head_epoch, ds_head_generation, ds_prune_commit_log,
    enroll_second_device, expect_losses, local_generation_of, prune_commits_below,
    republish_key_packages, retire_generations_below, set_current_suite, wipe, DsFault, TestClient,
    SUITE_LEGACY,
};
use pollis_lib::commands::auth::UserProfile;
use serial_test::serial;

fn placeholders(msgs: &[serde_json::Value]) -> Vec<&serde_json::Value> {
    msgs.iter()
        .filter(|m| m["undecryptable"].as_bool() == Some(true))
        .collect()
}

fn contents(msgs: &[serde_json::Value]) -> Vec<&str> {
    msgs.iter().filter_map(|m| m["content"].as_str()).collect()
}

async fn add_member(
    admin: &TestClient,
    group_id: &str,
    channel_id: &str,
    member: &TestClient,
    profile: &UserProfile,
) {
    admin.invite(group_id, &profile.username).await;
    let invite_id = member
        .first_pending_invite()
        .await
        .expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite_id).await;
    member.poll().await;
    admin.process_commits_for(channel_id).await;
}

/// Bob's device leaves an epoch before reading what was sealed at it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_stranded_message_shows_as_one_placeholder() {
    wipe().await;
    expect_losses();

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let mut carol = TestClient::new().await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Loss").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;

    alice.send_channel_message(&channel_id, "read in time").await;
    let seen = bob.fetch_channel_messages(&channel_id).await;
    assert!(contents(&seen).contains(&"read in time"), "{seen:#?}");

    alice.send_channel_message(&channel_id, "will be lost").await;
    add_member(&alice, &group_id, &channel_id, &carol, &carol_p).await;

    // The bug, forced: a bare commit replay that skips the epoch's ingest.
    pollis_core::commands::mls::process_pending_commits_inner(
        &bob.state,
        &group_id,
        bob.user_id(),
    )
    .await
    .expect("bare replay");

    alice.send_channel_message(&channel_id, "after").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents = contents(&bob_msgs);
    assert!(bob_contents.contains(&"read in time"), "{bob_msgs:#?}");
    assert!(bob_contents.contains(&"after"), "{bob_msgs:#?}");
    assert!(!bob_contents.contains(&"will be lost"), "{bob_msgs:#?}");
    let lost = placeholders(&bob_msgs);
    assert_eq!(lost.len(), 1, "exactly the stranded message: {bob_msgs:#?}");
    assert!(lost[0]["content"].is_null());

    // The placeholder is written once.
    let again = bob.fetch_channel_messages(&channel_id).await;
    assert_eq!(placeholders(&again).len(), 1);

    let carol_msgs = carol.fetch_channel_messages(&channel_id).await;
    assert!(contents(&carol_msgs).contains(&"after"), "{carol_msgs:#?}");
    assert!(placeholders(&carol_msgs).is_empty(), "pre-join is not a loss: {carol_msgs:#?}");

    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    assert!(placeholders(&alice_msgs).is_empty(), "{alice_msgs:#?}");
}

/// A message corrupted in flight, so it arrives at the right epoch but won't open.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_message_that_will_not_open_shows_as_a_placeholder() {
    wipe().await;
    expect_losses();

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Corrupt").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.send_channel_message(&channel_id, "first").await;
    pollis_core::commands::messages::test_hooks::corrupt_next_send();
    alice.send_channel_message(&channel_id, "mangled in flight").await;
    alice.send_channel_message(&channel_id, "last").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"first") && got.contains(&"last"), "{msgs:#?}");
    assert!(!got.contains(&"mangled in flight"), "{msgs:#?}");
    assert_eq!(placeholders(&msgs).len(), 1, "{msgs:#?}");

    let mine = alice.fetch_channel_messages(&channel_id).await;
    assert!(contents(&mine).contains(&"mangled in flight"), "{mine:#?}");
    assert!(placeholders(&mine).is_empty(), "{mine:#?}");
}

/// Removed then re-added: the gap is the accepted "before you joined" loss, not a placeholder.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn messages_sent_while_removed_are_not_losses() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Churn").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.send_channel_message(&channel_id, "while bob is in").await;
    bob.fetch_channel_messages(&channel_id).await;

    alice.remove_member(&group_id, &bob_p.id).await;
    alice.send_channel_message(&channel_id, "while bob is out").await;
    bob.process_commits_for(&channel_id).await;

    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.send_channel_message(&channel_id, "bob is back").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert!(placeholders(&msgs).is_empty(), "a removal gap is not a loss: {msgs:#?}");
}

/// #1765: a removal gap passes silently only from the epoch the removal
/// produced. A message sealed while Bob was still in, that he never reached, is
/// a real loss even though he was later removed and came back by external join.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_message_sealed_before_the_removal_stays_a_loss() {
    wipe().await;
    expect_losses();

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let mut carol = TestClient::new().await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Gap").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.send_channel_message(&channel_id, "read in time").await;
    bob.fetch_channel_messages(&channel_id).await;

    // Sealed to Bob, who never reads it before he is removed.
    alice.send_channel_message(&channel_id, "will be lost").await;
    alice.remove_member(&group_id, &bob_p.id).await;
    alice.process_commits_for(&channel_id).await;
    add_member(&alice, &group_id, &channel_id, &carol, &carol_p).await;
    alice.send_channel_message(&channel_id, "while bob is out").await;

    // Re-add Bob with his Welcome lost, so he comes back by external join.
    arm_ds_fault(DsFault::DropWelcome);
    alice.invite(&group_id, &bob_p.username).await;
    clear_ds_fault();
    let invite_id = bob.first_pending_invite().await.expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    // A bare replay first, so the epoch "will be lost" was sealed at is passed
    // unread: it applies the removal, is refused the next commit, and rejoins.
    pollis_core::commands::mls::process_pending_commits_inner(
        &bob.state,
        &group_id,
        bob.user_id(),
    )
    .await
    .expect("bare replay");
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "bob is back").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert_eq!(
        placeholders(&msgs).len(),
        1,
        "only \"will be lost\", sealed before the removal, shows; the gap is silent: {msgs:#?}"
    );
}

/// Rotate alice's leaf `n` times, each reported, so her catch-up reports lift
/// the commit-log floor (`PRUNE_SLACK_EPOCHS` = 8).
async fn rotate(alice: &TestClient, group_id: &str, channel_id: &str, n: usize) {
    for _ in 0..n {
        assert!(alice.self_update(group_id).await, "alice's rotation must land");
        alice.process_commits_for(channel_id).await;
    }
}

/// The #1772 world: Bob reads up to his held epoch, falls behind while still a
/// member (alice rotates, then seals "sealed before the removal" one epoch
/// above where Bob is), is removed, and misses "while bob is out". Alice then
/// rotates far enough that, before #1772, her reports pruned the log past Bob.
/// Returns the epoch Bob still holds.
async fn lag_then_remove(
    alice: &TestClient,
    bob: &TestClient,
    bob_p: &UserProfile,
    group_id: &str,
    channel_id: &str,
) -> i64 {
    add_member(alice, group_id, channel_id, bob, bob_p).await;
    alice.send_channel_message(channel_id, "read in time").await;
    let seen = bob.fetch_channel_messages(channel_id).await;
    assert!(contents(&seen).contains(&"read in time"), "{seen:#?}");
    let held = ds_head_epoch(group_id).await;

    rotate(alice, group_id, channel_id, 1).await;
    alice.send_channel_message(channel_id, "sealed before the removal").await;
    rotate(alice, group_id, channel_id, 10).await;
    alice.remove_member(group_id, &bob_p.id).await;
    alice.process_commits_for(channel_id).await;
    alice.send_channel_message(channel_id, "while bob is out").await;
    rotate(alice, group_id, channel_id, 4).await;
    held
}

/// #1772: a lagging member removed and re-added by Welcome replays the log to
/// its own removal before the Welcome replaces its group. What was sealed to it
/// before the removal opens; what was sealed while it was out stays silent.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_lagging_member_re_added_reads_what_was_sealed_before_its_removal() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Lag").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let held = lag_then_remove(&alice, &bob, &bob_p, &group_id, &channel_id).await;

    // The removed device still pins the log at its last report.
    let report = ds_prune_commit_log(&group_id).await;
    assert!(report.floor <= held, "the DS kept bob's commits: {report:?}, held {held}");

    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "bob is back").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"sealed before the removal"), "sealed to bob, so it opens: {msgs:#?}");
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert!(placeholders(&msgs).is_empty(), "the time out is silent: {msgs:#?}");
}

/// #1772, rebuild path: the log is pruned past the removed device anyway (Tier
/// 2, or a DS that collected it), so it cannot learn where it was removed.
/// Nothing tells it which of what it never reached was sealed while it was
/// out, so it errs toward showing: the message sealed before the removal is
/// one placeholder, and the one sealed while it was out shows too.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_re_add_over_a_pruned_log_shows_what_was_sealed_before_the_removal() {
    wipe().await;
    expect_losses();

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Pruned").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    let held = lag_then_remove(&alice, &bob, &bob_p, &group_id, &channel_id).await;
    prune_commits_below(&group_id, ds_head_epoch(&group_id).await - 1).await;
    assert!(ds_head_epoch(&group_id).await - 1 > held);

    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "bob is back").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"sealed before the removal"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert_eq!(
        placeholders(&msgs).len(),
        2,
        "no proven removal epoch: both unread messages show, neither passes: {msgs:#?}"
    );
}

/// [`lag_then_remove`] on the retired suite, then alice ships the new one and
/// migrates the group on her own while Bob is out.
async fn lag_remove_then_migrate(
    alice: &TestClient,
    bob: &TestClient,
    bob_p: &UserProfile,
    group_id: &str,
    channel_id: &str,
) {
    lag_then_remove(alice, bob, bob_p, group_id, channel_id).await;
    set_current_suite(None);
    republish_key_packages(alice).await;
    republish_key_packages(bob).await;
    for _ in 0..2 {
        alice.poll().await;
        alice.sweep().await;
    }
    assert_eq!(ds_head_generation(group_id).await, 1, "the group migrated");
    alice.send_channel_message(channel_id, "in the successor before bob").await;
    rotate(alice, group_id, channel_id, 2).await;
}

/// Re-add Bob into the successor by Welcome and read his view of the channel.
async fn re_add_into_the_successor(
    alice: &TestClient,
    bob: &TestClient,
    bob_p: &UserProfile,
    group_id: &str,
    channel_id: &str,
) -> Vec<serde_json::Value> {
    add_member(alice, group_id, channel_id, bob, bob_p).await;
    alice.process_commits_for(channel_id).await;
    alice.send_channel_message(channel_id, "bob is back").await;
    bob.sweep().await;
    let msgs = bob.fetch_channel_messages(channel_id).await;
    assert_eq!(local_generation_of(bob, group_id).await, 1);
    msgs
}

/// #1772, successor path: the removed device's lineage is retired by a suite
/// migration and it is re-added into the successor by Welcome. It replays the
/// retired lineage to its removal before adopting the successor, so what was
/// sealed to it there opens, and nothing it was out for shows.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_lagging_member_re_added_into_a_successor_reads_what_was_sealed_before_its_removal() {
    wipe().await;
    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Successor").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    lag_remove_then_migrate(&alice, &bob, &bob_p, &group_id, &channel_id).await;
    let report = ds_prune_commit_log(&group_id).await;
    assert_eq!(report.generation_floor, 0, "bob still pins the retired lineage: {report:?}");

    let msgs = re_add_into_the_successor(&alice, &bob, &bob_p, &group_id, &channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"sealed before the removal"), "sealed to bob, so it opens: {msgs:#?}");
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert!(!got.contains(&"in the successor before bob"), "{msgs:#?}");
    assert!(placeholders(&msgs).is_empty(), "the time out is silent: {msgs:#?}");
}

/// #1772, successor path over a retired log: the old lineage is gone (Tier 2,
/// or a DS that collected it), so the device cannot reach its removal there.
/// It errs toward showing: both messages of the old lineage it never reached
/// show, and only what the successor sealed before its Welcome passes.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_re_add_into_a_successor_over_a_retired_lineage_shows_what_was_sealed_before_the_removal() {
    wipe().await;
    expect_losses();
    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Retired").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    lag_remove_then_migrate(&alice, &bob, &bob_p, &group_id, &channel_id).await;
    retire_generations_below(&group_id, 1).await;

    let msgs = re_add_into_the_successor(&alice, &bob, &bob_p, &group_id, &channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"sealed before the removal"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is out"), "{msgs:#?}");
    assert!(!got.contains(&"in the successor before bob"), "{msgs:#?}");
    assert_eq!(
        placeholders(&msgs).len(),
        2,
        "no proven removal epoch in the old lineage: both unread messages there show: {msgs:#?}"
    );
}

/// #1772: a user who leaves and is invited back passes the time away silently,
/// although the leave records no removal: the leave itself is the device's
/// own act.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn messages_sent_after_a_leave_are_not_losses() {
    wipe().await;

    let mut alice = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Leave").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    alice.send_channel_message(&channel_id, "before the leave").await;
    bob.fetch_channel_messages(&channel_id).await;
    bob.leave_group(&group_id).await;
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "while bob is away").await;
    rotate(&alice, &group_id, &channel_id, 2).await;

    // No member is online to commit the leaver's removal here, so the re-add
    // finds its leaf still in the tree and sends no Welcome: Bob rejoins by
    // external join.
    add_member(&alice, &group_id, &channel_id, &bob, &bob_p).await;
    bob.process_commits_for(&channel_id).await;
    alice.process_commits_for(&channel_id).await;
    alice.send_channel_message(&channel_id, "bob is back").await;

    let msgs = bob.fetch_channel_messages(&channel_id).await;
    let got = contents(&msgs);
    assert!(got.contains(&"bob is back"), "{msgs:#?}");
    assert!(!got.contains(&"while bob is away"), "{msgs:#?}");
    assert!(placeholders(&msgs).is_empty(), "time away after a leave is silent: {msgs:#?}");
}

/// False positives: a device's own receipts, deletes and edits can't be decrypted on fetch.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn own_control_frames_and_sibling_devices_are_not_losses() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice_p = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let dm_id = alice_d1
        .create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()])
        .await;
    bob.accept_dm_request(&dm_id).await;
    for c in [&alice_d1, &alice_d2, &bob] {
        c.fetch_dm_messages(&dm_id).await;
    }

    let first = alice_d1.send_channel_message_id(&dm_id, "hi from d1").await;
    for c in [&alice_d1, &alice_d2, &bob] {
        c.fetch_dm_messages(&dm_id).await;
    }
    bob.send_channel_message(&dm_id, "hi back").await;
    for c in [&alice_d1, &alice_d2, &bob] {
        c.fetch_dm_messages(&dm_id).await;
    }
    alice_d2.send_channel_message(&dm_id, "hi from d2").await;
    alice_d1.edit_message(&dm_id, &first, "hi from d1 (edited)").await;
    let doomed = bob.send_channel_message_id(&dm_id, "never mind").await;
    bob.delete_message(&doomed).await;

    for c in [&alice_d1, &alice_d2, &bob] {
        // Twice: first-read receipts arrive on the second.
        c.fetch_dm_messages(&dm_id).await;
        let msgs = c.fetch_dm_messages(&dm_id).await;
        assert!(
            placeholders(&msgs).is_empty(),
            "no message was lost, none may show as lost: {msgs:#?}"
        );
        assert!(contents(&msgs).contains(&"hi back"), "{msgs:#?}");
        assert!(contents(&msgs).contains(&"hi from d2"), "{msgs:#?}");
    }
}
