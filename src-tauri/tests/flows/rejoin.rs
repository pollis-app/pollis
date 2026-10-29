use crate::harness::{wipe, TestClient};
use serial_test::serial;

/// Leave + rejoin: the stayer's catch-up verify must use the main DB, or it defers the re-add commit.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn member_rejoin_messages_visible_to_stayer() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Rejoin").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    alice.process_commits_for(&channel_id).await;
    bob.process_commits_for(&channel_id).await;

    alice.send_channel_message(&channel_id, "before-leave").await;
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"before-leave"),
        "bob should decrypt alice's message before leaving, got: {bob_contents:?}"
    );

    alice
        .set_member_role(&group_id, bob.user_id(), "admin")
        .await;

    bob.leave_group(&group_id).await;
    alice.process_commits_for(&channel_id).await;

    alice.invite(&group_id, &bob_profile.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have a pending re-invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    // A's catch-up here runs the cross-signing verify on the re-add.
    bob.process_commits_for(&channel_id).await;
    alice.process_commits_for(&channel_id).await;

    let members = alice.group_member_ids(&group_id).await;
    assert!(
        members.contains(&bob_profile.id),
        "bob must be a current member after rejoining, got: {members:?}"
    );

    // Load-bearing: on the buggy tree A stayed at the old epoch and cannot decrypt this.
    bob.send_channel_message(&channel_id, "rejoin-from-bob").await;
    alice.process_commits_for(&channel_id).await;
    let alice_msgs = alice.fetch_channel_messages(&channel_id).await;
    let alice_contents: Vec<&str> =
        alice_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        alice_contents.contains(&"rejoin-from-bob"),
        "STAYER A could not read rejoiner B's post-rejoin message — the re-add \
         commit was deferred (cross-signing verify misrouted to the log DB). \
         got: {alice_msgs:#?}"
    );

    alice.send_channel_message(&channel_id, "rejoin-from-alice").await;
    bob.process_commits_for(&channel_id).await;
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
    assert!(
        bob_contents.contains(&"rejoin-from-alice"),
        "rejoiner B could not read stayer A's post-rejoin message, got: {bob_contents:?}"
    );

    drop(alice);
    drop(bob);
}
