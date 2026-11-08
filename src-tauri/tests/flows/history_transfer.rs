//! History transfer between two of one account's devices (#1735),
//! `docs/history-transfer-design.md`. Every scenario ends by comparing the
//! receiver's rows to the source's field by field, and checking nothing is
//! duplicated; the harness's `Drop` then runs the zero-loss check.

use std::sync::Arc;
use std::time::Duration;

use crate::harness::{enroll_another_device, wipe, writable_remote, TestClient, TEST_PIN};
use pollis_core::commands::history_transfer::{self as ht, HistoryTransfer};
use pollis_core::commands::messages::test_hooks::{self as hooks, Crash};
use serde_json::json;
use serial_test::serial;

/// Short idle limit so a stalled run pauses (and the test sees it) quickly.
fn fast_idle() {
    ht::transfer_hooks::set_idle_limit(Duration::from_secs(4));
    ht::transfer_hooks::set_auto_resume(false);
    ht::transfer_hooks::set_rows_per_chunk(0);
}

/// One run, on its own task: a kill point (`test_hooks::die`) is a real unwind
/// that aborts the run wherever it stands, open transactions included, and
/// comes back as `Err("killed: …")`.
async fn run(c: &TestClient, id: &str) -> Result<HistoryTransfer, String> {
    c.activate();
    let state = c.state.clone();
    let id = id.to_string();
    let task = tokio::spawn(async move {
        ht::run_history_transfer(&state, id, Arc::new(pollis_core::sink::NoopSink)).await
    });
    match task.await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(e) if e.is_panic() => Err(format!("killed: {e}")),
        Err(e) => Err(format!("run task failed: {e}")),
    }
}

/// Run both sides together until both stop.
async fn run_both(src: &TestClient, dst: &TestClient, id: &str) -> (Result<HistoryTransfer, String>, Result<HistoryTransfer, String>) {
    tokio::join!(run(src, id), run(dst, id))
}

async fn request(dst: &TestClient, src: &TestClient, via: &str) -> Result<HistoryTransfer, String> {
    let src_device = device_id(src).await;
    dst.invoke_try("request_history_transfer", json!({ "sourceDeviceId": src_device, "via": via }))
        .await
        .map(|v| serde_json::from_value(v).expect("HistoryTransfer"))
}

async fn pending(src: &TestClient) -> Vec<serde_json::Value> {
    src.invoke_json("list_pending_history_transfer_requests", json!({}))
        .await
        .as_array()
        .expect("pending list")
        .clone()
}

async fn approve(src: &TestClient, id: &str, pin: Option<&str>) -> Result<HistoryTransfer, String> {
    src.invoke_try("approve_history_transfer", json!({ "transferId": id, "pin": pin }))
        .await
        .map(|v| serde_json::from_value(v).expect("HistoryTransfer"))
}

async fn device_id(c: &TestClient) -> String {
    c.state.device_id.lock().await.clone().expect("device id")
}

/// Request from `dst`, approve on `src` with the PIN, run both to the end.
async fn copy(src: &TestClient, dst: &TestClient) -> (HistoryTransfer, HistoryTransfer) {
    let t = request(dst, src, "sibling").await.expect("request");
    approve(src, &t.transfer_id, Some(TEST_PIN)).await.expect("approve");
    let (s, d) = run_both(src, dst, &t.transfer_id).await;
    (s.expect("source run"), d.expect("receiver run"))
}

/// `(id, conversation, sender, content, reply_to, thread, sent_at, edited?, deleted?)`.
/// "Edited" is compared only on live rows: a deleted message shows no text,
/// and whether an edit reached it before its delete differs by device.
type RowView = (String, String, String, Option<String>, Option<String>, Option<String>, String, bool, bool);

async fn rows(c: &TestClient, conversations: &[&str]) -> Vec<RowView> {
    let guard = c.state.local_db.lock().await;
    let conn = guard.as_ref().expect("unlocked").conn();
    let list = serde_json::to_string(conversations).unwrap();
    let mut stmt = conn
        .prepare(
            "SELECT id, conversation_id, sender_id, content, reply_to_id, thread_id, sent_at,
                    edited_at IS NOT NULL AND deleted_at IS NULL, deleted_at IS NOT NULL
             FROM message WHERE undecryptable IS NULL AND send_state IS NULL
               AND conversation_id IN (SELECT value FROM json_each(?1))
             ORDER BY id",
        )
        .unwrap();
    stmt.query_map([list], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?))
    })
    .unwrap()
    .collect::<rusqlite::Result<_>>()
    .unwrap()
}

async fn scalar(c: &TestClient, sql: &str) -> i64 {
    let guard = c.state.local_db.lock().await;
    guard.as_ref().expect("unlocked").conn().query_row(sql, [], |r| r.get(0)).unwrap()
}

/// `message.id` is the primary key, so "no duplicate" is checked where it can
/// fail: per conversation, the receiver holds exactly as many rows as the
/// source (no extra copy under another id, none missing), and every id is the
/// source's.
async fn assert_same_rows_per_conversation(dst: &TestClient, src: &TestClient, conversations: &[&str]) {
    for conv in conversations {
        let (d, s) = (rows(dst, &[conv]).await, rows(src, &[conv]).await);
        assert_eq!(d.len(), s.len(), "row count differs in {conv}");
        let ids = |v: &[RowView]| v.iter().map(|r| r.0.clone()).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids(&d), ids(&s), "row ids differ in {conv}");
    }
}

async fn exec(c: &TestClient, sql: &str) -> usize {
    let guard = c.state.local_db.lock().await;
    guard.as_ref().expect("unlocked").conn().execute(sql, []).unwrap()
}

async fn ingest(c: &TestClient, channel: &str) {
    c.ingest_channel(channel).await;
}

async fn lock_unlock(c: &TestClient) {
    c.invoke_json("lock", json!({})).await;
    c.invoke_json("unlock", json!({ "userId": c.user_id(), "pin": TEST_PIN })).await;
}

/// Alice on a desk with history in a channel (with bob) and a DM with bob,
/// including an edit and a delete. Returns `(desk, bob, channel, dm, ids)`.
async fn desk_with_history() -> (TestClient, TestClient, String, String, Vec<String>) {
    wipe().await;
    fast_idle();
    let mut desk = TestClient::new().await;
    desk.sign_up("alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let group = desk.create_group("History").await;
    let channel = desk.general_channel_id(&group).await;
    desk.invite(&group, &bob_p.username).await;
    let invite = bob.first_pending_invite().await.expect("invite")["id"].as_str().unwrap().to_string();
    bob.accept_invite(&invite).await;
    bob.poll().await;
    desk.process_commits_for(&channel).await;

    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(desk.send_channel_message_id(&channel, &format!("desk {i}")).await);
        ids.push(bob.send_channel_message_id(&channel, &format!("bob {i}")).await);
    }
    bob.edit_message(&channel, &ids[1], "bob 0, edited").await;
    desk.delete_message(&ids[2]).await;
    ingest(&desk, &channel).await;
    ingest(&bob, &channel).await;

    let dm = desk.create_dm(&[bob.user_id()]).await;
    bob.accept_dm_request(&dm).await;
    desk.send_channel_message(&dm, "dm one").await;
    bob.send_channel_message(&dm, "dm two").await;
    desk.fetch_dm_messages(&dm).await;
    bob.fetch_dm_messages(&dm).await;
    ingest(&desk, &channel).await;
    (desk, bob, channel, dm, ids)
}

/// Bring a sibling enrolled with the typed code into every conversation.
async fn new_sibling(desk: &TestClient, channel: &str, dm: &str) -> TestClient {
    let devices = desk.invoke_json("list_user_devices", json!({ "userId": desk.user_id() })).await;
    let n = devices.as_array().map(|a| a.len()).unwrap_or(1);
    let phone = enroll_another_device(desk, "alice@test.local", n + 1).await;
    phone.poll().await;
    ingest(&phone, channel).await;
    phone.fetch_dm_messages(dm).await;
    phone
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_sibling_copies_the_sources_history_exactly() {
    let (desk, bob, channel, dm, ids) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    // A brand-new device starts empty (loss 2).
    assert!(rows(&phone, &[&channel, &dm]).await.is_empty());

    let (s, d) = copy(&desk, &phone).await;
    assert_eq!((s.state.as_str(), d.state.as_str()), ("completed", "completed"), "{s:?} {d:?}");
    assert_eq!(d.rows_skipped, 0, "{d:?}");
    assert_eq!(d.deferred_conversations, 0, "{d:?}");
    assert_eq!(d.rows_done as usize, rows(&desk, &[&channel, &dm]).await.len(), "{d:?}");
    assert_eq!(d.estimated_total, s.estimated_total);
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
    assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
    // The edit and the delete came across as they are.
    let r = rows(&phone, &[&channel]).await;
    let edited = r.iter().find(|x| x.0 == ids[1]).unwrap();
    assert_eq!(edited.3.as_deref(), Some("bob 0, edited"));
    assert!(edited.7);
    let deleted = r.iter().find(|x| x.0 == ids[2]).unwrap();
    assert!(deleted.3.is_none() && deleted.8);
    // Every copied row is marked, and no key material is left behind.
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM message WHERE origin = 'transfer'").await, d.rows_done as i64);
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM history_transfer_local WHERE transfer_key IS NOT NULL OR kem_sk IS NOT NULL").await, 0);
    assert_eq!(scalar(&desk, "SELECT COUNT(*) FROM history_transfer_local WHERE transfer_key IS NOT NULL").await, 0);
    assert_eq!(scalar(&desk, "SELECT COUNT(*) FROM history_transfer_outgoing").await, 0);
    // Security events on both sides.
    let events = desk.invoke_json("list_security_events", json!({ "userId": desk.user_id(), "limit": 20 })).await;
    let kinds: Vec<&str> = events["events"].as_array().unwrap().iter().filter_map(|e| e["kind"].as_str()).collect();
    assert!(kinds.contains(&"history_transfer_approved") && kinds.contains(&"history_transfer_completed"), "{kinds:?}");

    // Live messaging keeps working both ways after the import.
    let live = bob.send_channel_message_id(&channel, "after the copy").await;
    ingest(&phone, &channel).await;
    let mine = phone.send_channel_message_id(&channel, "from the phone").await;
    ingest(&bob, &channel).await;
    ingest(&desk, &channel).await;
    assert!(rows(&phone, &[&channel]).await.iter().any(|x| x.0 == live));
    assert!(rows(&bob, &[&channel]).await.iter().any(|x| x.0 == mine));
    assert!(rows(&desk, &[&channel]).await.iter().any(|x| x.0 == mine));
    // An edit of a copied message arriving later applies to the copied row.
    bob.edit_message(&channel, &ids[3], "edited after the copy").await;
    ingest(&phone, &channel).await;
    let r = rows(&phone, &[&channel]).await;
    assert_eq!(r.iter().find(|x| x.0 == ids[3]).unwrap().3.as_deref(), Some("edited after the copy"));

    // DM receipts came across (receipts are on by default, and it is a DM).
    let on_desk = receipts(&desk, &dm).await;
    assert!(!on_desk.is_empty(), "the DM has delivery receipts to copy");
    assert_eq!(receipts(&phone, &dm).await, on_desk);

    // Copying again is safe: nothing changes, nothing duplicates.
    let before = rows(&phone, &[&channel, &dm]).await;
    let (_, d2) = copy(&desk, &phone).await;
    assert_eq!(d2.state, "completed");
    assert_eq!(rows(&phone, &[&channel, &dm]).await, before);
    assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
    drop((desk, bob, phone));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn approval_needs_the_pin_and_a_wrong_pin_counts() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let t = request(&phone, &desk, "sibling").await.expect("request");
    let p = pending(&desk).await;
    assert_eq!(p.len(), 1, "{p:?}");
    assert_eq!(p[0]["requester_device_id"], device_id(&phone).await);
    assert_eq!(p[0]["pin_required"], true);
    assert!(p[0]["requester_added_at"].as_str().is_some());
    // No PIN: refused. Wrong PIN: the PIN screens' own error, and it counts.
    assert!(approve(&desk, &t.transfer_id, None).await.unwrap_err().contains("PIN"));
    let wrong = approve(&desk, &t.transfer_id, Some("9999")).await.unwrap_err();
    assert!(wrong.contains("pin incorrect"), "{wrong}");
    // The receiver cannot approve its own request.
    assert!(approve(&phone, &t.transfer_id, Some(TEST_PIN)).await.is_err());
    // Nothing moves without approval: the receiver's run pauses, empty.
    let r = run(&phone, &t.transfer_id).await.expect("run");
    assert_eq!(r.state, "awaiting_approval");
    assert!(rows(&phone, &[&channel, &dm]).await.is_empty());
    // A second request while one is open is refused.
    assert!(request(&phone, &desk, "sibling").await.is_err());
    // Decline: the receiver learns it; the request is never shown again.
    desk.invoke_json("decline_history_transfer", json!({ "transferId": t.transfer_id })).await;
    assert!(pending(&desk).await.is_empty());
    let r = run(&phone, &t.transfer_id).await.expect("run");
    assert_eq!(r.state, "declined");
    assert!(rows(&phone, &[&channel, &dm]).await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_qr_linked_device_copies_with_one_tap_and_a_typed_code_link_does_not() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    // Typed-code enrollment: no shortcut.
    let typed = new_sibling(&desk, &channel, &dm).await;
    let t = request(&typed, &desk, "link").await.expect("request");
    assert_eq!(pending(&desk).await[0]["pin_required"], true);
    desk.invoke_json("decline_history_transfer", json!({ "transferId": t.transfer_id })).await;

    // QR link v1: approved with the PIN, so the follow-on copy is one tap.
    let phone = crate::device_link::link_by_qr(&desk, "alice@test.local").await;
    phone.poll().await;
    ingest(&phone, &channel).await;
    let sources = phone.invoke_json("list_history_transfer_sources", json!({})).await;
    let first = &sources.as_array().unwrap()[0];
    assert_eq!(first["device_id"], device_id(&desk).await, "{sources}");
    assert_eq!(first["approved_this_device"], true, "{sources}");
    let t = request(&phone, &desk, "link").await.expect("request");
    let p = pending(&desk).await;
    assert_eq!(p[0]["pin_required"], false, "{p:?}");
    approve(&desk, &t.transfer_id, None).await.expect("one-tap approve");
    let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
    assert_eq!(d.expect("dst").state, "completed");
    assert_eq!(s.expect("src").state, "completed");
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);

    // A lock ends the shortcut.
    let phone2 = crate::device_link::link_by_qr(&desk, "alice@test.local").await;
    lock_unlock(&desk).await;
    let t = request(&phone2, &desk, "link").await.expect("request");
    assert_eq!(pending(&desk).await[0]["pin_required"], true);
    assert!(approve(&desk, &t.transfer_id, None).await.is_err());
    drop((typed, phone, phone2));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_v2_scanned_device_copies_history() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let laptop = crate::device_link::link_by_offer(&desk).await;
    laptop.poll().await;
    let t = request(&laptop, &desk, "link").await.expect("request");
    assert_eq!(pending(&desk).await[0]["pin_required"], false);
    approve(&desk, &t.transfer_id, None).await.expect("approve");
    let (_, d) = run_both(&desk, &laptop, &t.transfer_id).await;
    assert_eq!(d.expect("dst").state, "completed");
    assert_eq!(rows(&laptop, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
}

/// Backfill: the receiver has its own rows since it joined (one of them a
/// placeholder). The copy heals the placeholder and never touches a real row.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn backfill_heals_placeholders_and_never_overwrites_a_real_row() {
    crate::harness::expect_losses();
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let a = bob.send_channel_message_id(&channel, "post-join a").await;
    let b = bob.send_channel_message_id(&channel, "post-join b").await;
    ingest(&phone, &channel).await;
    ingest(&desk, &channel).await;
    // `a` became a placeholder here (simulated loss); `b` was altered locally.
    exec(&phone, &format!("UPDATE message SET content = NULL, sender_id = 'sealed', undecryptable = 'failed' WHERE id = '{a}'")).await;
    exec(&phone, &format!("UPDATE message SET content = 'phone copy' WHERE id = '{b}'")).await;
    // The author edits the lost one while it is a placeholder here: recorded, not lost.
    bob.edit_message(&channel, &a, "post-join a, edited").await;
    ingest(&phone, &channel).await;
    ingest(&desk, &channel).await;
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM pending_message_change").await, 1);

    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed");
    let r = rows(&phone, &[&channel]).await;
    let healed = r.iter().find(|x| x.0 == a).expect("healed");
    assert_eq!(healed.3.as_deref(), Some("post-join a, edited"));
    assert_eq!(healed.2, bob.user_id());
    assert_eq!(scalar(&phone, &format!("SELECT COUNT(*) FROM message WHERE id = '{a}' AND undecryptable IS NULL AND origin = 'transfer'")).await, 1);
    assert_eq!(r.iter().find(|x| x.0 == b).unwrap().3.as_deref(), Some("phone copy"), "a real row is never overwritten");
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM pending_message_change").await, 0);
    assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
    // Everything else equals the source.
    let mut mine = rows(&phone, &[&channel, &dm]).await;
    let mut theirs = rows(&desk, &[&channel, &dm]).await;
    mine.retain(|x| x.0 != b);
    theirs.retain(|x| x.0 != b);
    assert_eq!(mine, theirs);
}

/// Edits, self-deletes and admin deletes of messages from before the receiver
/// joined, arriving before, during and after the copy: the end state is the
/// source's, and nothing deleted comes back.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn changes_racing_the_copy_converge_and_never_resurrect() {
    let (desk, bob, channel, dm, ids) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    // Before: both devices see it; the source's row already carries it.
    bob.edit_message(&channel, &ids[5], "before").await;
    ingest(&desk, &channel).await;
    ingest(&phone, &channel).await;

    // During: the source seals its rows, then (stopped before upload) the
    // changes land and the receiver ingests them before importing the stale rows.
    let t = request(&phone, &desk, "sibling").await.unwrap();
    approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();
    hooks::crash_next_send(Crash::TransferSourceAfterStore);
    let err = run(&desk, &t.transfer_id).await.unwrap_err();
    assert!(err.contains("killed"), "{err}");
    bob.edit_message(&channel, &ids[7], "during").await;
    bob.delete_message(&ids[9]).await;
    // Alice is the group's admin: she deletes bob's message for everyone.
    desk.delete_message(&ids[11]).await;
    desk.delete_message(&ids[4]).await;
    ingest(&phone, &channel).await;
    {
        let guard = phone.state.local_db.lock().await;
        let conn = guard.as_ref().unwrap().conn();
        let mut st = conn.prepare("SELECT target_id, kind, COALESCE(actor_id, '-') FROM pending_message_change ORDER BY target_id, kind").unwrap();
        let got: Vec<(String, String, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(|r| r.unwrap()).collect();
        let (alice, bobid) = (desk.user_id().to_string(), bob.user_id().to_string());
        let mut want = vec![
            // The "before" edit too: its target predates the phone, so only a copy can bring it.
            (ids[5].clone(), "edit".to_string(), bobid.clone()),
            (ids[7].clone(), "edit".to_string(), bobid.clone()),
            (ids[9].clone(), "redact".to_string(), bobid.clone()),
            (ids[11].clone(), "tombstone".to_string(), "-".to_string()),
            (ids[4].clone(), "redact".to_string(), alice),
        ];
        want.sort();
        assert_eq!(got, want, "exactly the changes that reached the phone before their targets");
    }
    let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
    assert_eq!(s.unwrap().state, "completed");
    assert_eq!(d.unwrap().state, "completed");
    ingest(&desk, &channel).await;

    // After: changes to copied rows apply directly.
    bob.edit_message(&channel, &ids[3], "after").await;
    bob.delete_message(&ids[1]).await;
    ingest(&phone, &channel).await;
    ingest(&desk, &channel).await;

    let p = rows(&phone, &[&channel]).await;
    let get = |id: &str| p.iter().find(|x| x.0 == id).cloned().unwrap();
    assert_eq!(get(&ids[5]).3.as_deref(), Some("before"));
    assert_eq!(get(&ids[7]).3.as_deref(), Some("during"));
    assert_eq!(get(&ids[3]).3.as_deref(), Some("after"));
    for deleted in [&ids[9], &ids[11], &ids[4], &ids[1], &ids[2]] {
        let r = get(deleted);
        assert!(r.3.is_none() && r.8, "{deleted} came back: {r:?}");
    }
    assert_eq!(p, rows(&desk, &[&channel]).await);
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM pending_message_change").await, 0);
    assert_same_rows_per_conversation(&phone, &desk, &[&channel]).await;
    drop((dm, bob));
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_receivers_retention_window_bounds_what_comes_across() {
    let (desk, _bob, channel, dm, ids) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    exec(&desk, &format!("UPDATE message SET received_at = datetime('now', '-40 days') WHERE id IN ('{}', '{}')", ids[0], ids[1])).await;
    exec(&desk, &format!("UPDATE message SET received_at = '2020-02-03 04:05:06' WHERE id = '{}'", ids[6])).await;
    phone.invoke_json("set_message_retention", json!({ "days": 365 })).await;
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed");
    let p = rows(&phone, &[&channel]).await;
    assert!(p.iter().any(|x| x.0 == ids[0]), "inside 365 days");
    assert!(!p.iter().any(|x| x.0 == ids[6]), "older than the window never crosses");
    // received_at is carried, normalized, never re-stamped "now".
    assert_eq!(
        scalar(&phone, &format!("SELECT COUNT(*) FROM message WHERE id = '{}' AND received_at < datetime('now', '-39 days')", ids[0])).await,
        1
    );
    // A 30-day window: the 40-day-old rows stay home too.
    let phone2 = new_sibling(&desk, &channel, &dm).await;
    phone2.invoke_json("set_message_retention", json!({ "days": 30 })).await;
    let (_, d) = copy(&desk, &phone2).await;
    assert_eq!(d.state, "completed");
    let p = rows(&phone2, &[&channel]).await;
    assert!(!p.iter().any(|x| x.0 == ids[0] || x.0 == ids[1]));
    assert!(p.iter().any(|x| x.0 == ids[2]));
    drop(phone);
}

#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_conversation_the_account_left_never_crosses() {
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    let bob_name = bob.profile.as_ref().unwrap().username.clone();
    let other = desk.create_group("Left behind").await;
    let other_ch = desk.general_channel_id(&other).await;
    desk.invite(&other, &bob_name).await;
    let inv = bob.first_pending_invite().await.unwrap()["id"].as_str().unwrap().to_string();
    bob.accept_invite(&inv).await;
    bob.poll().await;
    desk.process_commits_for(&other_ch).await;
    desk.send_channel_message(&other_ch, "secret plans").await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    // Bob runs the group; alice leaves it.
    desk.set_member_role(&other, bob.user_id(), "admin").await;
    desk.leave_group(&other).await;
    assert!(rows(&desk, &[&other_ch]).await.len() == 1, "the desk still holds the left group's rows");
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed");
    assert!(rows(&phone, &[&other_ch]).await.is_empty());
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
}

/// A kill at every point on either side, then a lock/unlock and a resume:
/// exactly-once, the same final state as an uninterrupted copy.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_kill_at_any_point_resumes_to_exactly_the_same_result() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    for (n, point) in [
        Crash::TransferSourceAfterStore,
        Crash::TransferSourceAfterPut,
        Crash::TransferReceiverAfterOpen,
        Crash::TransferReceiverAfterCommit,
    ]
    .into_iter()
    .enumerate()
    {
        let phone = new_sibling(&desk, &channel, &dm).await;
        let t = request(&phone, &desk, "sibling").await.unwrap();
        approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();
        hooks::crash_next_send(point);
        let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
        let crashed = [&s, &d].iter().filter(|r| r.as_ref().is_err_and(|e| e.contains("killed"))).count();
        assert_eq!(crashed, 1, "{point:?}: {s:?} {d:?}");
        if point == Crash::TransferReceiverAfterOpen {
            // The chunk rolled back whole.
            assert!(rows(&phone, &[&channel, &dm]).await.is_empty(), "{point:?}");
        }
        lock_unlock(&desk).await;
        lock_unlock(&phone).await;
        let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
        assert_eq!(d.unwrap().state, "completed", "{point:?}");
        assert_eq!(s.unwrap().state, "completed", "{point:?}");
        assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await, "{point:?}");
        assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
        let _ = n;
    }
}

/// A DS that tampers with what it relays: the receiver aborts, writes nothing
/// from the bad chunk, and never completes short.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_tampering_ds_is_caught_and_nothing_bad_is_written() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let remote = writable_remote().await;

    /// Seal and upload what fits, then stop the source (nobody acks, so it pauses).
    async fn staged(desk: &TestClient, phone: &TestClient) -> String {
        let t = request(phone, desk, "sibling").await.unwrap();
        approve(desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();
        let s = run(desk, &t.transfer_id).await.unwrap();
        assert_eq!(s.state, "paused", "{s:?}");
        t.transfer_id
    }

    for name in ["bit-flip", "truncate", "substitute acc", "swap order", "splice"] {
        let phone = new_sibling(&desk, &channel, &dm).await;
        let id = staged(&desk, &phone).await;
        let conn = remote.conn().await.unwrap();
        let chunk = |idx: i64| {
            let conn = conn.clone();
            let id = id.clone();
            async move {
                let mut r = conn
                    .query("SELECT sealed FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = ?2", libsql::params![id, idx])
                    .await
                    .unwrap();
                r.next().await.unwrap().map(|row| row.get::<Vec<u8>>(0).unwrap())
            }
        };
        // The DS's own code never rewrites a stored chunk; a malicious operator does.
        conn.execute("DROP TRIGGER history_transfer_chunk_immutable", ()).await.unwrap();
        let set = |idx: i64, bytes: Vec<u8>| {
            let conn = conn.clone();
            let id = id.clone();
            async move {
                conn.execute(
                    "UPDATE history_transfer_chunk SET sealed = ?3 WHERE transfer_id = ?1 AND idx = ?2",
                    libsql::params![id, idx, bytes],
                )
                .await
                .unwrap();
            }
        };
        match name {
            "bit-flip" => {
                let mut b = chunk(0).await.unwrap();
                let i = b.len() / 2;
                b[i] ^= 1;
                set(0, b).await;
            }
            "truncate" => {
                conn.execute("DELETE FROM history_transfer_chunk WHERE transfer_id = ?1", libsql::params![id.clone()]).await.unwrap();
            }
            "substitute acc" => {
                // Valid-looking but not the source's: its signature cannot match.
                conn.execute("UPDATE history_transfer SET acc = X'7B7D' WHERE transfer_id = ?1", libsql::params![id.clone()])
                    .await
                    .unwrap();
            }
            "swap order" => {
                let (a, b) = (chunk(0).await.unwrap(), chunk(1).await.unwrap());
                set(0, b).await;
                set(1, a).await;
            }
            "splice" => {
                // Another account's transfer chunk, from a separate world? Same
                // effect with any other key: bytes sealed for another transfer.
                let mut b = chunk(0).await.unwrap();
                let n = b.len();
                b[n - 1] ^= 0x80;
                b[2..26].copy_from_slice(&[0u8; 24]);
                set(0, b).await;
            }
            _ => unreachable!(),
        }
        restore_chunk_trigger(&conn).await;
        let d = run(&phone, &id).await.expect("run");
        if name == "truncate" {
            assert_eq!(d.state, "paused", "{name}: {d:?}");
        } else {
            assert_eq!(d.state, "failed", "{name}: {d:?}");
        }
        assert!(rows(&phone, &[&channel, &dm]).await.is_empty(), "{name}: nothing written");
        ht::cancel_history_transfer(&phone.state, id.clone()).await.ok();
        ht::cancel_history_transfer(&desk.state, id.clone()).await.ok();
    }
}

/// A chunk replayed at a later index (after the receiver imported it) is
/// refused: the index is inside the AEAD's associated data.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_replayed_chunk_is_refused() {
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    // Enough history for several chunks is not needed: chunk 0 and the final
    // chunk 1 are two indices to swap.
    let phone = new_sibling(&desk, &channel, &dm).await;
    let t = request(&phone, &desk, "sibling").await.unwrap();
    approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();
    assert_eq!(run(&desk, &t.transfer_id).await.unwrap().state, "paused");
    hooks::crash_next_send(Crash::TransferReceiverAfterCommit);
    assert!(run(&phone, &t.transfer_id).await.unwrap_err().contains("killed"));
    let imported = rows(&phone, &[&channel, &dm]).await;
    assert!(!imported.is_empty());
    let conn = writable_remote().await.conn().await.unwrap();
    let zero: Vec<u8> = {
        let mut r = conn
            .query("SELECT sealed FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = 0", libsql::params![t.transfer_id.clone()])
            .await
            .unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };
    conn.execute("DROP TRIGGER history_transfer_chunk_immutable", ()).await.unwrap();
    conn.execute(
        "UPDATE history_transfer_chunk SET sealed = ?2 WHERE transfer_id = ?1 AND idx = 1",
        libsql::params![t.transfer_id.clone(), zero],
    )
    .await
    .unwrap();
    restore_chunk_trigger(&conn).await;
    let d = run(&phone, &t.transfer_id).await.unwrap();
    assert_eq!(d.state, "failed", "{d:?}");
    // Only the genuine chunk 0 is there, once.
    assert_eq!(rows(&phone, &[&channel, &dm]).await, imported);
    // Nothing beyond the genuine chunk 0 was written (its rows count exactly once).
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM message WHERE origin = 'transfer'").await, imported.len() as i64);
    drop(bob);
}

/// A request whose signed bytes the DS altered (a substituted key) never
/// reaches the approval screen; one naming another device is refused too.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_forged_or_redirected_request_is_never_shown() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let t = request(&phone, &desk, "sibling").await.unwrap();
    let remote = writable_remote().await;
    let conn = remote.conn().await.unwrap();
    let req: Vec<u8> = {
        let mut r = conn
            .query("SELECT req FROM history_transfer WHERE transfer_id = ?1", libsql::params![t.transfer_id.clone()])
            .await
            .unwrap();
        r.next().await.unwrap().unwrap().get(0).unwrap()
    };
    let mut v: serde_json::Value = serde_json::from_slice(&req).unwrap();
    v["kem_pub_x25519"] = json!(base64_of(&[9u8; 32]));
    conn.execute(
        "UPDATE history_transfer SET req = ?2 WHERE transfer_id = ?1",
        libsql::params![t.transfer_id.clone(), serde_json::to_vec(&v).unwrap()],
    )
    .await
    .unwrap();
    assert!(pending(&desk).await.is_empty(), "a substituted key must not verify");
    assert!(approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.is_err());
    // Restore the bytes but sign-swap: another device id as the receiver.
    conn.execute("UPDATE history_transfer SET req = ?2 WHERE transfer_id = ?1", libsql::params![t.transfer_id.clone(), req.clone()])
        .await
        .unwrap();
    assert_eq!(pending(&desk).await.len(), 1);
    conn.execute(
        "UPDATE history_transfer SET dst_device_id = 'someone-else' WHERE transfer_id = ?1",
        libsql::params![t.transfer_id.clone()],
    )
    .await
    .unwrap();
    assert!(pending(&desk).await.is_empty(), "the signed receiver must match the DS's");
}

/// Unread counts after a copy equal the source's (cursors converge), and a
/// notification fetch never previews a copied row.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn copied_history_is_not_unread_news() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    desk.invoke_json("mark_conversation_read", json!({ "conversationId": channel, "userId": desk.user_id() })).await;
    desk.invoke_json("sync_read_cursors", json!({ "userId": desk.user_id() })).await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed");
    assert_eq!(unread(&phone, &channel).await, unread(&desk, &channel).await);
    assert_eq!(unread(&phone, &channel).await, 0);
    assert_eq!(unread(&phone, &dm).await, unread(&desk, &dm).await);
    // The notification query's filter: no copied row qualifies as news.
    assert_eq!(
        scalar(&phone, "SELECT COUNT(*) FROM message WHERE origin IS NULL AND id IN (SELECT id FROM message WHERE origin = 'transfer')").await,
        0
    );
}

/// The DS's record expiring under an open request ends it on both sides.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn an_expired_request_ends_cleanly() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let t = request(&phone, &desk, "sibling").await.unwrap();
    writable_remote()
        .await
        .conn()
        .await
        .unwrap()
        .execute("UPDATE history_transfer SET expires_at = 1 WHERE transfer_id = ?1", libsql::params![t.transfer_id.clone()])
        .await
        .unwrap();
    assert!(pending(&desk).await.is_empty());
    let r = run(&phone, &t.transfer_id).await.unwrap();
    assert_eq!(r.state, "expired");
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM history_transfer_local WHERE kem_sk IS NOT NULL").await, 0);
    // A new request works afterwards.
    let t2 = request(&phone, &desk, "sibling").await.unwrap();
    approve(&desk, &t2.transfer_id, Some(TEST_PIN)).await.unwrap();
    let (_, d) = run_both(&desk, &phone, &t2.transfer_id).await;
    assert_eq!(d.unwrap().state, "completed");
}

async fn receipts(c: &TestClient, conversation: &str) -> Vec<(String, String, String)> {
    let guard = c.state.local_db.lock().await;
    let conn = guard.as_ref().unwrap().conn();
    let mut st = conn
        .prepare(
            "SELECT r.message_id, r.reader_id, r.kind FROM message_receipt r JOIN message m ON m.id = r.message_id
             WHERE m.conversation_id = ?1 ORDER BY 1, 2, 3",
        )
        .unwrap();
    st.query_map([conversation], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

async fn unread(c: &TestClient, conversation: &str) -> i64 {
    c.invoke_json("get_unread_counts", json!({ "userId": c.user_id() }))
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["conversation_id"] == conversation)
        .map(|u| u["count"].as_i64().unwrap())
        .unwrap_or(0)
}

/// Put back the immutability trigger a test dropped to play a malicious
/// operator (the world's DB outlives the test).
async fn restore_chunk_trigger(conn: &libsql::Connection) {
    conn.execute(
        "CREATE TRIGGER IF NOT EXISTS history_transfer_chunk_immutable BEFORE UPDATE ON history_transfer_chunk \
         BEGIN SELECT RAISE(ABORT, 'history_transfer_chunk: stored chunks are immutable'); END",
        (),
    )
    .await
    .unwrap();
}

fn base64_of(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// Seeded soak: messages, edits, self-deletes and admin deletes from both
/// sides, ingests on random devices, locks, and transfers cut short at random
/// kill points, interleaved; then one last copy. The receiver must end with
/// exactly the source's history: no duplicate, nothing deleted brought back.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn soak_transfers_interleaved_with_traffic_and_kills() {
    for seed in [0x1735_u64, 0xC0FFEE] {
        soak_one(seed, 30).await;
    }
}

async fn soak_one(seed: u64, steps: usize) {
    let mut rng = seed;
    let mut next = |n: u64| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng % n
    };
    let (desk, bob, channel, dm, mut ids) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let mut bob_ids: Vec<String> = ids.iter().skip(1).step_by(2).cloned().collect();
    let mut open: Option<String> = None;
    let points = [
        Crash::TransferSourceAfterStore,
        Crash::TransferSourceAfterPut,
        Crash::TransferReceiverAfterOpen,
        Crash::TransferReceiverAfterCommit,
    ];
    for step in 0..steps {
        match next(9) {
            0 => bob_ids.push(bob.send_channel_message_id(&channel, &format!("bob s{step}")).await),
            1 => ids.push(desk.send_channel_message_id(&channel, &format!("desk s{step}")).await),
            2 if !bob_ids.is_empty() => {
                let id = bob_ids[next(bob_ids.len() as u64) as usize].clone();
                bob.edit_message(&channel, &id, &format!("edit s{step}")).await;
            }
            3 if !bob_ids.is_empty() => {
                let id = bob_ids.remove(next(bob_ids.len() as u64) as usize);
                bob.delete_message(&id).await;
            }
            4 if !bob_ids.is_empty() => {
                let id = bob_ids.remove(next(bob_ids.len() as u64) as usize);
                desk.delete_message(&id).await;
            }
            5 => {
                let who = [&desk, &phone, &bob][next(3) as usize];
                ingest(who, &channel).await;
            }
            6 => {
                if open.is_none() {
                    let t = request(&phone, &desk, "sibling").await.expect("request");
                    approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.expect("approve");
                    open = Some(t.transfer_id);
                }
            }
            7 => {
                if let Some(id) = open.clone() {
                    if next(2) == 0 {
                        hooks::crash_next_send(points[next(4) as usize]);
                    }
                    let (s, d) = run_both(&desk, &phone, &id).await;
                    hooks::disarm_crash();
                    if s.is_err() || d.is_err() {
                        assert!(
                            [&s, &d].iter().all(|r| r.as_ref().map_or_else(|e| e.contains("killed"), |_| true)),
                            "seed {seed:#x} step {step}: {s:?} {d:?}"
                        );
                        lock_unlock(&desk).await;
                        lock_unlock(&phone).await;
                    }
                    if d.as_ref().is_ok_and(|t| t.is_ended()) {
                        assert_eq!(d.as_ref().unwrap().state, "completed", "seed {seed:#x} step {step}");
                        open = None;
                    }
                }
            }
            _ => lock_unlock([&desk, &phone][next(2) as usize]).await,
        }
    }
    // Finish what is open, settle every device, then one last full copy.
    if let Some(id) = open.take() {
        loop {
            let (_, d) = run_both(&desk, &phone, &id).await;
            if d.expect("final run").is_ended() {
                break;
            }
        }
    }
    for c in [&desk, &phone, &bob, &desk, &phone] {
        ingest(c, &channel).await;
    }
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed", "seed {seed:#x}");
    let p = rows(&phone, &[&channel, &dm]).await;
    let dsk = rows(&desk, &[&channel, &dm]).await;
    for r in dsk.iter().filter(|r| !p.contains(r)) {
        let guard = phone.state.local_db.lock().await;
        let there: Option<String> = guard
            .as_ref()
            .unwrap()
            .conn()
            .query_row(
                "SELECT json_array(conversation_id, sender_id, content, deleted_at, undecryptable, send_state, origin) FROM message WHERE id = ?1",
                [&r.0],
                |x| x.get(0),
            )
            .ok();
        eprintln!("seed {seed:#x}: desk row {r:?} — on the phone: {there:?}");
    }
    assert_eq!(p, dsk, "seed {seed:#x}");
    // Every live message equals bob's, who was there all along. (Tombstone ROWS
    // differ by device today, independent of #1735: a message deleted before a
    // device fetched it leaves that device no row at all.)
    let live = |v: Vec<RowView>| v.into_iter().filter(|r| !r.8).collect::<Vec<_>>();
    assert_eq!(live(rows(&phone, &[&channel]).await), live(rows(&bob, &[&channel]).await), "seed {seed:#x}");
    // ...and no message the source holds deleted is live here.
    let deleted_on_desk: Vec<String> = dsk.iter().filter(|r| r.8).map(|r| r.0.clone()).collect();
    assert!(p.iter().all(|r| !deleted_on_desk.contains(&r.0) || r.8), "seed {seed:#x}: a deleted message is live");
    assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
    // A change may still wait for a target nobody holds (a message deleted before
    // any device fetched it); none may wait for a row this device HAS.
    assert_eq!(
        scalar(&phone, "SELECT COUNT(*) FROM pending_message_change p JOIN message m ON m.id = p.target_id AND m.undecryptable IS NULL").await,
        0,
        "seed {seed:#x}"
    );
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM message WHERE deleted_at IS NOT NULL AND content IS NOT NULL").await, 0);
    drop((desk, bob, phone));
}


/// More than a window of chunks (8): the source fills the window and waits;
/// a receiver killed in the middle of a chunk, and a source killed between
/// storing and uploading one, both resume to the exact result.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn many_chunks_through_the_window_with_kills_in_the_middle() {
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    for i in 0..12 {
        bob.send_channel_message(&channel, &format!("more {i}")).await;
    }
    ingest(&desk, &channel).await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    ht::transfer_hooks::set_rows_per_chunk(2);
    let t = request(&phone, &desk, "sibling").await.unwrap();
    approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();

    // Alone, the source seals exactly a window and waits.
    assert_eq!(run(&desk, &t.transfer_id).await.unwrap().state, "paused");
    assert_eq!(scalar(&desk, "SELECT COUNT(*) FROM history_transfer_outgoing").await, 8);
    // The receiver imports those 8 and waits for more.
    let d = run(&phone, &t.transfer_id).await.unwrap();
    assert_eq!((d.state.as_str(), d.chunks_done), ("paused", 8), "{d:?}");
    // The source is killed after storing chunk 8, before uploading it.
    hooks::crash_next_send(Crash::TransferSourceAfterStore);
    assert!(run(&desk, &t.transfer_id).await.unwrap_err().contains("killed"));
    lock_unlock(&desk).await;
    // The receiver is killed in the middle of importing chunk 8.
    hooks::crash_next_send(Crash::TransferReceiverAfterOpen);
    let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
    assert!(d.as_ref().unwrap_err().contains("killed"), "{s:?} {d:?}");
    assert_eq!(scalar(&phone, "SELECT next_index FROM history_transfer_local").await, 8, "chunk 8 rolled back whole");
    lock_unlock(&phone).await;
    let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
    let (s, d) = (s.unwrap(), d.unwrap());
    assert_eq!((s.state.as_str(), d.state.as_str()), ("completed", "completed"), "{s:?} {d:?}");
    assert!(d.chunks_done > 10, "{d:?}");
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
    assert_same_rows_per_conversation(&phone, &desk, &[&channel, &dm]).await;
    assert_eq!(scalar(&desk, "SELECT COUNT(*) FROM history_transfer_outgoing").await, 0);
    ht::transfer_hooks::set_rows_per_chunk(0);
}

/// A transient DS error while verifying the acceptance is not a verdict: the
/// run stops, the transfer stays open, and the next run completes (M1).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_transient_error_at_acceptance_resumes() {
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let t = request(&phone, &desk, "sibling").await.unwrap();
    approve(&desk, &t.transfer_id, Some(TEST_PIN)).await.unwrap();
    crate::harness::arm_ds_fault(crate::harness::DsFault::DevicesReadUnavailable);
    let first = run(&phone, &t.transfer_id).await;
    assert!(first.is_err(), "{first:?}");
    let listed = ht::list_history_transfers(&phone.state).await.unwrap();
    assert_eq!(listed[0].state, "awaiting_approval", "a transient error never fails the transfer");
    let (s, d) = run_both(&desk, &phone, &t.transfer_id).await;
    assert_eq!(d.unwrap().state, "completed");
    assert_eq!(s.unwrap().state, "completed");
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
}

/// A conversation the receiver belongs to but holds no group for yet (its
/// Welcome not processed) is reported as deferred, not silently dropped (M4).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_conversation_not_joined_yet_is_reported_deferred() {
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let late = desk.create_group("Later").await;
    let late_ch = desk.general_channel_id(&late).await;
    desk.invite(&late, &bob.profile.as_ref().unwrap().username).await;
    desk.send_channel_message(&late_ch, "phone has no group for this yet").await;
    // The phone has not processed the new group's Welcome.
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.state, "completed");
    assert!(d.deferred_conversations >= 1, "{d:?}");
    assert!(rows(&phone, &[&late_ch]).await.is_empty());
    // Once it joins, copying again brings it.
    phone.poll().await;
    ingest(&phone, &late_ch).await;
    let (_, d) = copy(&desk, &phone).await;
    assert_eq!(d.deferred_conversations, 0, "{d:?}");
    assert_eq!(rows(&phone, &[&late_ch]).await, rows(&desk, &[&late_ch]).await);
}


/// Wait (bounded) until `c`'s transfer `id` has ended; its final state.
async fn ended(c: &TestClient, id: &str) -> HistoryTransfer {
    let mut since: Option<String> = None;
    for _ in 0..12 {
        let t = ht::await_history_transfer(&c.state, id.to_string(), since.clone()).await.expect("await");
        if t.is_ended() {
            return t;
        }
        since = Some(t.updated_at.clone());
    }
    panic!("transfer {id} never ended");
}

/// A request and an acceptance that LAND on the DS but whose answers are lost
/// leave both rows open (never `failed` with the key wiped while the DS keeps
/// the transfer), and the source is driven in the background with nobody
/// starting a run on it: the copy still completes (review M-A, round 3).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn lost_answers_to_a_request_and_an_acceptance_still_complete() {
    use crate::harness::{arm_ds_fault, DsFault};
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;

    arm_ds_fault(DsFault::TransferRequestLandsThenTimesOut);
    let src = device_id(&desk).await;
    let r = phone.invoke_try("request_history_transfer", json!({ "sourceDeviceId": src, "via": "sibling" })).await;
    assert!(r.as_ref().is_err_and(|e| e.contains("keeps trying")), "the answer was lost: {r:?}");
    let mine = ht::list_history_transfers(&phone.state).await.unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].state, "awaiting_approval", "kept open, key intact");
    let id = mine[0].transfer_id.clone();
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM history_transfer_local WHERE kem_sk IS NOT NULL").await, 1);
    // It did land: the source sees it.
    assert_eq!(pending(&desk).await.len(), 1);

    arm_ds_fault(DsFault::TransferAcceptLandsThenTimesOut);
    let a = approve(&desk, &id, Some(TEST_PIN)).await;
    assert!(a.as_ref().is_err_and(|e| e.contains("keeps trying")), "the answer was lost: {a:?}");
    // Nobody runs the source: only the receiver is driven, as its screen would.
    let d = run(&phone, &id).await.unwrap();
    let d = if d.is_ended() { d } else { run(&phone, &id).await.unwrap() };
    assert_eq!(d.state, "completed", "{d:?}");
    assert_eq!(ended(&desk, &id).await.state, "completed");
    assert_eq!(rows(&phone, &[&channel, &dm]).await, rows(&desk, &[&channel, &dm]).await);
    assert_eq!(scalar(&desk, "SELECT COUNT(*) FROM history_transfer_local WHERE transfer_key IS NOT NULL").await, 0);
}

/// A request that never reached the DS (its answer lost too) is kept open, the
/// background run learns from the DS that it does not exist and ends it as
/// expired — never `failed` — and the next request is not blocked.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_request_that_never_landed_does_not_block_the_next() {
    use crate::harness::{arm_ds_fault, DsFault};
    let (desk, _bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    arm_ds_fault(DsFault::TransferRequestTimesOutUnlanded);
    assert!(request(&phone, &desk, "sibling").await.is_err());
    let old = ht::list_history_transfers(&phone.state).await.unwrap().remove(0);
    assert!(pending(&desk).await.is_empty(), "it never landed");
    assert_eq!(ended(&phone, &old.transfer_id).await.state, "expired");
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM history_transfer_local WHERE kem_sk IS NOT NULL").await, 0);
    let t2 = request(&phone, &desk, "sibling").await.expect("a new request is not refused");
    assert_ne!(t2.transfer_id, old.transfer_id);
    approve(&desk, &t2.transfer_id, Some(TEST_PIN)).await.unwrap();
    let (_, d) = run_both(&desk, &phone, &t2.transfer_id).await;
    assert_eq!(d.unwrap().state, "completed");
}

/// Ingest's own heal of a placeholder (#1312) applies the changes that waited
/// for it, through the real decrypt path: a tombstone and the author's
/// redaction delete, the author's latest edit applies, another member's
/// redaction does not; and a placeholder that was tombstoned heals with no text.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn ingest_heals_a_placeholder_and_applies_what_waited_for_it() {
    let (desk, bob, channel, dm, _) = desk_with_history().await;
    let phone = new_sibling(&desk, &channel, &dm).await;
    let alice = desk.user_id().to_string();
    let bobid = bob.user_id().to_string();
    // Five messages the phone has not fetched yet.
    let mut ids = Vec::new();
    for i in 0..5 {
        ids.push(bob.send_channel_message_id(&channel, &format!("heal {i}")).await);
    }
    let env = || ulid::Ulid::new().to_string();
    // Each stands on the phone as a placeholder (a pass that could not open it),
    // with a change that reached the phone first.
    for (i, id) in ids.iter().enumerate() {
        let deleted = if i == 4 { "'2026-01-01T00:00:00Z'" } else { "NULL" };
        exec(&phone, &format!(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable, deleted_at)
             VALUES ('{id}', '{channel}', 'sealed', X'', NULL, '2026-01-01T00:00:00Z', 'failed', {deleted})"
        ))
        .await;
    }
    let pending = |envelope: String, target: &str, kind: &str, actor: Option<&str>, content: Option<&str>| {
        format!(
            "INSERT INTO pending_message_change (envelope_id, target_id, conversation_id, kind, actor_id, content, seq)
             VALUES ('{envelope}', '{target}', '{channel}', '{kind}', {}, {}, 1)",
            actor.map(|a| format!("'{a}'")).unwrap_or("NULL".into()),
            content.map(|c| format!("'{c}'")).unwrap_or("NULL".into()),
        )
    };
    exec(&phone, &pending(env(), &ids[0], "tombstone", None, None)).await;
    exec(&phone, &pending(env(), &ids[1], "edit", Some(&bobid), Some("edited while lost"))).await;
    exec(&phone, &pending(env(), &ids[2], "redact", Some(&bobid), None)).await;
    exec(&phone, &pending(env(), &ids[3], "redact", Some(&alice), None)).await;

    ingest(&phone, &channel).await;

    let r = rows(&phone, &[&channel]).await;
    let get = |id: &str| r.iter().find(|x| x.0 == id).cloned().unwrap_or_else(|| panic!("{id} not healed"));
    assert!(get(&ids[0]).3.is_none() && get(&ids[0]).8, "tombstone applied");
    assert_eq!(get(&ids[1]).3.as_deref(), Some("edited while lost"));
    assert!(get(&ids[1]).7);
    assert!(get(&ids[2]).3.is_none() && get(&ids[2]).8, "the author's redaction applied");
    assert_eq!(get(&ids[3]).3.as_deref(), Some("heal 3"), "another member's redaction never applies");
    assert!(get(&ids[4]).3.is_none() && get(&ids[4]).8, "a tombstoned placeholder heals with no text");
    for (i, id) in ids.iter().enumerate() {
        assert_eq!(get(id).2, bobid, "{i}: the real author replaces the sealed sentinel");
    }
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM message WHERE undecryptable IS NOT NULL").await, 0);
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM pending_message_change").await, 0);
    assert_eq!(scalar(&phone, "SELECT COUNT(*) FROM message WHERE content IS NOT NULL AND deleted_at IS NOT NULL").await, 0);
}
