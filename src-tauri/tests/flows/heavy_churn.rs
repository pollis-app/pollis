use crate::harness::{wipe, TestClient};
use serial_test::serial;

/// Regression guard for issue #418: messages sent during heavy offline churn
/// must not be permanently dropped.
///
/// ## What this exercises
///
/// A member (Bob) joins a channel and then goes offline. While he is offline,
/// the group churns through several epochs — adds AND removes — with an
/// application message sent at EVERY epoch in between. When Bob comes back and
/// ingests, every message sent while he was a member must decrypt and be
/// present, and the message sent BEFORE he joined must NOT be delivered.
///
/// ## Why the OLD code fails this
///
/// `max_past_epochs` is openmls's default 0, so the ratchet keys for an epoch
/// are discarded the instant the group advances past it. The old ingest path
/// applied EVERY pending commit first — jumping Bob's local group straight to
/// head — and only then decrypted the backlog. Every message sealed at an
/// intermediate epoch (M1..M4 below) was then encrypted to keys that no longer
/// existed and decrypted as `WrongEpoch`; only the head-epoch message (M5)
/// survived. Worse, the buggy watermark took the global-max sent_at across
/// successes, so M5's success leapfrogged the watermark past M1..M4 and they
/// were never re-fetched — permanently dropped. On the old code Bob sees ONLY
/// M5, so the `M1` assertion below fails.
///
/// ## Why the NEW code passes
///
/// The fix decrypts each epoch's messages WHILE the local group is still at that
/// epoch, interleaved with the commit replay, so M1..M5 all decrypt and the
/// watermark only advances over the contiguous prefix of handled envelopes.
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

    // ── A message sent BEFORE Bob joins (epoch 0). Bob must NEVER see this —
    //    it is sealed at an epoch he was not a member of (bounded history). ──
    alice.send_channel_message(&channel_id, "M0-prejoin").await;

    // ── Bob joins (epoch advances to 1). He polls his Welcome to build his
    //    local group at the join epoch, then "goes offline": no further poll /
    //    process / ingest until the very end. ──
    join_member(&alice, &bob, &group_id, &channel_id, &bob_p.username).await;

    // M1 is sent at Bob's join epoch — the first message he must receive.
    alice.send_channel_message(&channel_id, "M1").await;

    // ── Heavy churn while Bob is offline: an add, a remove, an add, a remove,
    //    with an application message at every epoch in between. Each membership
    //    change is a commit that advances the epoch; each message is sealed at
    //    the epoch it was sent at. ──
    join_member(&alice, &carol, &group_id, &channel_id, &carol_p.username).await;
    alice.send_channel_message(&channel_id, "M2").await;

    alice.remove_member(&group_id, &carol_p.id).await;
    alice.send_channel_message(&channel_id, "M3").await;

    join_member(&alice, &dave, &group_id, &channel_id, &dave_p.username).await;
    alice.send_channel_message(&channel_id, "M4").await;

    alice.remove_member(&group_id, &dave_p.id).await;
    alice.send_channel_message(&channel_id, "M5").await;

    // ── Bob comes back online and ingests for the first time since joining.
    //    `get_channel_messages` (via `fetch_channel_messages`) drains welcomes,
    //    replays commits, and decrypts the backlog in one interleaved pass. ──
    let bob_msgs = bob.fetch_channel_messages(&channel_id).await;
    let bob_contents: Vec<&str> = bob_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();

    // LOAD-BEARING: every message sent while Bob was a member must be present.
    // M1..M4 are the intermediate-epoch messages the old apply-all-then-decrypt
    // path dropped; M5 is the head-epoch message it kept.
    for expected in ["M1", "M2", "M3", "M4", "M5"] {
        assert!(
            bob_contents.contains(&expected),
            "Bob (a continuous member through the churn) is missing in-membership \
             message {expected:?} — intermediate-epoch messages were dropped \
             (issue #418). got: {bob_contents:?}"
        );
    }

    // The pre-join message must NOT be delivered (bounded history, epoch < join).
    assert!(
        !bob_contents.contains(&"M0-prejoin"),
        "Bob received a message sent before he joined — pre-join history must not \
         be delivered. got: {bob_contents:?}"
    );
}

/// Invite `member` to `group_id`, accept, drain the Welcome, and replay commits
/// so the member is a fully-joined participant in the channel's MLS group.
/// Mirrors the proven join sequence in `rejoin.rs`.
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
    // Both sides settle on the add commit.
    inviter.process_commits_for(channel_id).await;
    member.process_commits_for(channel_id).await;
}
