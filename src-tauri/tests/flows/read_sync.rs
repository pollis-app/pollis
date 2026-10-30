//! Cross-device unread (#1628), end to end through the real command pipeline and the in-process DS:
//! a wake reports only news, a read made on one device reaches the others, and a device that has
//! been running for a while cannot roll back reads the account's other devices made since.

use std::time::Duration;

use crate::harness::{enroll_second_device, wipe, TestClient};
use serde_json::{json, Value};
use serial_test::serial;

/// Past the core's 5 s push debounce, so the debounced converge has run.
const AFTER_PUSH: Duration = Duration::from_secs(7);

async fn ingest_channel(c: &TestClient, channel: &str) -> Vec<Value> {
    c.activate();
    c.invoke_json(
        "ingest_channel_envelopes",
        json!({ "userId": c.user_id(), "channelId": channel }),
    )
    .await
    .as_array()
    .expect("ingest reports the news it stored")
    .clone()
}

async fn ingest_dm(c: &TestClient, dm: &str) -> Vec<Value> {
    c.activate();
    c.invoke_json(
        "ingest_dm_envelopes",
        json!({ "userId": c.user_id(), "dmChannelId": dm }),
    )
    .await
    .as_array()
    .expect("ingest reports the news it stored")
    .clone()
}

fn ids(news: &[Value]) -> Vec<String> {
    news.iter()
        .map(|m| m["id"].as_str().expect("message id").to_string())
        .collect()
}

async fn mark_read(c: &TestClient, conversation: &str) -> bool {
    c.activate();
    c.invoke_json(
        "mark_conversation_read",
        json!({ "conversationId": conversation, "userId": c.user_id() }),
    )
    .await
    .as_bool()
    .expect("mark_conversation_read answers a bool")
}

async fn refresh(c: &TestClient) -> Vec<String> {
    c.activate();
    c.invoke_json("refresh_read_cursors", json!({ "userId": c.user_id() }))
        .await
        .as_array()
        .expect("moved conversation ids")
        .iter()
        .map(|v| v.as_str().expect("conversation id").to_string())
        .collect()
}

async fn unread(c: &TestClient, conversation: &str) -> i64 {
    c.activate();
    c.invoke_json("get_unread_counts", json!({ "userId": c.user_id() }))
        .await
        .as_array()
        .expect("unread counts")
        .iter()
        .find(|u| u["conversation_id"] == conversation)
        .map(|u| u["count"].as_i64().expect("count"))
        .unwrap_or(0)
}

/// Alice on a desk and a phone, bob in a group with her (channels `x` and `y`).
async fn desk_phone_and_bob() -> (TestClient, TestClient, TestClient, String, String) {
    wipe().await;
    let mut desk = TestClient::new().await;
    desk.sign_up("alice@test.local").await;
    let phone = enroll_second_device(&desk, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group = desk.create_group("Read Sync").await;
    let x = desk.general_channel_id(&group).await;
    let y = desk.create_channel(&group, "other").await;
    desk.invite(&group, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite).await;
    bob.poll().await;
    desk.process_commits_for(&x).await;
    phone.poll().await;
    for c in [&desk, &phone, &bob] {
        ingest_channel(c, &x).await;
    }
    (desk, phone, bob, x, y)
}

/// Root cause 3: the push was last-writer-wins over this device's set alone, so a desk running
/// since before the phone's read overwrote it with its older position. Fails on the old push.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_stale_device_cannot_roll_back_a_read_made_on_another() {
    let (desk, phone, bob, x, y) = desk_phone_and_bob().await;

    bob.send_channel_message(&x, "first").await;
    ingest_channel(&desk, &x).await;
    ingest_channel(&phone, &x).await;
    assert!(mark_read(&desk, &x).await);
    tokio::time::sleep(AFTER_PUSH).await;

    // The phone reads further than the desk ever has, and pushes.
    bob.send_channel_message(&x, "second").await;
    ingest_channel(&phone, &x).await;
    assert!(mark_read(&phone, &x).await);
    tokio::time::sleep(AFTER_PUSH).await;

    // The desk, still at "first" in x, reads something else; its debounced push follows.
    bob.send_channel_message(&y, "elsewhere").await;
    ingest_channel(&desk, &y).await;
    assert!(mark_read(&desk, &y).await);
    tokio::time::sleep(AFTER_PUSH).await;

    // A device starting from the account's blob alone (a relaunch with no local cursor).
    {
        let guard = phone.state.local_db.lock().await;
        guard
            .as_ref()
            .expect("phone is unlocked")
            .conn()
            .execute("DELETE FROM read_cursor", [])
            .expect("clear the phone's local cursors");
    }
    let moved = refresh(&phone).await;
    assert!(moved.contains(&x) && moved.contains(&y), "the account blob carries both reads: {moved:?}");
    assert_eq!(
        unread(&phone, &x).await,
        0,
        "the desk's push rolled the phone's read of x back to an older position"
    );
    assert_eq!(unread(&phone, &y).await, 0);
    // And the desk itself learned the phone's read while pushing.
    assert_eq!(unread(&desk, &x).await, 0, "the desk merged before it pushed");
}

/// Root causes 1 and 2: a wake reports only what it newly stored from someone else and unread,
/// and a read made on the phone reaches a running desk on the next converge.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_wake_reports_only_news_and_reads_elsewhere_reach_a_running_device() {
    let (desk, phone, bob, x, _y) = desk_phone_and_bob().await;

    let m1 = bob.send_channel_message_id(&x, "news").await;
    assert_eq!(ids(&ingest_channel(&desk, &x).await), vec![m1.clone()], "a new message is news");
    assert!(ingest_channel(&desk, &x).await.is_empty(), "a replayed wake stored nothing new");

    // The desk's own message is never news, on the desk or on the phone.
    desk.send_channel_message(&x, "mine").await;
    assert!(ingest_channel(&desk, &x).await.is_empty());
    assert_eq!(ids(&ingest_channel(&phone, &x).await), vec![m1.clone()], "only bob's message");

    // The phone reads; the desk, running, converges (the DS nudge's path) and drops the badge.
    assert_eq!(unread(&desk, &x).await, 1);
    assert!(mark_read(&phone, &x).await);
    tokio::time::sleep(AFTER_PUSH).await;
    assert_eq!(refresh(&desk).await, vec![x.clone()], "the read made on the phone moved the desk");
    assert_eq!(unread(&desk, &x).await, 0);
    assert!(refresh(&desk).await.is_empty(), "converging again moves nothing");

    // The reported bug: bob writes, the phone reads it before the desk has even fetched it.
    // When the desk does fetch (a wake, or the launch catch-up), it is stored but is not news.
    let m2 = bob.send_channel_message_id(&x, "read on the phone first").await;
    assert_eq!(ids(&ingest_channel(&phone, &x).await), vec![m2]);
    assert!(mark_read(&phone, &x).await);
    tokio::time::sleep(AFTER_PUSH).await;
    assert_eq!(refresh(&desk).await, vec![x.clone()]);
    assert!(
        ingest_channel(&desk, &x).await.is_empty(),
        "a message already read on another device must not alert"
    );
    assert_eq!(unread(&desk, &x).await, 0);
}

/// The prod repro of #1628, exactly: a DM arrives while device A runs; the user reads it on
/// device B, whose read receipt (and delivered receipt) wake A with `new_message`. A must report
/// no news — no sound, banner, status-bar alert or unread — and the converge its nudge triggers
/// clears A's badge.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_read_receipt_from_the_other_device_is_not_news_and_the_nudge_clears_the_badge() {
    wipe().await;
    let mut desk = TestClient::new().await;
    let me = desk.sign_up("alice@test.local").await;
    let phone = enroll_second_device(&desk, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm = bob.create_dm(&[&bob_p.id, &me.id]).await;
    desk.poll().await;
    desk.accept_dm_request(&dm).await;
    phone.poll().await;
    for c in [&desk, &phone, &bob] {
        ingest_dm(c, &dm).await;
    }

    // 1-2: the app is running and the DM message arrives: news, once.
    let m = bob.send_channel_message_id(&dm, "lunch?").await;
    assert_eq!(ids(&ingest_dm(&desk, &dm).await), vec![m.clone()]);
    assert_eq!(unread(&desk, &dm).await, 1);

    // 3: read on the phone — the read receipt on screen, then the cursor.
    assert_eq!(ids(&ingest_dm(&phone, &dm).await), vec![m.clone()]);
    phone.activate();
    phone
        .invoke_json(
            "mark_messages_read",
            json!({ "conversationId": dm, "userId": me.id, "messageIds": [m] }),
        )
        .await;
    assert!(mark_read(&phone, &dm).await);

    // 4: the receipts wake the desk. Its ingest reports nothing, so nothing alerts or counts.
    assert!(
        ingest_dm(&desk, &dm).await.is_empty(),
        "the phone's read receipt was announced on the desk as a new message"
    );
    assert_eq!(unread(&desk, &dm).await, 1, "a receipt wake must not add unread");

    // The DS nudge after the phone's cursor push: the desk converges and its badge clears.
    tokio::time::sleep(AFTER_PUSH).await;
    assert_eq!(refresh(&desk).await, vec![dm.clone()]);
    assert_eq!(unread(&desk, &dm).await, 0);
    assert!(ingest_dm(&desk, &dm).await.is_empty(), "and stays quiet afterwards");
}

/// Two wakes for one message race (the message's own wake and a receipt's): whichever ingest ran
/// the catch-up and whichever joined it, the message is announced exactly once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn racing_wakes_announce_a_message_once_whoever_ran_the_pass() {
    let (desk, _phone, bob, x, _y) = desk_phone_and_bob().await;
    let m = bob.send_channel_message_id(&x, "one message, two wakes").await;
    let (a, b) = tokio::join!(ingest_channel(&desk, &x), ingest_channel(&desk, &x));
    let mut all = ids(&a);
    all.extend(ids(&b));
    assert_eq!(all, vec![m], "announced {} times", all.len());
}

/// Root cause 1, receipts: a delivered or read receipt rides the same `new_message` wake as a
/// message (the LiveKit server must not tell them apart); its ingest stores nothing to announce.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn receipt_wakes_report_nothing() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dm = alice.create_dm(&[&alice_p.id, &bob_p.id]).await;
    bob.poll().await;
    bob.accept_dm_request(&dm).await;
    alice.poll().await;
    ingest_dm(&bob, &dm).await;
    ingest_dm(&alice, &dm).await;

    let m = alice.send_channel_message_id(&dm, "hello bob").await;
    // Bob stores it (news) and emits a delivered receipt back to alice.
    assert_eq!(ids(&ingest_dm(&bob, &dm).await), vec![m.clone()]);
    assert!(ingest_dm(&alice, &dm).await.is_empty(), "a delivered receipt is not news");

    // Bob reads it on screen: a read receipt.
    bob.activate();
    bob.invoke_json(
        "mark_messages_read",
        json!({ "conversationId": dm, "userId": bob_p.id, "messageIds": [m] }),
    )
    .await;
    assert!(ingest_dm(&alice, &dm).await.is_empty(), "a read receipt is not news");
    assert_eq!(unread(&alice, &dm).await, 0, "alice's own message never badges her");
}
