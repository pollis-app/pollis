//! Channel pushes by default and the per-channel mute (#1431), end to end
//! through the real command pipeline and the in-process DS.
//!
//! What a send asks for is observed on the wire (`sent_push_audience`); who
//! that wakes is then decided by the DS's own `push_recipients` against the
//! same database, so the two halves are tested together rather than each
//! against an assumption about the other.

use crate::harness::{
    await_wake_up, enroll_second_device, sent_push_audience, wipe, writable_remote, TestClient,
};
use pollis_delivery::push::{push_recipients, PushAudience};
use serial_test::serial;

async fn muted(client: &TestClient) -> Vec<String> {
    client
        .invoke_json(
            "list_muted_channels",
            serde_json::json!({ "userId": client.user_id() }),
        )
        .await
        .as_array()
        .expect("muted channel list")
        .iter()
        .map(|v| v.as_str().expect("channel id").to_string())
        .collect()
}

async fn set_muted(client: &TestClient, channel_id: &str, muted: bool) {
    client
        .invoke_json(
            "set_channel_muted",
            serde_json::json!({
                "userId": client.user_id(),
                "channelId": channel_id,
                "muted": muted,
            }),
        )
        .await;
}

/// Who the DS wakes for the send of `message_id`, by its own rule.
async fn woken(message_id: &str, sender_id: &str) -> Vec<String> {
    let sent = sent_push_audience(message_id).expect("the send reached the DS");
    let Some(audience) = PushAudience::from_send(sent.push_to, sent.push_mentions) else {
        return Vec::new();
    };
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut ids = push_recipients(&conn, &sent.conversation_id, sender_id, &audience)
        .await
        .expect("push recipients");
    ids.sort();
    ids
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn channel_messages_push_everyone_and_a_mute_holds_except_for_a_direct_mention() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("Mute Test").await;
    for (who, profile) in [(&bob, &bob_p), (&carol, &carol_p)] {
        alice.invite(&group_id, &profile.username).await;
        let invite = who.first_pending_invite().await.expect("pending invite");
        who.accept_invite(invite["id"].as_str().expect("invite id")).await;
        who.poll().await;
    }
    alice.poll().await;
    let channel = alice.general_channel_id(&group_id).await;

    // Nothing is muted by default, and ordinary chatter now pushes everyone.
    assert!(muted(&bob).await.is_empty());
    let chatter = alice.send_channel_message_id(&channel, "shipping the build now").await;
    let mut everyone = vec![bob_p.id.clone(), carol_p.id.clone()];
    everyone.sort();
    assert_eq!(woken(&chatter, &alice_p.id).await, everyone, "chatter pushes every other member");

    // Bob mutes the channel; it is stored for his account, so a second device
    // of his reads the same mute.
    set_muted(&bob, &channel, true).await;
    assert_eq!(muted(&bob).await, vec![channel.clone()]);
    let bob2 = enroll_second_device(&bob, "bob@test.local").await;
    assert_eq!(muted(&bob2).await, vec![channel.clone()], "the mute syncs across bob's devices");
    assert!(muted(&carol).await.is_empty(), "a mute is per account");

    // Chatter and @all skip bob; a direct @bob reaches him through the mute.
    let chatter = alice.send_channel_message_id(&channel, "more chatter").await;
    assert_eq!(woken(&chatter, &alice_p.id).await, vec![carol_p.id.clone()]);
    let at_all = alice.send_channel_message_id(&channel, "@all standup in 5").await;
    assert_eq!(woken(&at_all, &alice_p.id).await, vec![carol_p.id.clone()]);
    let at_bob = alice
        .send_channel_message_id(&channel, &format!("can you look @{}", bob_p.username))
        .await;
    assert_eq!(woken(&at_bob, &alice_p.id).await, everyone, "a direct mention gets through");
    let sent = sent_push_audience(&at_bob).expect("sent");
    assert_eq!(sent.push_mentions, Some(vec![bob_p.id.clone()]));

    // Named inside an @all, bob gets the louder user_mention ping (the one his
    // client lets through a mute); carol gets the plain all_mention.
    let both = alice
        .send_channel_message_id(&channel, &format!("@all and @{} especially", bob_p.username))
        .await;
    assert_eq!(woken(&both, &alice_p.id).await, everyone);
    let bob_inbox = format!("inbox-{}", bob_p.id);
    let carol_inbox = format!("inbox-{}", carol_p.id);
    assert!(
        await_wake_up(&bob_inbox, |p| p["type"] == "user_mention").await.is_some(),
        "a directly named member is pinged as a user mention"
    );
    assert!(await_wake_up(&carol_inbox, |p| p["type"] == "all_mention").await.is_some());

    // Unmuting restores ordinary pushes.
    set_muted(&bob2, &channel, false).await;
    assert!(muted(&bob).await.is_empty(), "an unmute on one device clears it on the other");
    let chatter = alice.send_channel_message_id(&channel, "back to normal").await;
    assert_eq!(woken(&chatter, &alice_p.id).await, everyone);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn only_a_member_can_mute_and_leaving_drops_the_mute() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut mallory = TestClient::new().await;
    let _ = alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let _ = mallory.sign_up("mallory@test.local").await;

    let group_id = alice.create_group("Mute Authz").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    let channel = alice.general_channel_id(&group_id).await;

    // A non-member cannot mute (or learn anything about) the channel.
    let refused = mallory
        .invoke_try(
            "set_channel_muted",
            serde_json::json!({ "userId": mallory.user_id(), "channelId": channel, "muted": true }),
        )
        .await;
    assert!(refused.is_err(), "a non-member's mute must be refused");
    assert!(muted(&mallory).await.is_empty());

    // A DM cannot be muted: it is personal and always notifies.
    let dm = alice.create_dm(&[bob_p.id.as_str()]).await;
    let refused = alice
        .invoke_try(
            "set_channel_muted",
            serde_json::json!({ "userId": alice.user_id(), "channelId": dm, "muted": true }),
        )
        .await;
    assert!(refused.is_err(), "a DM is not mutable");

    set_muted(&bob, &channel, true).await;
    assert_eq!(muted(&bob).await, vec![channel.clone()]);
    bob.leave_group(&group_id).await;
    assert!(muted(&bob).await.is_empty(), "leaving the group drops its mutes");
}
