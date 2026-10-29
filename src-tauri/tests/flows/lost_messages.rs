//! A lost message is visible, and only a real loss is (#1312).
//!
//! Not a loss: messages sent before joining, and this device's own control frames.

use crate::harness::{enroll_second_device, expect_losses, wipe, TestClient};
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
