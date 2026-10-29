//! A device catching a group up in one pass must decrypt in the order senders
//! sealed, not channel by channel.
//! A group's channels share one MLS group, and openmls refuses generations >5 behind the newest.

use crate::harness::{wipe, TestClient};
use serde_json::json;
use serial_test::serial;

fn contents(msgs: &[serde_json::Value]) -> Vec<String> {
    msgs.iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

fn placeholders(msgs: &[serde_json::Value]) -> usize {
    msgs.iter()
        .filter(|m| m["undecryptable"].as_bool() == Some(true))
        .count()
}

async fn ingest(c: &TestClient, channel: &str) {
    c.invoke_json(
        "ingest_channel_envelopes",
        json!({ "userId": c.user_id(), "channelId": channel }),
    )
    .await;
}

async fn assert_reads_all(c: &TestClient, who: &str, channel: &str, want: &[String]) {
    let msgs = c.fetch_channel_messages(channel).await;
    let got = contents(&msgs);
    let missing: Vec<&String> = want.iter().filter(|w| !got.contains(w)).collect();
    assert!(missing.is_empty(), "{who} lost {missing:?} (has {got:?})");
    assert_eq!(placeholders(&msgs), 0, "{who} shows a loss: {msgs:#?}");
}

/// A device away the whole time catches up with racing ingests, channels interleaved both ways.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_catching_up_reads_a_sender_who_interleaved_channels() {
    wipe().await;

    let mut desk = TestClient::new().await;
    desk.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let mut phone = TestClient::new().await;
    let phone_p = phone.sign_up("carol@test.local").await;

    let group = desk.create_group("Interleave").await;
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
    desk.invite(&group, &phone_p.username).await;
    let invite = phone.first_pending_invite().await.expect("invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    phone.accept_invite(&invite).await;
    phone.poll().await;
    desk.process_commits_for(&x).await;
    bob.process_commits_for(&x).await;
    for c in [&desk, &phone, &bob] {
        ingest(c, &x).await;
    }

    let mut want_x: Vec<String> = Vec::new();
    let mut want_y: Vec<String> = Vec::new();
    let say = |list: &mut Vec<String>, s: &str| list.push(s.to_string());

    desk.send_channel_message(&x, "x before").await;
    say(&mut want_x, "x before");
    for i in 1..=6 {
        let t = format!("y{i}");
        desk.send_channel_message(&y, &t).await;
        say(&mut want_y, &t);
    }
    bob.send_channel_message(&x, "bob 1").await;
    say(&mut want_x, "bob 1");
    let first = desk.send_channel_message_id(&x, "x1").await;
    desk.edit_message(&x, &first, "x1 edited").await;
    say(&mut want_x, "x1 edited");
    for i in 2..=6 {
        let t = format!("x{i}");
        desk.send_channel_message(&x, &t).await;
        say(&mut want_x, &t);
        if i % 2 == 0 {
            let b = format!("bob {i}");
            bob.send_channel_message(&x, &b).await;
            say(&mut want_x, &b);
        }
    }
    desk.send_channel_message(&y, "y after").await;
    say(&mut want_y, "y after");

    let p = &phone;
    tokio::join!(ingest(p, &x), ingest(p, &y), ingest(p, &x), ingest(p, &y));
    assert_reads_all(&phone, "the away device", &x, &want_x).await;
    assert_reads_all(&phone, "the away device", &y, &want_y).await;

    ingest(&bob, &y).await;
    assert_reads_all(&bob, "bob", &x, &want_x).await;
    assert_reads_all(&bob, "bob", &y, &want_y).await;
}

/// #1524: the away device is the sender's own phone, and its catch-up crosses an epoch and fails twice.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_senders_own_phone_catches_up_through_a_faulty_resume() {
    use crate::harness::{arm_ds_fault, enroll_second_device, DsFault};
    use pollis_core::commands::messages::test_hooks::{self, Crash};

    wipe().await;
    let mut desk = TestClient::new().await;
    desk.sign_up("alice@test.local").await;
    let phone = enroll_second_device(&desk, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group = desk.create_group("Interleave").await;
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
        ingest(c, &x).await;
    }

    let mut want_x: Vec<String> = Vec::new();
    let mut want_y: Vec<String> = Vec::new();
    for i in 1..=7 {
        let t = format!("y{i}");
        desk.send_channel_message(&y, &t).await;
        want_y.push(t);
    }
    bob.send_channel_message(&x, "bob 1").await;
    want_x.push("bob 1".into());
    let first = desk.send_channel_message_id(&x, "x1").await;
    desk.edit_message(&x, &first, "x1 edited").await;
    want_x.push("x1 edited".into());
    assert!(bob.self_update(&group).await);
    for i in 2..=7 {
        let t = format!("x{i}");
        desk.send_channel_message(&x, &t).await;
        want_x.push(t);
        let t = format!("y after {i}");
        desk.send_channel_message(&y, &t).await;
        want_y.push(t);
    }

    arm_ds_fault(DsFault::CatchUpTimesOutMidway);
    ingest(&phone, &y).await;
    test_hooks::crash_next_send(Crash::AfterDecrypt);
    phone.activate();
    let (state, uid, ch) = (phone.state.clone(), phone.user_id().to_string(), x.clone());
    let died = tokio::spawn(async move {
        pollis_core::commands::messages::ingest_channel_envelopes_inner(&state, &uid, &ch).await
    })
    .await;
    assert!(matches!(&died, Err(e) if e.is_panic()), "the simulated kill unwound the catch-up: {died:?}");
    let p = &phone;
    tokio::join!(ingest(p, &x), ingest(p, &y), ingest(p, &x));
    assert_reads_all(&phone, "alice's phone", &x, &want_x).await;
    assert_reads_all(&phone, "alice's phone", &y, &want_y).await;
    ingest(&bob, &y).await;
    assert_reads_all(&bob, "bob", &x, &want_x).await;
    assert_reads_all(&bob, "bob", &y, &want_y).await;
}
