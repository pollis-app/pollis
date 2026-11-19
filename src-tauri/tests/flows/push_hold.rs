//! Reading at a desktop never buzzes the phone (#2056), end to end through the real command
//! pipeline and the in-process DS: the desktop's active report holds the push, its read drops it,
//! going inactive releases what is left, and a read a moment after arrival clears that message's
//! push by position, not by clock.

use std::time::{Duration, Instant};

use crate::harness::{writable_remote, TestClient};
use pollis_delivery::push::{mint_push_handles_for, PushMessage};
use serial_test::serial;

async fn held(user: &str, conversation: &str) -> Option<String> {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT message_id FROM push_held WHERE user_id = ?1 AND conversation_id = ?2",
            libsql::params![user.to_string(), conversation.to_string()],
        )
        .await
        .expect("push_held read");
    rows.next().await.expect("row").map(|r| r.get::<String>(0).unwrap())
}

async fn until<F, Fut>(what: &str, mut check: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if check().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("timed out waiting for: {what}");
}

async fn dismissed(user: &str, handle: &str) -> bool {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT 1 FROM push_handle WHERE user_id = ?1 AND handle = ?2 AND dismissed_at IS NOT NULL",
            libsql::params![user.to_string(), handle.to_string()],
        )
        .await
        .expect("push_handle read");
    rows.next().await.expect("row").is_some()
}

async fn report(client: &TestClient, active: bool) {
    client
        .invoke_json(
            "report_push_activity",
            serde_json::json!({ "userId": client.user_id(), "active": active }),
        )
        .await;
}

async fn mark_read(client: &TestClient, conversation: &str) -> bool {
    client
        .invoke_json(
            "mark_conversation_read",
            serde_json::json!({ "conversationId": conversation, "userId": client.user_id() }),
        )
        .await
        .as_bool()
        .expect("mark_conversation_read answers a bool")
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_desktop_in_use_holds_the_phone_push_and_its_read_drops_it() {
    crate::harness::wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Hold Test").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.poll().await;
    let general = alice.general_channel_id(&group_id).await;
    bob.poll().await;

    // Bob is at his desktop: alice's message is held for his phone, not pushed.
    report(&bob, true).await;
    alice.send_channel_message(&general, "are you around?").await;
    until("the push is held", || async { held(&bob_p.id, &general).await.is_some() }).await;

    // He reads it there: the held push is dropped, so going idle sends nothing for it.
    bob.poll().await;
    let seen = bob.fetch_channel_messages(&general).await;
    assert!(seen.iter().any(|m| m["content"] == "are you around?"), "bob has it: {seen:?}");
    assert!(mark_read(&bob, &general).await);
    until("the read drops the hold", || async { held(&bob_p.id, &general).await.is_none() }).await;

    // Another message while he is still here, then he leaves without reading: it is released.
    alice.send_channel_message(&general, "ping").await;
    until("the second push is held", || async { held(&bob_p.id, &general).await.is_some() }).await;
    report(&bob, false).await;
    until("going inactive releases it", || async { held(&bob_p.id, &general).await.is_none() }).await;

    // Away from the desk: nothing is held any more.
    alice.send_channel_message(&general, "still there?").await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(held(&bob_p.id, &general).await, None, "an inactive user is pushed at once");
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_read_a_moment_after_arrival_clears_that_push() {
    crate::harness::wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Position Test").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.poll().await;
    let general = alice.general_channel_id(&group_id).await;
    bob.poll().await;

    alice.send_channel_message(&general, "quick one").await;
    bob.poll().await;
    let seen = bob.fetch_channel_messages(&general).await;
    let msg = seen
        .iter()
        .find(|m| m["content"] == "quick one")
        .expect("bob has the message");
    // The phone's push for it, minted now: the old time cutoff (read time minus the client's
    // round-up) would leave it in the tray.
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let message = PushMessage {
        id: msg["id"].as_str().expect("id").to_string(),
        sent_at: msg["sent_at"].as_str().expect("sent_at").to_string(),
    };
    let h = mint_push_handles_for(&conn, &[bob_p.id.clone()], &general, "channel", Some(&message))
        .await[&bob_p.id]
        .clone();
    drop(conn);

    assert!(mark_read(&bob, &general).await);
    until("the read clears the push by position", || async { dismissed(&bob_p.id, &h).await }).await;
}
