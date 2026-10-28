//! Zero message loss (#1524): every message sent to current members decrypts on
//! every current member device — through message-path faults, through a
//! process killed at the two points where a key is spent before its effect is
//! durable, and across a long multi-device, multi-channel run.
//!
//! `lost_messages.rs` proves a loss is *visible*. This file proves there is
//! none. Every test here also runs under the harness's zero-loss check
//! (`harness::assert_no_unexpected_loss`): any placeholder or `[loss]` line
//! fails it, whatever the assertions below say.

use std::collections::{BTreeMap, BTreeSet};

use crate::harness::{arm_ds_fault, clear_ds_fault, enroll_second_device, wipe, DsFault, TestClient};
use pollis_core::commands::messages::test_hooks::{self, Crash};
use rand::{rngs::StdRng, Rng, SeedableRng};
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

async fn join(admin: &TestClient, group: &str, channel: &str, member: &TestClient) {
    let username = member.profile.as_ref().expect("signed in").username.clone();
    admin.invite(group, &username).await;
    let invite = member.first_pending_invite().await.expect("pending invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    member.accept_invite(&invite).await;
    member.poll().await;
    admin.process_commits_for(channel).await;
}

/// Alice and Bob in one group, both caught up. Returns (alice, bob, group, channel).
async fn pair() -> (TestClient, TestClient, String, String) {
    wipe().await;
    let mut alice = TestClient::new().await;
    alice.sign_up("zl-alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("zl-bob@test.local").await;
    let group = alice.create_group("Zero loss").await;
    let channel = alice.general_channel_id(&group).await;
    join(&alice, &group, &channel, &bob).await;
    for c in [&alice, &bob] {
        c.ingest_channel(&channel).await;
    }
    (alice, bob, group, channel)
}

async fn new_id(c: &TestClient) -> String {
    c.invoke_json("new_message_id", json!({}))
        .await
        .as_str()
        .expect("id")
        .to_string()
}

async fn send_with_id(
    c: &TestClient,
    conversation: &str,
    id: &str,
    content: &str,
) -> Result<serde_json::Value, String> {
    c.invoke_try(
        "send_message",
        json!({
            "conversationId": conversation,
            "senderId": c.user_id(),
            "content": content,
            "replyToId": null,
            "threadId": null,
            "senderUsername": null,
            "messageId": id,
        }),
    )
    .await
}

/// Drain `c`'s outbox until nothing is left unsent, as successive reconnects
/// would. Panics if a message is still unsent after a few rounds — that is a
/// message the sender saw accepted and nobody received.
async fn drain(c: &TestClient) {
    for _ in 0..4 {
        let report = c.flush_outbox().await;
        if report["failed"].as_array().is_none_or(|f| f.is_empty()) {
            return;
        }
    }
    panic!("the outbox never emptied");
}

/// Every one of `want` present exactly once on `c` in `channel`, nothing shown
/// as lost.
async fn assert_reads_all(c: &TestClient, who: &str, channel: &str, want: &[&str]) {
    let msgs = c.fetch_channel_messages(channel).await;
    let got = contents(&msgs);
    for w in want {
        let n = got.iter().filter(|g| g == w).count();
        assert_eq!(n, 1, "{who}: {w:?} present {n} times (has {got:?})");
    }
    assert_eq!(placeholders(&msgs), 0, "{who} shows a loss: {msgs:#?}");
}

/// The check has teeth: a loss the test did not ask for fails it, even though
/// no assertion in the test body looks.
#[tokio::test(flavor = "multi_thread")]
#[serial]
#[should_panic(expected = "zero-loss invariant")]
async fn an_unexpected_loss_fails_the_test() {
    let (alice, bob, _group, channel) = pair().await;
    test_hooks::corrupt_next_send();
    alice.send_channel_message(&channel, "mangled in flight").await;
    bob.ingest_channel(&channel).await;
    drop((alice, bob));
}

// ── Message-path faults ─────────────────────────────────────────────────────

/// The send lands and the sender never hears so. The message is marked failed,
/// the next drain re-posts it as a replay, and everyone holds it exactly once.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_send_that_lands_then_times_out_reaches_everyone_once() {
    let (alice, bob, _group, channel) = pair().await;
    let id = new_id(&alice).await;
    arm_ds_fault(DsFault::SendLandsThenTimesOut);
    send_with_id(&alice, &channel, &id, "landed, answer lost")
        .await
        .expect_err("the sender sees a timeout");
    assert!(!crate::harness::ds_fault_armed(), "the fault fired");

    // The recipient can read it before the sender has done anything about it.
    assert_reads_all(&bob, "bob", &channel, &["landed, answer lost"]).await;

    drain(&alice).await;
    assert_reads_all(&alice, "alice", &channel, &["landed, answer lost"]).await;
    assert_reads_all(&bob, "bob", &channel, &["landed, answer lost"]).await;
}

/// The send times out before the DS stores anything. Nobody has it until the
/// drain delivers it; then everyone does.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_send_that_times_out_unlanded_is_delivered_by_the_drain() {
    let (alice, bob, _group, channel) = pair().await;
    let id = new_id(&alice).await;
    arm_ds_fault(DsFault::SendTimesOutUnlanded);
    send_with_id(&alice, &channel, &id, "never landed")
        .await
        .expect_err("the sender sees a timeout");
    assert!(!contents(&bob.fetch_channel_messages(&channel).await).contains(&"never landed".into()));

    // A commit lands meanwhile, so the retry has to seal at a newer epoch.
    assert!(bob.self_update(&_group).await);
    drain(&alice).await;
    assert_reads_all(&bob, "bob", &channel, &["never landed"]).await;
    assert_reads_all(&alice, "alice", &channel, &["never landed"]).await;
}

/// An edit whose answer is lost, retried by the user: the final text is what
/// everyone sees.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_edit_that_lands_then_times_out_converges() {
    let (alice, bob, _group, channel) = pair().await;
    let id = alice.send_channel_message_id(&channel, "draft").await;
    bob.ingest_channel(&channel).await;
    arm_ds_fault(DsFault::SendLandsThenTimesOut);
    let first = alice
        .invoke_try(
            "edit_message",
            json!({
                "conversationId": channel,
                "messageId": id,
                "userId": alice.user_id(),
                "newContent": "final",
            }),
        )
        .await;
    assert!(first.is_err(), "the editor sees a timeout");
    alice.edit_message(&channel, &id, "final").await;
    assert_reads_all(&bob, "bob", &channel, &["final"]).await;
    assert_reads_all(&alice, "alice", &channel, &["final"]).await;
}

/// A device away across several epochs catches up, and the commit fetch times
/// out part-way. What it decrypted stays; the rest arrives on the next pass.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_catch_up_that_times_out_midway_loses_nothing() {
    let (alice, bob, group, channel) = pair().await;
    let mut want: Vec<String> = Vec::new();
    for round in 0..3 {
        for i in 0..3 {
            let t = format!("epoch {round} msg {i}");
            alice.send_channel_message(&channel, &t).await;
            want.push(t);
        }
        // A commit bob has not seen: the next round is sealed one epoch on.
        assert!(alice.self_update(&group).await);
    }
    arm_ds_fault(DsFault::CatchUpTimesOutMidway);
    bob.ingest_channel(&channel).await;
    assert!(!crate::harness::ds_fault_armed(), "the fault fired on bob's catch-up");
    bob.ingest_channel(&channel).await;
    let want: Vec<&str> = want.iter().map(String::as_str).collect();
    assert_reads_all(&bob, "bob", &channel, &want).await;
}

/// The cursor report after a catch-up is lost, so the next catch-up hands the
/// device every envelope again — messages, edits and (in a DM) receipts it
/// already opened. None of them may come back as a loss.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_lost_cursor_report_redelivers_without_a_loss() {
    wipe().await;
    let mut alice = TestClient::new().await;
    let alice_p = alice.sign_up("zl-alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("zl-bob@test.local").await;
    let dm = alice.create_dm(&[alice_p.id.as_str(), bob_p.id.as_str()]).await;
    bob.accept_dm_request(&dm).await;
    for c in [&alice, &bob] {
        c.fetch_dm_messages(&dm).await;
    }
    let first = alice.send_channel_message_id(&dm, "one").await;
    bob.send_channel_message(&dm, "two").await;
    alice.edit_message(&dm, &first, "one, edited").await;

    for c in [&bob, &alice, &bob, &alice] {
        arm_ds_fault(DsFault::WatermarkTimesOut);
        c.fetch_dm_messages(&dm).await;
    }
    clear_ds_fault();
    for c in [&alice, &bob] {
        let msgs = c.fetch_dm_messages(&dm).await;
        let got = contents(&msgs);
        assert!(got.contains(&"one, edited".into()) && got.contains(&"two".into()), "{got:?}");
        assert_eq!(placeholders(&msgs), 0, "{msgs:#?}");
    }
}

// ── A process killed where a key is spent ───────────────────────────────────

/// The receiving device dies after `process_message` opened the envelope —
/// its ratchet key spent and persisted — and before the row was written. On
/// restart the same envelope is fetched again and must still open.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_receiver_killed_between_decrypt_and_insert_still_reads_the_message() {
    let (alice, bob, _group, channel) = pair().await;
    alice.send_channel_message(&channel, "before the kill").await;
    alice.send_channel_message(&channel, "also before").await;

    test_hooks::crash_next_send(Crash::AfterDecrypt);
    bob.activate();
    let state = bob.state.clone();
    let uid = bob.user_id().to_string();
    let ch = channel.clone();
    let died = tokio::spawn(async move {
        pollis_core::commands::messages::ingest_channel_envelopes_inner(&state, &uid, &ch).await
    })
    .await;
    assert!(matches!(&died, Err(e) if e.is_panic()), "the simulated kill unwound the ingest: {died:?}");

    alice.send_channel_message(&channel, "after the restart").await;
    assert_reads_all(
        &bob,
        "bob",
        &channel,
        &["before the kill", "also before", "after the restart"],
    )
    .await;
}

/// The sending device dies after sealing — its own ratchet already past that
/// generation — and before the post. The outbox drain after restart must
/// deliver it, and it must open everywhere.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sender_killed_between_seal_and_post_still_delivers() {
    let (alice, bob, _group, channel) = pair().await;
    alice.send_channel_message(&channel, "first").await;
    let id = new_id(&alice).await;
    test_hooks::crash_next_send(Crash::AfterSeal);
    let err = send_with_id(&alice, &channel, &id, "sealed, then the kill")
        .await
        .expect_err("the simulated kill ends the send");
    assert!(err.contains("simulated process death"), "{err}");

    let uid = alice.user_id().to_string();
    alice.invoke_json("lock", json!({})).await;
    alice
        .invoke_json("unlock", json!({ "userId": uid, "pin": crate::harness::TEST_PIN }))
        .await;
    drain(&alice).await;
    alice.send_channel_message(&channel, "last").await;

    for (c, who) in [(&bob, "bob"), (&alice, "alice")] {
        assert_reads_all(c, who, &channel, &["first", "sealed, then the kill", "last"]).await;
    }
}

// ── The soak ────────────────────────────────────────────────────────────────

/// One message the soak sent: where, by which device, and its text now.
struct Sent {
    channel: usize,
    by: usize,
    text: String,
    /// The DS has it (the send returned, or a drain since delivered it).
    landed: bool,
}

const EMOJI: [&str; 3] = ["👍", "🎉", "👀"];

/// Two users, three devices (alice on a desktop and a phone), three channels
/// in one group, a few hundred interleaved sends, edits, reactions,
/// self-updates and catch-ups, with the message-path faults switched on at
/// random. The phone is away for long stretches and catches up in large
/// batches. Pass = zero losses on every device, every message present once
/// with its final text, every reaction where it was put.
///
/// `POLLIS_ZERO_LOSS_SOAK_OPS` lengthens it (default 240) and
/// `POLLIS_ZERO_LOSS_SOAK_SEED` replays a run; the seed is printed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn zero_loss_soak_multi_device_multi_channel() {
    let ops: usize = std::env::var("POLLIS_ZERO_LOSS_SOAK_OPS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(240);
    let seed: u64 = std::env::var("POLLIS_ZERO_LOSS_SOAK_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| rand::thread_rng().gen());
    eprintln!("[soak] {ops} ops, seed {seed} (POLLIS_ZERO_LOSS_SOAK_SEED={seed} replays it)");
    let mut rng = StdRng::seed_from_u64(seed);

    wipe().await;
    let mut desk = TestClient::new().await;
    desk.sign_up("zl-alice@test.local").await;
    let phone = enroll_second_device(&desk, "zl-alice@test.local").await;
    let mut bob = TestClient::new().await;
    bob.sign_up("zl-bob@test.local").await;

    let group = desk.create_group("Soak").await;
    let general = desk.general_channel_id(&group).await;
    let channels = [
        general.clone(),
        desk.create_channel(&group, "two").await,
        desk.create_channel(&group, "three").await,
    ];
    join(&desk, &group, &general, &bob).await;
    phone.poll().await;
    let devices: [&TestClient; 3] = [&desk, &phone, &bob];
    for d in devices {
        for c in &channels {
            d.ingest_channel(c).await;
        }
    }

    let mut sent: BTreeMap<String, Sent> = BTreeMap::new();
    let mut reactions: BTreeSet<(String, String, &str)> = BTreeSet::new();
    let mut faults = 0usize;
    let mut kills = 0usize;
    for step in 0..ops {
        // The phone acts and reads far less often: away, then a big catch-up.
        let d = match rng.gen_range(0..10) {
            0 => 1,
            1..=5 => 0,
            _ => 2,
        };
        let dev = devices[d];
        if rng.gen_bool(0.2) {
            // Re-arming over one that never applied would hide it: count only
            // those that fired.
            if !crate::harness::ds_fault_armed() {
                let f = DsFault::MESSAGE_PATH[rng.gen_range(0..DsFault::MESSAGE_PATH.len())];
                arm_ds_fault(f);
                faults += 1;
            }
        } else if rng.gen_bool(0.03) {
            // A sender killed with its envelope sealed and unposted.
            test_hooks::crash_next_send(Crash::AfterSeal);
            kills += 1;
        }
        let ch = rng.gen_range(0..channels.len());
        match rng.gen_range(0..100) {
            0..=49 => {
                let id = new_id(dev).await;
                let text = format!("s{step} d{d} c{ch}");
                let landed = send_with_id(dev, &channels[ch], &id, &text).await.is_ok();
                sent.insert(id, Sent { channel: ch, by: d, text, landed });
            }
            50..=61 => {
                let mine: Vec<String> = sent
                    .iter()
                    .filter(|(_, s)| s.by == d && s.landed)
                    .map(|(id, _)| id.clone())
                    .collect();
                if mine.is_empty() {
                    continue;
                }
                let id = mine[rng.gen_range(0..mine.len())].clone();
                let text = format!("s{step} edit of {id}");
                let s = sent.get_mut(&id).expect("sent");
                let args = json!({
                    "conversationId": channels[s.channel],
                    "messageId": id,
                    "userId": dev.user_id(),
                    "newContent": text,
                });
                // A failed edit is retried, as its author would.
                let mut ok = false;
                for _ in 0..3 {
                    if dev.invoke_try("edit_message", args.clone()).await.is_ok() {
                        ok = true;
                        break;
                    }
                }
                assert!(ok, "an edit failed three times running");
                s.text = text;
            }
            62..=71 => {
                let landed: Vec<String> = sent
                    .iter()
                    .filter(|(_, s)| s.landed)
                    .map(|(id, _)| id.clone())
                    .collect();
                if landed.is_empty() {
                    continue;
                }
                let id = landed[rng.gen_range(0..landed.len())].clone();
                let emoji = EMOJI[rng.gen_range(0..EMOJI.len())];
                dev.invoke_json(
                    "add_reaction",
                    json!({ "messageId": id, "userId": dev.user_id(), "emoji": emoji }),
                )
                .await;
                reactions.insert((id, dev.user_id().to_string(), emoji));
            }
            72..=75 => {
                // A commit: rotation is what moves the epoch under in-flight
                // sends and away devices.
                let _ = dev.try_self_update(&group).await;
            }
            76..=81 => {
                let report = dev.flush_outbox().await;
                mark_delivered(&mut sent, &report);
            }
            _ => {
                if d == 1 {
                    // The phone comes back and catches every channel at once,
                    // as a resume fires them.
                    tokio::join!(
                        dev.ingest_channel(&channels[0]),
                        dev.ingest_channel(&channels[1]),
                        dev.ingest_channel(&channels[2]),
                    );
                } else {
                    dev.ingest_channel(&channels[ch]).await;
                }
            }
        }
    }
    clear_ds_fault();
    test_hooks::disarm_crash();

    // Everyone reconnects: drains, then reads every channel.
    for d in devices {
        drain(d).await;
    }
    for s in sent.values_mut() {
        s.landed = true;
    }
    for _ in 0..2 {
        for d in devices {
            for c in &channels {
                d.ingest_channel(c).await;
            }
        }
    }

    let names = ["alice's desktop", "alice's phone", "bob"];
    for (d, dev) in devices.iter().enumerate() {
        for (ci, c) in channels.iter().enumerate() {
            let msgs = dev.fetch_channel_messages(c).await;
            assert_eq!(placeholders(&msgs), 0, "{} shows a loss in channel {ci}", names[d]);
            let by_id: BTreeMap<&str, &serde_json::Value> = msgs
                .iter()
                .filter_map(|m| m["id"].as_str().map(|id| (id, m)))
                .collect();
            for (id, s) in sent.iter().filter(|(_, s)| s.channel == ci) {
                let m = by_id.get(id.as_str()).unwrap_or_else(|| {
                    panic!("{} never received {id} ({:?}) in channel {ci}", names[d], s.text)
                });
                assert_eq!(
                    m["content"].as_str(),
                    Some(s.text.as_str()),
                    "{} holds a stale text for {id}",
                    names[d]
                );
            }
            let expected = sent.values().filter(|s| s.channel == ci).count();
            assert_eq!(by_id.len(), expected, "{} holds a message nobody sent", names[d]);
        }
        let mut want: BTreeMap<&str, BTreeMap<&str, BTreeSet<&str>>> = BTreeMap::new();
        for (id, user, emoji) in &reactions {
            want.entry(id.as_str())
                .or_default()
                .entry(*emoji)
                .or_default()
                .insert(user.as_str());
        }
        for (id, per_emoji) in &want {
            let got = dev
                .invoke_json("get_reactions", json!({ "messageId": id }))
                .await;
            for (emoji, users) in per_emoji {
                let r = got
                    .as_array()
                    .and_then(|a| a.iter().find(|r| r["emoji"] == *emoji))
                    .unwrap_or_else(|| panic!("{} lacks {emoji} on {id}: {got}", names[d]));
                let have: BTreeSet<&str> = r["user_ids"]
                    .as_array()
                    .expect("user_ids")
                    .iter()
                    .filter_map(|u| u.as_str())
                    .collect();
                assert_eq!(&have, users, "{} reactions on {id}", names[d]);
            }
        }
    }
    eprintln!(
        "[soak] {} messages, {} reactions, {faults} DS faults, {kills} sender kills armed — \
         zero loss on 3 devices",
        sent.len(),
        reactions.len()
    );
}

fn mark_delivered(sent: &mut BTreeMap<String, Sent>, report: &serde_json::Value) {
    for id in report["delivered"].as_array().into_iter().flatten() {
        if let Some(s) = id.as_str().and_then(|id| sent.get_mut(id)) {
            s.landed = true;
        }
    }
}