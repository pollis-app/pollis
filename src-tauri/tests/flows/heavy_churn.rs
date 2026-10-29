use crate::harness::{wipe, TestClient};
use serial_test::serial;

/// #418: each epoch's messages must decrypt before its commit is applied (`max_past_epochs` is 0).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn offline_churn_delivers_every_in_membership_message() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let mut dave = TestClient::new().await;

    let _alice = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("Churn").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    // Sealed before Bob joined: he must never see it.
    alice.send_channel_message(&channel_id, "M0-prejoin").await;

    // Bob joins, then goes offline until the end.
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    alice.send_channel_message(&channel_id, "M1").await;

    // Churn with a message at every epoch.
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.send_channel_message(&channel_id, "M2").await;

    alice.remove_member(&group_id, &carol_p.id).await;
    alice.send_channel_message(&channel_id, "M3").await;

    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    alice.send_channel_message(&channel_id, "M4").await;

    alice.remove_member(&group_id, &dave_p.id).await;
    alice.send_channel_message(&channel_id, "M5").await;

    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();

    // Load-bearing: M1..M4 are what apply-all-then-decrypt dropped.
    for expected in ["M1", "M2", "M3", "M4", "M5"] {
        assert!(
            bob_contents.contains(&expected),
            "Bob (a continuous member through the churn) is missing in-membership \
             message {expected:?} — intermediate-epoch messages were dropped \
             (issue #418). got: {bob_contents:?}"
        );
    }

    assert!(
        !bob_contents.contains(&"M0-prejoin"),
        "Bob received a message sent before he joined — pre-join history must not \
         be delivered. got: {bob_contents:?}"
    );
}

/// Invite, accept, drain the Welcome and replay commits.
async fn join_member(
    inviter: &TestClient,
    member: &TestClient,
    group_id: &str,
    channel_id: &str,
    member_username: &str,
) {
    inviter.invite(group_id, member_username).await;
    let invite_id = member
        .first_pending_invite()
        .await
        .expect("member should have a pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite_id).await;
    member.poll().await;
    inviter.process_commits_for(channel_id).await;
    member.process_commits_for(channel_id).await;
}
