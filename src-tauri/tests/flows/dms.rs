use std::sync::Arc;

use crate::harness::{enroll_second_device, wipe, TestClient};
use pollis_lib::test_harness::invoke;
use serde_json::json;
use serial_test::serial;

/// Alice creates a DM to Bob. Bob sees it as a pending request (not in his
/// accepted DM list). After accepting, it moves to his DM channel list.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_request_accept_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()]).await;

    // Alice created — it's accepted on her side (she sees it as a channel).
    let alice_channels = alice.list_dms().await;
    assert_eq!(alice_channels.len(), 1);
    assert_eq!(alice_channels[0]["id"], dm_id);

    // Bob sees it as a pending request, not an accepted channel.
    let bob_requests = bob.list_dm_requests().await;
    assert_eq!(bob_requests.len(), 1);
    assert_eq!(bob_requests[0]["id"], dm_id);
    assert!(bob.list_dms().await.is_empty());

    bob.accept_dm_request(&dm_id).await;

    // After accept, it's in Bob's channels and gone from his requests.
    assert!(bob.list_dm_requests().await.is_empty());
    let bob_channels = bob.list_dms().await;
    assert_eq!(bob_channels.len(), 1);
    assert_eq!(bob_channels[0]["id"], dm_id);

    drop(alice);
    drop(bob);
}

/// If Bob blocks Alice before she creates a DM, Alice's create_dm_channel
/// should fail with the generic BLOCK_ERR message — the block is
/// indistinguishable from any other "pending" state to the sender.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn block_prevents_dm_creation() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    bob.block(&alice_profile.id).await;

    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "create_dm_channel",
        json!({
            "creatorId": alice_profile.id,
            "memberIds": [alice_profile.id.clone(), bob_profile.id.clone()],
        }),
    )
    .await
    .err()
    .expect("create_dm_channel should fail when blocked");
    assert!(
        err.contains("message request pending"),
        "expected BLOCK_ERR, got: {err}"
    );

    // And no DM rows on either side.
    assert!(alice.list_dms().await.is_empty());
    assert!(alice.list_dm_requests().await.is_empty());
    assert!(bob.list_dms().await.is_empty());
    assert!(bob.list_dm_requests().await.is_empty());

    drop(alice);
    drop(bob);
}

/// Multi-device DM: alice and bob each run two enrolled devices. A DM
/// between them must reach all four leaves of the MLS tree, because
/// `dm_channel_member` is keyed per user but the tree expands to every
/// enrolled device of every member during reconcile. A non-member
/// (carol) cannot decrypt any of the four messages.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_multi_device_round_trip() {
    wipe().await;

    // ── Setup: alice_d1 + alice_d2, bob_d1 + bob_d2 ──
    let mut alice_d1 = TestClient::new().await;
    let alice_profile = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // ── Create DM: alice_d1 → bob, bob_d1 accepts ──
    let dm_id = alice_d1
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    // Bob sees the pending request on d1 and accepts.
    assert_eq!(bob_d1.list_dm_requests().await.len(), 1);
    bob_d1.accept_dm_request(&dm_id).await;

    // ── Warm MLS on all four devices ──
    // `get_dm_messages` polls welcomes and processes pending commits, so
    // each call advances that device's local MLS state to the current
    // epoch for this conversation.
    alice_d1.fetch_dm_messages(&dm_id).await;
    alice_d2.fetch_dm_messages(&dm_id).await;
    bob_d1.fetch_dm_messages(&dm_id).await;
    bob_d2.fetch_dm_messages(&dm_id).await;

    // ── Carol: signs up, does NOT join the DM. Baseline for non-member. ──
    let mut carol = TestClient::new().await;
    let _carol_profile = carol.sign_up("carol@test.local").await;

    // ── Helper: assert that exactly the three "other" devices decrypt `content`. ──
    async fn assert_other_three_decrypt(
        sender_label: &str,
        content: &str,
        dm_id: &str,
        others: [(&TestClient, &str); 3],
        carol: &TestClient,
    ) {
        for (client, label) in others.iter() {
            let msgs = client.fetch_dm_messages(dm_id).await;
            let contents: Vec<&str> =
                msgs.iter().filter_map(|m| m["content"].as_str()).collect();
            assert!(
                contents.contains(&content),
                "{label} should decrypt '{content}' sent by {sender_label}, got: {contents:?}"
            );
        }
        let carol_msgs = carol.fetch_dm_messages(dm_id).await;
        let carol_contents: Vec<&str> =
            carol_msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            !carol_contents.contains(&content),
            "non-member carol must not decrypt '{content}', got: {carol_contents:?}"
        );
    }

    // ── Round trip #1: alice_d1 → the other three ──
    alice_d1
        .send_channel_message(&dm_id, "hi from alice-d1")
        .await;
    assert_other_three_decrypt(
        "alice_d1",
        "hi from alice-d1",
        &dm_id,
        [
            (&alice_d2, "alice_d2"),
            (&bob_d1, "bob_d1"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #2: alice_d2 → the other three ──
    alice_d2
        .send_channel_message(&dm_id, "hi from alice-d2")
        .await;
    assert_other_three_decrypt(
        "alice_d2",
        "hi from alice-d2",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&bob_d1, "bob_d1"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #3: bob_d1 → the other three ──
    bob_d1
        .send_channel_message(&dm_id, "hi from bob-d1")
        .await;
    assert_other_three_decrypt(
        "bob_d1",
        "hi from bob-d1",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&alice_d2, "alice_d2"),
            (&bob_d2, "bob_d2"),
        ],
        &carol,
    )
    .await;

    // ── Round trip #4: bob_d2 → the other three ──
    bob_d2
        .send_channel_message(&dm_id, "hi from bob-d2")
        .await;
    assert_other_three_decrypt(
        "bob_d2",
        "hi from bob-d2",
        &dm_id,
        [
            (&alice_d1, "alice_d1"),
            (&alice_d2, "alice_d2"),
            (&bob_d1, "bob_d1"),
        ],
        &carol,
    )
    .await;

    drop(alice_d1);
    drop(alice_d2);
    drop(bob_d1);
    drop(bob_d2);
    drop(carol);
}

/// Rejecting a DM invite must remove EVERY device of the rejecter from the
/// MLS tree, not just the device that invoked reject — and after the reject,
/// alice can open a FRESH DM with bob and both sides converge again.
///
/// Phase 1 (reject): `leave_dm_channel` deletes the rejecter's single
/// `dm_channel_member` row (there is one row per user, not per device) and then
/// reconcile removes every leaf in the MLS tree that belongs to that user. Bob
/// runs two devices, rejects from `bob_d1`, and neither `bob_d1` nor `bob_d2`
/// may decrypt subsequent messages sent by alice.
///
/// Phases 2-5 (re-invite): every bob device must re-enter the MLS tree with a
/// brand-new `dm_channel.id`, and no stale state from the rejected DM may
/// surface. (These were two tests, `dm_invite_reject_removes_from_tree` and
/// `dm_re_invite_after_reject`, that built the same two-device world and
/// rejected the same way; the second never checked the reject's effects.)
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_invite_reject_removes_from_tree_then_re_invite_converges() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // ── Phase 1: create → reject removes EVERY bob device ──
    // Alice invites bob. Both bob devices land in the MLS tree at creation
    // time — dm.rs:127 "Reconcile then adds all members' devices".
    let rejected_dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    // Pre-reject sanity: bob sees the pending request on both devices.
    assert_eq!(
        bob_d1.list_dm_requests().await.len(),
        1,
        "bob_d1 should see the pending DM request"
    );
    assert_eq!(
        bob_d2.list_dm_requests().await.len(),
        1,
        "bob_d2 should see the pending DM request"
    );

    // bob_d1 rejects. `leave_dm_channel` deletes the `dm_channel_member`
    // row (per-user, so both bob devices lose membership), forgets this
    // device's local MLS state, and signals remaining members to
    // reconcile away the bob leaves.
    bob_d1.leave_dm(&rejected_dm_id).await;

    // `dm_channel_member` has NO rows for bob in this dm.
    let remote = crate::harness::writable_remote().await;
    let bob_member_count: i64 = {
        let conn = remote.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM dm_channel_member \
                 WHERE dm_channel_id = ?1 AND user_id = ?2",
                libsql::params![rejected_dm_id.clone(), bob_profile.id.clone()],
            )
            .await
            .expect("count bob members");
        let row = rows.next().await.expect("row").expect("some row");
        row.get::<i64>(0).expect("count")
    };
    assert_eq!(
        bob_member_count, 0,
        "bob should have zero dm_channel_member rows after reject"
    );

    // Alice reconciles her own tree (pulling the membership change into
    // her local MLS state) and then sends a post-reject message.
    alice.process_commits_for(&rejected_dm_id).await;
    alice
        .send_channel_message(&rejected_dm_id, "alice post-reject")
        .await;

    // Neither bob device may decrypt alice's new message — their leaves
    // are gone from the tree. They may still see an envelope row on the
    // remote, but the content decrypts to None.
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let msgs = client.fetch_dm_messages(&rejected_dm_id).await;
        let contents: Vec<&str> =
            msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            !contents.contains(&"alice post-reject"),
            "{label} must NOT decrypt 'alice post-reject' after reject, got: {contents:?}"
        );
    }

    // And the DM no longer appears in either bob device's list/requests —
    // from bob's side the conversation is fully gone.
    assert!(
        bob_d1
            .list_dm_requests()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(rejected_dm_id.as_str())),
        "bob_d1 must not list the rejected DM as a request"
    );
    assert!(
        bob_d1
            .list_dms()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(rejected_dm_id.as_str())),
        "bob_d1 must not list the rejected DM as an accepted channel"
    );
    assert!(
        bob_d2
            .list_dm_requests()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(rejected_dm_id.as_str())),
        "bob_d2 must not list the rejected DM as a request"
    );
    assert!(
        bob_d2
            .list_dms()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(rejected_dm_id.as_str())),
        "bob_d2 must not list the rejected DM as an accepted channel"
    );

    // ── Phase 2: fresh DM; a brand-new dm_channel.id ──
    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    assert_ne!(
        dm_id, rejected_dm_id,
        "second create_dm_channel must produce a distinct dm_channel.id"
    );

    // Bob sees only the new pending request, not the rejected one.
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let requests = client.list_dm_requests().await;
        let ids: Vec<&str> = requests
            .iter()
            .filter_map(|r| r["id"].as_str())
            .collect();
        assert!(
            ids.contains(&dm_id.as_str()),
            "{label} should see the NEW dm request {dm_id}, got: {ids:?}"
        );
        assert!(
            !ids.contains(&rejected_dm_id.as_str()),
            "{label} must not see the rejected dm {rejected_dm_id}, got: {ids:?}"
        );
    }

    // bob_d1 accepts. Reconcile pulls in both bob devices.
    bob_d1.accept_dm_request(&dm_id).await;

    // Warm MLS on all three devices in the fresh DM.
    alice.fetch_dm_messages(&dm_id).await;
    bob_d1.fetch_dm_messages(&dm_id).await;
    bob_d2.fetch_dm_messages(&dm_id).await;

    // ── Phase 3: alice → both bob devices decrypt ──
    alice
        .send_channel_message(&dm_id, "fresh-dm alice->bob")
        .await;
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let msgs = client.fetch_dm_messages(&dm_id).await;
        let contents: Vec<&str> =
            msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            contents.contains(&"fresh-dm alice->bob"),
            "{label} should decrypt 'fresh-dm alice->bob' in re-invited DM, got: {contents:?}"
        );
    }

    // ── Phase 4: bob_d2 replies → alice decrypts ──
    bob_d2
        .send_channel_message(&dm_id, "fresh-dm bob-d2->alice")
        .await;
    let alice_msgs = alice.fetch_dm_messages(&dm_id).await;
    let alice_contents: Vec<&str> = alice_msgs
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        alice_contents.contains(&"fresh-dm bob-d2->alice"),
        "alice should decrypt bob_d2's reply, got: {alice_contents:?}"
    );

    // ── Phase 5: no leak on bob's side ──
    // Bob rejected, so the rejected dm must not appear on any of bob's
    // devices. Alice's side may still hold a ghost row (leave_dm_channel
    // only tears down the channel when there are zero remaining members
    // — alice is still the creator and auto-accepted) but that ghost must
    // not interfere with the fresh DM. The new DM's id must appear in
    // alice's accepted list exactly once, while the rejected id's fresh
    // messages are never mixed into the new conversation.
    for (list_name, list) in [
        ("bob_d1 list_dms", bob_d1.list_dms().await),
        ("bob_d1 list_dm_requests", bob_d1.list_dm_requests().await),
        ("bob_d2 list_dms", bob_d2.list_dms().await),
        ("bob_d2 list_dm_requests", bob_d2.list_dm_requests().await),
    ] {
        let ids: Vec<&str> = list.iter().filter_map(|c| c["id"].as_str()).collect();
        assert!(
            !ids.contains(&rejected_dm_id.as_str()),
            "{list_name} must not contain the rejected dm id, got: {ids:?}"
        );
    }
    let alice_new_dm_count = alice
        .list_dms()
        .await
        .iter()
        .filter(|c| c["id"].as_str() == Some(dm_id.as_str()))
        .count();
    assert_eq!(
        alice_new_dm_count, 1,
        "alice should list the fresh DM exactly once"
    );

    // The fresh DM's messages must not carry over onto the rejected id —
    // fetching the rejected id on bob's devices yields no plaintext.
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let msgs = client.fetch_dm_messages(&rejected_dm_id).await;
        let contents: Vec<&str> =
            msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            !contents.contains(&"fresh-dm alice->bob"),
            "{label} must not see fresh-DM plaintext via the rejected id, got: {contents:?}"
        );
    }

    drop(alice);
    drop(bob_d1);
    drop(bob_d2);
}

/// Count rows in the remote `user_block` table for a given (blocker,
/// blocked) pair. Lets tests observe the raw row state without going
/// through a Tauri command that also filters by block.
async fn user_block_count(
    remote: &Arc<pollis_delivery::db::Db>,
    blocker_id: &str,
    blocked_id: &str,
) -> i64 {
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM user_block \
             WHERE blocker_id = ?1 AND blocked_id = ?2",
            libsql::params![blocker_id.to_string(), blocked_id.to_string()],
        )
        .await
        .expect("count user_block");
    let row = rows.next().await.expect("row").expect("some row");
    row.get::<i64>(0).expect("count")
}

/// Block lifecycle: search-before, block hides the DM on alice's side but
/// not bob's, create_dm_channel is refused with BLOCK_ERR, unblock
/// restores visibility, and create_dm_channel works again after unblock.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn user_block_lifecycle() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let remote = crate::harness::writable_remote().await;

    // Before block: search_user_by_username resolves bob for alice.
    let hit: serde_json::Value = alice
        .invoke_json(
            "search_user_by_username",
            json!({ "username": bob_profile.username.clone() }),
        )
        .await;
    assert_eq!(
        hit["id"], bob_profile.id,
        "pre-block search_user_by_username should return bob"
    );
    assert_eq!(
        user_block_count(&remote, &alice_profile.id, &bob_profile.id).await,
        0,
        "no user_block row before block_user runs"
    );

    // Alice creates a DM; bob sees it as a pending request.
    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    let bob_requests = bob.list_dm_requests().await;
    assert!(
        bob_requests.iter().any(|c| c["id"] == dm_id),
        "bob should see alice's DM as a pending request pre-block"
    );

    // ── Block ──
    alice.block(&bob_profile.id).await;
    assert_eq!(
        user_block_count(&remote, &alice_profile.id, &bob_profile.id).await,
        1,
        "block_user should insert a user_block row"
    );

    // Alice's side: DM is hidden from both list_dms and list_dm_requests
    // (block_user also nulls alice's accepted_at, and list_dm_requests
    // filters out channels whose other participant she has blocked).
    assert!(
        alice
            .list_dms()
            .await
            .iter()
            .all(|c| c["id"] != dm_id),
        "post-block: alice's list_dms must not contain the blocked-user's DM"
    );
    assert!(
        alice
            .list_dm_requests()
            .await
            .iter()
            .all(|c| c["id"] != dm_id),
        "post-block: alice's list_dm_requests must not contain the blocked-user's DM"
    );

    // Bob's side must still see the DM — the block is invisible to the
    // blocked party by design (dm.rs:169-173).
    let bob_requests_after_block = bob.list_dm_requests().await;
    assert!(
        bob_requests_after_block.iter().any(|c| c["id"] == dm_id),
        "post-block: bob must continue to see the DM on his side, got: {bob_requests_after_block:?}"
    );

    // create_dm_channel is refused with BLOCK_ERR while the block is
    // active — mirrors block_prevents_dm_creation.
    let err = invoke::<serde_json::Value>(
        &alice.webview,
        "create_dm_channel",
        json!({
            "creatorId": alice_profile.id,
            "memberIds": [alice_profile.id.clone(), bob_profile.id.clone()],
        }),
    )
    .await
    .err()
    .expect("create_dm_channel should fail while blocked");
    assert!(
        err.contains("message request pending"),
        "expected BLOCK_ERR, got: {err}"
    );

    // ── Unblock ──
    alice.unblock(&bob_profile.id).await;
    assert_eq!(
        user_block_count(&remote, &alice_profile.id, &bob_profile.id).await,
        0,
        "unblock_user should remove the user_block row"
    );

    // Alice sees the original DM again. block_user nulled her
    // accepted_at, so the unblocked channel resurfaces as a request
    // (not in list_dms) — both locations are acceptable places for the
    // restored row, assert it appears in at least one.
    let alice_dms_ids: Vec<serde_json::Value> = alice.list_dms().await;
    let alice_req_ids: Vec<serde_json::Value> = alice.list_dm_requests().await;
    let in_dms = alice_dms_ids.iter().any(|c| c["id"] == dm_id);
    let in_reqs = alice_req_ids.iter().any(|c| c["id"] == dm_id);
    assert!(
        in_dms || in_reqs,
        "post-unblock: alice should see the original DM again in either list, \
         dms={alice_dms_ids:?} reqs={alice_req_ids:?}"
    );

    // create_dm_channel works again — use a fresh peer (carol) so the
    // lingering alice↔bob DM from above doesn't tangle with a brand-new
    // channel. Carol signs up now that the block is gone.
    let mut carol = TestClient::new().await;
    let carol_profile = carol.sign_up("carol@test.local").await;
    let new_dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), carol_profile.id.as_str()])
        .await;
    assert_ne!(new_dm_id, dm_id, "new DM should have a fresh id");
    assert!(
        alice
            .list_dms()
            .await
            .iter()
            .any(|c| c["id"] == new_dm_id),
        "post-unblock create_dm_channel should succeed and surface on alice's side"
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// The DM-accept convergence race (#1041), in the ordering CI produced.
///
/// `create_dm_channel` publishes GroupInfo at epoch 0 (`init_mls_group`) BEFORE
/// reconcile adds the recipient, and the recipient's membership row is visible
/// from the first write. A recipient whose sync loop ticks in that window has no
/// Welcome to defer to, so it external-joins: it builds a local branch at epoch
/// 1 and submits a compare-and-swap on epoch 0. If the creator's Add lands
/// first, the join loses, discards its branch and — on the retry — defers to
/// the Welcome the Add produced.
///
/// The recipient is driven the way the TUI drives it: a background sync loop
/// (welcomes → commits → ingest) AND a UI refresh loop (ingest of the open
/// conversation) running concurrently, so the Welcome path (`apply_welcome`)
/// and the external-join path overlap exactly as they do in `pollis-tui`.
///
/// Locally the recipient nearly always WINS epoch 0 (the creator's reconcile
/// does more work), which is why the TUI e2e only ever failed on CI. The
/// rendezvous parks the recipient with its branch built and the CAS unsent, so
/// the losing ordering is reproduced on demand rather than by luck.
///
/// This ordering was the one the e2e's comment blamed, and it is handled: the
/// lost CAS falls back to the Welcome. It was NOT the loss CI saw — that is the
/// committer window `message_sealed_at_the_epoch_a_committer_leaves_is_still_delivered`
/// pins below. Kept because a lost epoch-0 join is a real interleaving that
/// must keep converging.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn recipient_that_loses_the_epoch_zero_race_still_reads_the_creators_message() {
    use pollis_lib::commands::mls::{reconcile_group_mls_impl, rendezvous};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // The DM rows and the creator's half of the MLS bootstrap; the reconcile
    // half is driven by hand below so the recipient can be interleaved.
    let dm_id = dm_with_only_the_creators_group(&alice, &alice_profile.id, &bob_profile.id).await;

    // Bob, shaped like the TUI: a sync loop and a UI refresh loop, both free
    // running until the test is done with them.
    let mut parked = rendezvous::arm(rendezvous::Point::ExternalJoinBeforeSubmit);
    let stop = Arc::new(AtomicBool::new(false));
    let sync_loop = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        let dm = dm_id.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = pollis_core::commands::mls::poll_mls_welcomes(&state, user.clone()).await;
                let _ = pollis_core::commands::mls::process_pending_commits(
                    &state,
                    dm.clone(),
                    user.clone(),
                )
                .await;
                let _ = pollis_core::commands::messages::get_dm_messages(
                    user.clone(),
                    dm.clone(),
                    Some(50),
                    None,
                    &state,
                )
                .await;
                tokio::time::sleep(Duration::from_millis(40)).await;
            }
        })
    };
    let refresh_loop = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        let dm = dm_id.clone();
        let stop = stop.clone();
        tokio::spawn(async move {
            while !stop.load(Ordering::Relaxed) {
                let _ = pollis_core::commands::messages::get_dm_messages(
                    user.clone(),
                    dm.clone(),
                    Some(50),
                    None,
                    &state,
                )
                .await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
    };

    // Bob's sync tick lands in the window: no Welcome, GroupInfo at epoch 0, a
    // membership row — the gate picks ExternalJoin. He parks with the branch
    // built and the CAS unsent.
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("bob never reached the external-join submit point")
        .expect("rendezvous closed");

    // Half two: the creator's reconcile adds Bob at epoch 0 → 1 and writes his
    // Welcome — it wins epoch 0 because Bob's CAS has not been sent.
    reconcile_group_mls_impl(&alice.state, &dm_id, &alice_profile.id)
        .await
        .expect("reconcile");
    // Bob's UI refresh loop (ingest → unlocked welcome poll) gets to see the
    // Welcome while his external join is still parked under the group lock —
    // the overlap `pollis-tui` produces on every accept.
    tokio::time::sleep(Duration::from_millis(500)).await;
    // Bob's CAS now loses; his retry finds the Welcome and defers to it.
    rendezvous::disarm(rendezvous::Point::ExternalJoinBeforeSubmit);
    let _ = release.send(());

    // Bob accepts through the same core call the TUI's `a` key makes.
    bob.accept_dm_request(&dm_id).await;

    // The creator sends. This is the message CI never surfaced.
    alice.send_channel_message(&dm_id, "PING_ACROSS_THE_UI").await;

    // Bob's loops must surface it on their own, exactly as the TUI's do.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut bob_view: Vec<String> = Vec::new();
    while tokio::time::Instant::now() < deadline {
        bob_view = bob
            .fetch_dm_messages(&dm_id)
            .await
            .iter()
            .filter_map(|m| m["content"].as_str().map(str::to_string))
            .collect();
        if bob_view.contains(&"PING_ACROSS_THE_UI".to_string()) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop.store(true, Ordering::Relaxed);
    let _ = sync_loop.await;
    let _ = refresh_loop.await;
    if !bob_view.contains(&"PING_ACROSS_THE_UI".to_string()) {
        // Same idea as `messages::diagnose_admin_delete_failure`: a bare
        // "bob's view is []" says nothing about WHICH link broke, and this test
        // is a race, so a failure is likely to be seen on CI and not
        // reproducible locally. Classify it while the state still exists.
        let diag = diagnose_missing_delivery(&bob_profile.id, &dm_id).await;
        panic!(
            "bob lost the creator's message after losing the epoch-0 race: \
             {bob_view:?}\n{diag}"
        );
    }
    assert_cursor_never_passes_an_unprocessed_envelope(&bob, &bob_profile.id, &alice, &dm_id).await;

    // And back: the DM is usable in both directions.
    bob.send_channel_message(&dm_id, "PONG_BACK_ACROSS_UI").await;
    let alice_view: Vec<String> = alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        alice_view.contains(&"PONG_BACK_ACROSS_UI".to_string()),
        "alice did not get bob's reply: {alice_view:?}"
    );

    drop(alice);
    drop(bob);
}

/// The DM rows plus the creator's half of the MLS bootstrap (a single-member
/// group and its GroupInfo at epoch 0) — the window before reconcile adds the
/// recipient, in which a recipient with no Welcome external-joins. The
/// reconcile half is left to the test so it can be interleaved.
async fn dm_with_only_the_creators_group(
    alice: &TestClient,
    alice_id: &str,
    bob_id: &str,
) -> String {
    use pollis_api::DsRequest;
    let dm_id = ulid::Ulid::new().to_string();
    let body = pollis_api::profile::CreateDmBody {
        id: dm_id.clone(),
        creator_id: alice_id.to_string(),
        member_ids: vec![alice_id.to_string(), bob_id.to_string()],
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    let status = crate::harness::signed_post_status(
        alice,
        <pollis_api::profile::CreateDmBody as DsRequest>::PATH,
        &serde_json::to_vec(&body).expect("serialize CreateDmBody"),
    )
    .await;
    assert_eq!(status, 200, "create_dm rows");
    pollis_lib::commands::mls::init_mls_group(&alice.state, &dm_id, alice_id)
        .await
        .expect("init_mls_group");
    dm_id
}

/// Poll `cond` until it holds, failing the test after 30s. For waiting on a
/// state another task produces — never for papering over an ordering.
async fn wait_until<F, Fut>(what: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
    while !cond().await {
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

async fn holds_local_group(client: &TestClient, conversation_id: &str) -> bool {
    let guard = client.state.local_db.lock().await;
    guard
        .as_ref()
        .is_some_and(|db| pollis_lib::commands::mls::has_local_group(db.conn(), conversation_id))
}

async fn welcomes_acknowledged(recipient_id: &str, conversation_id: &str) -> bool {
    let remote = crate::harness::writable_log().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_welcome \
             WHERE recipient_id = ?1 AND conversation_id = ?2 AND delivered = 1",
            libsql::params![recipient_id.to_string(), conversation_id.to_string()],
        )
        .await
        .expect("welcome query");
    rows.next()
        .await
        .ok()
        .flatten()
        .and_then(|r| r.get::<i64>(0).ok())
        .unwrap_or(0)
        > 0
}

/// The delivery invariant (#1220), checked against the real tables: a
/// recipient's cursor never sits at or above a message it did not ingest.
///
/// The messages are the sender's own sends (her local `message` rows carry the
/// envelope ids), which excludes the control frames — receipts — that ride in
/// `message` envelopes but never become rows on either side. The recipient was
/// a member before every one of them was sealed, so no acceptable loss
/// excuses any: each envelope at or below the recipient's cursor that carries
/// one of them must be in the recipient's local `message` table. A cursor past
/// one that is not there means no fetch will select it again — it is gone.
async fn assert_cursor_never_passes_an_unprocessed_envelope(
    recipient: &TestClient,
    recipient_id: &str,
    sender: &TestClient,
    conversation_id: &str,
) {
    let remote = crate::harness::writable_remote().await;
    let conn = remote.conn().await.expect("remote conn");
    let mut cursor: Option<i64> = None;
    let mut rows = conn
        .query(
            "SELECT MAX(last_seq) FROM conversation_watermark \
             WHERE conversation_id = ?1 AND user_id = ?2",
            libsql::params![conversation_id.to_string(), recipient_id.to_string()],
        )
        .await
        .expect("watermark query");
    if let Ok(Some(r)) = rows.next().await {
        cursor = r.get::<Option<i64>>(0).unwrap_or(None);
    }
    let cursor = cursor.expect("the recipient reported a cursor");
    let mut passed: Vec<String> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT id FROM message_envelope \
             WHERE conversation_id = ?1 AND type = 'message' AND seq <= ?2 ORDER BY seq",
            libsql::params![conversation_id.to_string(), cursor],
        )
        .await
        .expect("envelope query");
    while let Ok(Some(r)) = rows.next().await {
        passed.push(r.get::<String>(0).expect("envelope id"));
    }
    let has_row = |client: &TestClient, id: &str| {
        let id = id.to_string();
        let state = client.state.clone();
        async move {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().expect("signed in");
            db.conn()
                .query_row("SELECT 1 FROM message WHERE id = ?1", [id.as_str()], |_| Ok(()))
                .is_ok()
        }
    };
    let mut sent = 0;
    let mut missing: Vec<String> = Vec::new();
    for id in &passed {
        if !has_row(sender, id).await {
            continue;
        }
        sent += 1;
        if !has_row(recipient, id).await {
            missing.push(id.clone());
        }
    }
    assert!(sent > 0, "precondition: the cursor covers at least one of the sender's messages");
    assert!(
        missing.is_empty(),
        "the cursor (seq {cursor}) passed message envelope(s) this device never ingested: {missing:?}"
    );
}

/// #1220, ordering A — the one the real-speed suite hit on every run. The
/// recipient external-joins in the pre-reconcile window, loses epoch 0 to the
/// creator's Add, drops its branch, and — before the retry — its Welcome lands
/// (the welcome poll does not take the conversation lock). The retry used to
/// external-join anyway: `build_external_commit` deleted the confirmed Welcome
/// group at epoch 1 and claimed epoch 1 for a fresh branch. The creator's
/// message, sealed at epoch 1 in between, was then undecryptable, and the
/// cursor moved past it.
///
/// Both windows are rendezvous points, so the ordering is forced at any build
/// speed.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn retry_after_a_lost_epoch_zero_race_keeps_the_welcome_that_landed_meanwhile() {
    use pollis_lib::commands::mls::reconcile_group_mls_impl;
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    use std::time::Duration;

    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let dm_id = dm_with_only_the_creators_group(&alice, &alice_profile.id, &bob_profile.id).await;

    let mut before_submit = rendezvous::arm(Point::ExternalJoinBeforeSubmit);
    let mut after_lost = rendezvous::arm(Point::ExternalJoinAfterLostRace);

    // Bob, with no group and no Welcome, external-joins.
    let join = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        let dm = dm_id.clone();
        tokio::spawn(async move {
            pollis_core::commands::mls::process_pending_commits(&state, dm, user).await
        })
    };
    let release = tokio::time::timeout(Duration::from_secs(30), before_submit.recv())
        .await
        .expect("bob never built his external-join branch")
        .expect("rendezvous closed");

    // Alice's Add wins epoch 0 and writes Bob's Welcome; Bob's CAS then loses.
    reconcile_group_mls_impl(&alice.state, &dm_id, &alice_profile.id)
        .await
        .expect("reconcile");
    rendezvous::disarm(Point::ExternalJoinBeforeSubmit);
    let _ = release.send(());
    let release = tokio::time::timeout(Duration::from_secs(30), after_lost.recv())
        .await
        .expect("bob's external join never lost its race")
        .expect("rendezvous closed");
    rendezvous::disarm(Point::ExternalJoinAfterLostRace);

    // Between the lost race and the retry, Bob's Welcome lands. The poll then
    // waits for the conversation lock (its post-join self-update) until the
    // join is released below.
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await })
    };
    wait_until("bob's welcome to be applied", || holds_local_group(&bob, &dm_id)).await;

    // Alice seals at epoch 1 — the epoch only Bob's Welcome group can open.
    alice.send_channel_message(&dm_id, "SEALED_AT_THE_WELCOME_EPOCH").await;

    // The retry runs now, with the Welcome group stored.
    let _ = release.send(());
    let _ = join.await.expect("join task");
    let _ = poll.await.expect("poll task");

    bob.accept_dm_request(&dm_id).await;
    let bob_view: Vec<String> = bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    if !bob_view.contains(&"SEALED_AT_THE_WELCOME_EPOCH".to_string()) {
        let diag = diagnose_missing_delivery(&bob_profile.id, &dm_id).await;
        panic!("bob lost the message sealed at his Welcome's epoch: {bob_view:?}\n{diag}");
    }
    assert_cursor_never_passes_an_unprocessed_envelope(&bob, &bob_profile.id, &alice, &dm_id).await;

    // And the DM works both ways afterwards.
    bob.send_channel_message(&dm_id, "PONG").await;
    assert!(alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .any(|m| m["content"] == "PONG"));

    drop(alice);
    drop(bob);
}

/// #1220, ordering B. The same race, but Bob's Welcome is polled while his
/// doomed branch is still STORED (built at epoch 1 on GroupInfo 0, CAS not yet
/// sent). `join_from_welcome` used to see "a local group at epoch 1, a Welcome
/// at epoch 1", refuse it as a replay — and the poll acknowledges every
/// outcome, so the Welcome was gone. The lost CAS then dropped the branch and
/// the retry external-joined at epoch 1 → 2, past the creator's message sealed
/// at epoch 1. An unconfirmed branch now yields to a Welcome at its own epoch,
/// and the lost CAS drops only a branch, never the group that replaced it.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn welcome_polled_while_a_doomed_join_branch_is_stored_replaces_the_branch() {
    use pollis_lib::commands::mls::reconcile_group_mls_impl;
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    use std::time::Duration;

    wipe().await;
    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    let dm_id = dm_with_only_the_creators_group(&alice, &alice_profile.id, &bob_profile.id).await;

    let mut before_submit = rendezvous::arm(Point::ExternalJoinBeforeSubmit);
    let join = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        let dm = dm_id.clone();
        tokio::spawn(async move {
            pollis_core::commands::mls::process_pending_commits(&state, dm, user).await
        })
    };
    let release = tokio::time::timeout(Duration::from_secs(30), before_submit.recv())
        .await
        .expect("bob never built his external-join branch")
        .expect("rendezvous closed");
    assert!(holds_local_group(&bob, &dm_id).await, "precondition: the branch is stored");

    reconcile_group_mls_impl(&alice.state, &dm_id, &alice_profile.id)
        .await
        .expect("reconcile");

    // Bob's welcome poll runs against the stored branch and acknowledges the
    // Welcome whatever it decides.
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await })
    };
    wait_until("bob's welcome to be acknowledged", || {
        welcomes_acknowledged(&bob_profile.id, &dm_id)
    })
    .await;

    // Alice seals at epoch 1 while Bob's CAS is still unsent.
    alice.send_channel_message(&dm_id, "SEALED_WHILE_THE_BRANCH_WAS_STORED").await;

    rendezvous::disarm(Point::ExternalJoinBeforeSubmit);
    let _ = release.send(());
    let _ = join.await.expect("join task");
    let _ = poll.await.expect("poll task");

    bob.accept_dm_request(&dm_id).await;
    let bob_view: Vec<String> = bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    if !bob_view.contains(&"SEALED_WHILE_THE_BRANCH_WAS_STORED".to_string()) {
        let diag = diagnose_missing_delivery(&bob_profile.id, &dm_id).await;
        panic!(
            "bob lost the message sealed while his doomed branch was stored: {bob_view:?}\n{diag}"
        );
    }
    assert_cursor_never_passes_an_unprocessed_envelope(&bob, &bob_profile.id, &alice, &dm_id).await;

    bob.send_channel_message(&dm_id, "PONG").await;
    assert!(alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .any(|m| m["content"] == "PONG"));

    drop(alice);
    drop(bob);
}

/// The loss CI actually produced (#1041): a message sealed at the epoch a
/// committer is about to leave.
///
/// A new member's post-join self-update runs an ingesting catch-up and THEN
/// commits. The two are not one step: an envelope the creator seals at the
/// current epoch and posts after the catch-up's fetch but before the commit
/// merges is decryptable by nobody who has merged that commit
/// (`max_past_epochs = 0`). On the DM-accept path the committer is the
/// recipient, the commit is guaranteed, and the creator is typing — so the
/// first message of the DM is the one that lands in the window.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn message_sealed_at_the_epoch_a_committer_leaves_is_still_delivered() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    use std::time::Duration;

    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Alice's Add lands Bob's Welcome at epoch 1.
    let dm_id = alice.create_dm(&[&bob_profile.id]).await;

    // Bob applies the Welcome, catches up (nothing to ingest) and stages his
    // post-join self-update — parking under the group lock with the commit
    // built and unsent.
    let mut parked = rendezvous::arm(Point::SelfUpdateBeforeSubmit);
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(async move {
            pollis_core::commands::mls::poll_mls_welcomes(&state, user).await
        })
    };
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("bob never staged his post-join self-update")
        .expect("rendezvous closed");

    // Alice, at epoch 1 with no commit to catch up on, seals at epoch 1.
    alice.send_channel_message(&dm_id, "PING_ACROSS_THE_UI").await;

    // Bob's commit lands (head is still 1) and merges: epoch 2.
    rendezvous::disarm(Point::SelfUpdateBeforeSubmit);
    let _ = release.send(());
    poll.await.expect("poll task").expect("poll_mls_welcomes");
    bob.accept_dm_request(&dm_id).await;

    let bob_view: Vec<String> = bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        bob_view.contains(&"PING_ACROSS_THE_UI".to_string()),
        "bob lost the message sealed at the epoch his self-update left: {bob_view:?}"
    );

    // Alice reads Bob's commit and the DM keeps working both ways.
    bob.send_channel_message(&dm_id, "PONG_BACK_ACROSS_UI").await;
    let alice_view: Vec<String> = alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        alice_view.contains(&"PONG_BACK_ACROSS_UI".to_string()),
        "alice did not get bob's reply: {alice_view:?}"
    );

    drop(alice);
    drop(bob);
}

/// #1041, the SENDER-side arm of the same race. Alice catches up, seals at the
/// head epoch, and then — before her post reaches the DS — Bob's commit lands
/// and moves the head. Her envelope is now sealed at an epoch every member has
/// left. The DS refuses it (`epoch_behind`) and she catches up, re-seals at the
/// new epoch, and posts again; Bob reads it. Deterministic: Alice parks at
/// `EnvelopeBeforePost` while Bob's self-update runs to completion.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn message_sealed_behind_a_landed_commit_is_resealed_and_delivered() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    use std::time::Duration;

    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&[&bob_profile.id]).await;
    // Bob joins fully (Welcome + post-join self-update) and both sides settle.
    bob.accept_dm_request(&dm_id).await;
    alice.send_channel_message(&dm_id, "settle").await;
    assert!(bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .any(|m| m["content"] == "settle"));

    // Alice's send catches up and seals, then parks with the envelope unposted.
    let mut parked = rendezvous::arm(Point::EnvelopeBeforePost);
    let send = {
        let state = alice.state.clone();
        let user = alice.user_id().to_string();
        let dm = dm_id.clone();
        tokio::spawn(async move {
            pollis_core::commands::messages::send_message(
                dm,
                user,
                "PING_BEHIND_THE_HEAD".to_string(),
                None,
                None,
                Some("alice".to_string()),
                &state,
            )
            .await
        })
    };
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("alice never reached the post")
        .expect("rendezvous closed");

    // Bob's commit lands and merges while Alice's envelope is sealed at the
    // epoch it closes.
    assert!(bob.self_update(&dm_id).await, "bob's self-update must land");

    rendezvous::disarm(Point::EnvelopeBeforePost);
    let _ = release.send(());
    let sent = send.await.expect("send task").expect("send_message");
    assert_eq!(sent.content.as_deref(), Some("PING_BEHIND_THE_HEAD"));

    // The envelope Bob fetches must be the RE-SEALED one — at his epoch.
    let bob_view: Vec<String> = bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        bob_view.contains(&"PING_BEHIND_THE_HEAD".to_string()),
        "bob lost the message alice sealed behind his commit: {bob_view:?}"
    );

    // Alice's own copy carries the stamp that actually landed, so her local
    // history and the envelope agree.
    let alice_view = alice.fetch_dm_messages(&dm_id).await;
    let own = alice_view
        .iter()
        .find(|m| m["content"] == "PING_BEHIND_THE_HEAD")
        .expect("alice keeps her own message");
    assert_eq!(own["sent_at"].as_str(), Some(sent.sent_at.as_str()));

    drop(alice);
    drop(bob);
}

/// #1079: nothing that merely wants to ENCRYPT may advance this device's epoch.
///
/// `load_group_with_signer` used to `merge_pending_commit` unconditionally, so
/// every caller that only wanted a group got an epoch advance as a side effect.
/// `try_mls_encrypt` is one such caller, and it is reached from
/// `receipts::emit_receipt`, which by design takes no MLS lock and runs at the
/// end of every DM catch-up as well as from `mark_messages_read`. A committer
/// holds a staged commit across its DS round-trip, so a receipt landing in that
/// window merged it:
///
/// * **lost race** — the device sat at a phantom epoch on a branch the log never
///   held, past `invariants::resolve`, until some later replay classified it as
///   `ForkedTree` and external-joined;
/// * **won race** — `sweep_before_merge` opened its decryptor at the
///   already-advanced lineage and skipped every envelope sealed at the closing
///   epoch. With `max_past_epochs = 0` those are simply gone. That is the
///   committer-arm loss #1041 exists to prevent.
///
/// Deterministic: Bob parks at `SelfUpdateBeforeSubmit` with his post-join
/// commit staged and unsent, and the receipt is driven at him there.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_read_receipt_cannot_advance_the_epoch_of_a_parked_committer() {
    use pollis_lib::commands::mls::rendezvous::{self, Point};
    use std::time::Duration;

    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // Alice's Add lands Bob's Welcome at epoch 1, and a first message so Bob has
    // something of hers to acknowledge.
    let dm_id = alice.create_dm(&[&bob_profile.id]).await;
    alice.send_channel_message(&dm_id, "READ_ME").await;

    // Bob applies the Welcome, ingests, and stages his post-join self-update —
    // parking under the group lock with the commit built and unsent.
    let mut parked = rendezvous::arm(Point::SelfUpdateBeforeSubmit);
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(
            async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await },
        )
    };
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("bob never staged his post-join self-update")
        .expect("rendezvous closed");

    // Bob is mid-commit: staged, undecided.
    let (epoch_parked, pending_parked) =
        pollis_lib::commands::mls::local_epoch_and_pending(&bob.state, &dm_id)
            .await
            .expect("bob has a local group while parked");
    assert!(
        pending_parked,
        "the rendezvous must park bob with a staged commit, or this test proves nothing"
    );

    // The message ids come from ALICE, not from Bob. Bob is parked holding the
    // per-conversation MLS lock, and his own fetch would run an ingest that wants
    // it — the test would deadlock rather than assert anything. The ids are the
    // DS's, so they are the same on both sides, and Bob already ingested the
    // message during the catch-up that precedes the park.
    let read_ids: Vec<String> = alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    assert!(!read_ids.is_empty(), "bob must have alice's message to acknowledge");

    // The receipt. Takes no MLS lock, on purpose — this is the call that used to
    // merge Bob's staged commit out from under him.
    pollis_core::commands::messages::mark_messages_read(
        dm_id.clone(),
        bob_profile.id.clone(),
        read_ids,
        &bob.state,
    )
    .await
    .expect("mark_messages_read is best-effort and must not fail");

    // THE ASSERTION. The epoch has not moved and the commit is still staged: the
    // log has not decided, so neither has Bob.
    let (epoch_after, pending_after) =
        pollis_lib::commands::mls::local_epoch_and_pending(&bob.state, &dm_id)
            .await
            .expect("bob still has a local group");
    assert_eq!(
        epoch_after, epoch_parked,
        "a read receipt advanced a parked committer's epoch — it merged a commit the log had \
         not decided (#1079)"
    );
    assert!(
        pending_after,
        "a read receipt resolved a parked committer's staged commit (#1079)"
    );

    // Alice, still at the pre-commit epoch, seals a message there — exactly the
    // envelope the won-race arm used to lose.
    alice.send_channel_message(&dm_id, "SEALED_AT_THE_CLOSING_EPOCH").await;

    // Release: Bob's commit lands and merges, and his pre-merge sweep must pick
    // up the envelope sealed at the epoch he is leaving.
    rendezvous::disarm(Point::SelfUpdateBeforeSubmit);
    let _ = release.send(());
    poll.await.expect("poll task").expect("poll_mls_welcomes");
    bob.accept_dm_request(&dm_id).await;

    let bob_view: Vec<String> = bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        bob_view.contains(&"SEALED_AT_THE_CLOSING_EPOCH".to_string()),
        "bob lost the message sealed at the epoch his commit left: {bob_view:?}"
    );

    // And the DM still works both ways afterwards.
    bob.send_channel_message(&dm_id, "PONG_1079").await;
    let alice_view: Vec<String> = alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect();
    assert!(
        alice_view.contains(&"PONG_1079".to_string()),
        "alice did not get bob's reply: {alice_view:?}"
    );

    let _ = alice_profile;
    drop(alice);
    drop(bob);
}

/// Why an envelope did not reach a device, read straight off the DS tables.
///
/// Three links can break and they look identical from the recipient's empty
/// view: the envelope was never stored; it WAS stored but sits at or below the
/// device's cursor so no fetch selects it; or it was fetched and could not be
/// decrypted. The first two are visible in the DS's own rows, so report them
/// rather than leaving a future CI failure to be guessed at.
async fn diagnose_missing_delivery(user_id: &str, conversation_id: &str) -> String {
    let remote = crate::harness::writable_remote().await;
    let Ok(conn) = remote.conn().await else {
        return "  diagnosis: could not open the remote connection".to_string();
    };

    let mut envelopes: Vec<String> = Vec::new();
    if let Ok(mut rows) = conn
        .query(
            "SELECT id, type, seq, sent_at FROM message_envelope \
             WHERE conversation_id = ?1 ORDER BY seq",
            libsql::params![conversation_id.to_string()],
        )
        .await
    {
        while let Ok(Some(r)) = rows.next().await {
            envelopes.push(format!(
                "    id={} type={} seq={:?} sent_at={}",
                r.get::<String>(0).unwrap_or_default(),
                r.get::<String>(1).unwrap_or_default(),
                r.get::<Option<i64>>(2).unwrap_or(None),
                r.get::<String>(3).unwrap_or_default(),
            ));
        }
    }

    let mut cursors: Vec<String> = Vec::new();
    if let Ok(mut rows) = conn
        .query(
            "SELECT device_id, last_seq, last_fetched_at FROM conversation_watermark \
             WHERE conversation_id = ?1 AND user_id = ?2",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await
    {
        while let Ok(Some(r)) = rows.next().await {
            cursors.push(format!(
                "    device={} last_seq={:?} last_fetched_at={}",
                r.get::<String>(0).unwrap_or_default(),
                r.get::<Option<i64>>(1).unwrap_or(None),
                r.get::<String>(2).unwrap_or_default(),
            ));
        }
    }

    let counter: Option<i64> = match conn
        .query(
            "SELECT next_seq FROM conversation_seq WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
    {
        Ok(mut rows) => rows
            .next()
            .await
            .ok()
            .flatten()
            .and_then(|r| r.get::<i64>(0).ok()),
        Err(_) => None,
    };

    format!(
        "  diagnosis (#1087 delivery sequence):\n  \
         conversation_seq.next_seq = {counter:?}\n  \
         envelopes ({}):\n{}\n  \
         this user's cursors ({}):\n{}\n  \
         — no envelopes at all means the send never landed; an envelope whose \
         seq is <= every cursor means the fetch could never select it (a cursor \
         seeded or advanced too far); otherwise it was fetched and not decrypted.",
        envelopes.len(),
        if envelopes.is_empty() { "    (none)".to_string() } else { envelopes.join("\n") },
        cursors.len(),
        if cursors.is_empty() { "    (none)".to_string() } else { cursors.join("\n") },
    )
}
