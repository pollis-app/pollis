//! A device catching a group up in one pass must decrypt in the order senders
//! sealed, not channel by channel.
//!
//! Every channel of a group shares one MLS group, so a sender's ratchet
//! generations run across all of its channels, and openmls refuses a
//! generation more than 5 below the newest one it has opened for that sender
//! ("too distant in the past"). The catch-up used to decrypt each epoch's
//! envelopes conversation by conversation; a member reading along live opens
//! them in arrival order and loses nothing, but a device that was away (a phone
//! in the background) caught up channel by channel and lost every message more
//! than 5 generations behind the sender's last message in an earlier channel.

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

/// `want` all present on `c` in `channel`, and nothing shown as lost.
async fn assert_reads_all(c: &TestClient, who: &str, channel: &str, want: &[String]) {
    let msgs = c.fetch_channel_messages(channel).await;
    let got = contents(&msgs);
    let missing: Vec<&String> = want.iter().filter(|w| !got.contains(w)).collect();
    assert!(missing.is_empty(), "{who} lost {missing:?} (has {got:?})");
    assert_eq!(placeholders(&msgs), 0, "{who} shows a loss: {msgs:#?}");
}

/// The shape of the field report: a sender talks in one channel — messages and
/// edits, another member replying live — after also posting in a sibling
/// channel; a device away the whole time catches the group up at once (several
/// ingests racing, as a resume fires them). Interleaved both ways
/// round, so whichever channel the catch-up lists first, the old per-channel
/// order loses messages.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_device_catching_up_reads_a_sender_who_interleaved_channels() {
    wipe().await;

    let mut desk = TestClient::new().await;
    desk.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    // The device that is away for the whole conversation — a phone in the
    // background, say.
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

    // The away device catches up — several ingests at once, as a resume fires
    // them, on both channels.
    let p = &phone;
    tokio::join!(ingest(p, &x), ingest(p, &y), ingest(p, &x), ingest(p, &y));
    assert_reads_all(&phone, "the away device", &x, &want_x).await;
    assert_reads_all(&phone, "the away device", &y, &want_y).await;

    // So does the other member, who was not reading along either.
    ingest(&bob, &y).await;
    assert_reads_all(&bob, "bob", &x, &want_x).await;
    assert_reads_all(&bob, "bob", &y, &want_y).await;
}
