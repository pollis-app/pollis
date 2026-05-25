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
/// MLS tree, not just the device that invoked reject. `leave_dm_channel`
/// deletes the rejecter's single `dm_channel_member` row (there is one row
/// per user, not per device) and then reconcile removes every leaf in the
/// MLS tree that belongs to that user. Bob runs two devices, rejects from
/// `bob_d1`, and neither `bob_d1` nor `bob_d2` may decrypt subsequent
/// messages sent by alice.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_invite_reject_removes_from_tree() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // Alice invites bob. Both bob devices land in the MLS tree at creation
    // time — dm.rs:127 "Reconcile then adds all members' devices".
    let dm_id = alice
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
    bob_d1.leave_dm(&dm_id).await;

    // `dm_channel_member` has NO rows for bob in this dm.
    let remote = alice.state.remote_db.clone();
    let bob_member_count: i64 = {
        let conn = remote.conn().await.expect("remote conn");
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM dm_channel_member \
                 WHERE dm_channel_id = ?1 AND user_id = ?2",
                libsql::params![dm_id.clone(), bob_profile.id.clone()],
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
    alice.process_commits_for(&dm_id).await;
    alice
        .send_channel_message(&dm_id, "alice post-reject")
        .await;

    // Neither bob device may decrypt alice's new message — their leaves
    // are gone from the tree. They may still see an envelope row on the
    // remote, but the content decrypts to None.
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let msgs = client.fetch_dm_messages(&dm_id).await;
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
            .all(|c| c["id"].as_str() != Some(dm_id.as_str())),
        "bob_d1 must not list the rejected DM as a request"
    );
    assert!(
        bob_d1
            .list_dms()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(dm_id.as_str())),
        "bob_d1 must not list the rejected DM as an accepted channel"
    );
    assert!(
        bob_d2
            .list_dm_requests()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(dm_id.as_str())),
        "bob_d2 must not list the rejected DM as a request"
    );
    assert!(
        bob_d2
            .list_dms()
            .await
            .iter()
            .all(|c| c["id"].as_str() != Some(dm_id.as_str())),
        "bob_d2 must not list the rejected DM as an accepted channel"
    );

    drop(alice);
    drop(bob_d1);
    drop(bob_d2);
}

/// After a reject, alice can open a FRESH DM with bob and both sides
/// converge again. Every bob device must re-enter the MLS tree with a
/// brand-new `dm_channel.id`, and no stale state from the rejected DM
/// may surface.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_re_invite_after_reject() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // ── Phase 1: create → reject ──
    let rejected_dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    bob_d1.leave_dm(&rejected_dm_id).await;

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
    remote: &Arc<pollis_lib::db::remote::RemoteDb>,
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

    let remote = alice.state.remote_db.clone();

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
