//! A read clears its notifications on every device (#1508), end to end through
//! the real command pipeline and the in-process DS.
//! Handles are minted directly with `mint_push_handles`; the silent push is pinned in `pollis-delivery`.

use std::time::{Duration, Instant};

use crate::harness::{writable_remote, TestClient};
use pollis_delivery::push::mint_push_handles;
use serial_test::serial;

async fn handles_of(user: &str, conversation: &str) -> Vec<(String, bool)> {
    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let mut rows = conn
        .query(
            "SELECT handle, dismissed_at IS NOT NULL FROM push_handle \
             WHERE user_id = ?1 AND conversation_id = ?2 ORDER BY handle",
            libsql::params![user.to_string(), conversation.to_string()],
        )
        .await
        .expect("push_handle read");
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.expect("row") {
        out.push((row.get::<String>(0).unwrap(), row.get::<i64>(1).unwrap() == 1));
    }
    out
}

async fn await_dismissed(user: &str, conversation: &str, handle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if handles_of(user, conversation)
            .await
            .iter()
            .any(|(h, dismissed)| h == handle && *dismissed)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    false
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
async fn reading_a_conversation_clears_its_notifications_and_only_those() {
    crate::harness::wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("Dismiss Test").await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("pending invite");
    bob.accept_invite(invite["id"].as_str().expect("invite id")).await;
    bob.poll().await;
    alice.poll().await;
    let general = alice.general_channel_id(&group_id).await;
    let other = alice.create_channel(&group_id, "elsewhere").await;
    bob.poll().await;

    alice.send_channel_message(&general, "are we shipping today?").await;
    bob.poll().await;
    // The cursor is resolved from messages this device holds.
    let seen = bob.fetch_channel_messages(&general).await;
    assert!(
        seen.iter().any(|m| m["content"] == "are we shipping today?"),
        "bob has the message: {seen:?}"
    );

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let bob_ids = vec![bob_p.id.clone()];
    let in_general = mint_push_handles(&conn, &bob_ids, &general, "channel").await[&bob_p.id].clone();
    let in_other = mint_push_handles(&conn, &bob_ids, &other, "channel").await[&bob_p.id].clone();
    conn.execute(
        "UPDATE push_handle SET created_at = datetime('now', '-5 seconds')",
        (),
    )
    .await
    .expect("backdate");
    drop(conn);

    assert!(mark_read(&bob, &general).await, "there was something to read");
    assert!(
        await_dismissed(&bob_p.id, &general, &in_general).await,
        "reading general must clear its notification"
    );
    assert_eq!(handles_of(&bob_p.id, &other).await, vec![(in_other, false)]);

    let remote = writable_remote().await;
    let conn = remote.conn().await.expect("conn");
    let later = mint_push_handles(&conn, &bob_ids, &general, "channel").await[&bob_p.id].clone();
    drop(conn);
    assert!(!mark_read(&bob, &general).await);
    tokio::time::sleep(Duration::from_secs(4)).await;
    let general_now = handles_of(&bob_p.id, &general).await;
    assert!(
        general_now.contains(&(later.clone(), false)),
        "a newer notification must not be cleared by an older read: {general_now:?}"
    );
}
