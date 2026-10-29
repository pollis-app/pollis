use std::sync::Arc;

use crate::harness::{
    await_nth_wake_up, await_wake_up, enroll_second_device, wake_up_count, wipe, TestClient,
};
use pollis_lib::test_harness::invoke;
use serde_json::json;
use serial_test::serial;

/// A DM is a pending request for Bob until he accepts.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_request_accept_flow() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()]).await;

    let alice_channels = alice.list_dms().await;
    assert_eq!(alice_channels.len(), 1);
    assert_eq!(alice_channels[0]["id"], dm_id);

    let bob_requests = bob.list_dm_requests().await;
    assert_eq!(bob_requests.len(), 1);
    assert_eq!(bob_requests[0]["id"], dm_id);
    assert!(bob.list_dms().await.is_empty());

    bob.accept_dm_request(&dm_id).await;

    assert!(bob.list_dm_requests().await.is_empty());
    let bob_channels = bob.list_dms().await;
    assert_eq!(bob_channels.len(), 1);
    assert_eq!(bob_channels[0]["id"], dm_id);

    drop(alice);
    drop(bob);
}

/// A block makes create_dm_channel fail with the generic BLOCK_ERR, indistinguishable to the sender.
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

    assert!(alice.list_dms().await.is_empty());
    assert!(alice.list_dm_requests().await.is_empty());
    assert!(bob.list_dms().await.is_empty());
    assert!(bob.list_dm_requests().await.is_empty());

    drop(alice);
    drop(bob);
}

/// Multi-device DM: the tree expands to all four devices; non-member carol decrypts nothing.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_multi_device_round_trip() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice_profile = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    let dm_id = alice_d1
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

    assert_eq!(bob_d1.list_dm_requests().await.len(), 1);
    bob_d1.accept_dm_request(&dm_id).await;

    // Each fetch advances that device to the current epoch.
    alice_d1.fetch_dm_messages(&dm_id).await;
    alice_d2.fetch_dm_messages(&dm_id).await;
    bob_d1.fetch_dm_messages(&dm_id).await;
    bob_d2.fetch_dm_messages(&dm_id).await;

    let mut carol = TestClient::new().await;
    let _carol_profile = carol.sign_up("carol@test.local").await;

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

/// Rejecting removes EVERY device of the rejecter from the tree; a fresh DM then converges.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn dm_invite_reject_removes_from_tree_then_re_invite_converges() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;

    let mut bob_d1 = TestClient::new().await;
    let bob_profile = bob_d1.sign_up("bob@test.local").await;
    let bob_d2 = enroll_second_device(&bob_d1, "bob@test.local").await;

    // Both bob devices land in the tree at creation.
    let rejected_dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;

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

    // The member row is per-user, so both bob devices lose membership.
    bob_d1.leave_dm(&rejected_dm_id).await;

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

    alice.process_commits_for(&rejected_dm_id).await;
    alice
        .send_channel_message(&rejected_dm_id, "alice post-reject")
        .await;

    // Their leaves are gone: any envelope row decrypts to None.
    for (client, label) in [(&bob_d1, "bob_d1"), (&bob_d2, "bob_d2")] {
        let msgs = client.fetch_dm_messages(&rejected_dm_id).await;
        let contents: Vec<&str> =
            msgs.iter().filter_map(|m| m["content"].as_str()).collect();
        assert!(
            !contents.contains(&"alice post-reject"),
            "{label} must NOT decrypt 'alice post-reject' after reject, got: {contents:?}"
        );
    }

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

    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    assert_ne!(
        dm_id, rejected_dm_id,
        "second create_dm_channel must produce a distinct dm_channel.id"
    );

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

    bob_d1.accept_dm_request(&dm_id).await;

    alice.fetch_dm_messages(&dm_id).await;
    bob_d1.fetch_dm_messages(&dm_id).await;
    bob_d2.fetch_dm_messages(&dm_id).await;

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

    // The rejected DM must not appear on bob's devices; alice's ghost row must not touch the new DM.
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

/// Raw `user_block` row count, bypassing the block-filtering commands.
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

/// Block lifecycle: hidden on alice's side only, creation refused, then restored by unblock.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn user_block_lifecycle() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let remote = crate::harness::writable_remote().await;

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

    let dm_id = alice
        .create_dm(&[alice_profile.id.as_str(), bob_profile.id.as_str()])
        .await;
    let bob_requests = bob.list_dm_requests().await;
    assert!(
        bob_requests.iter().any(|c| c["id"] == dm_id),
        "bob should see alice's DM as a pending request pre-block"
    );

    alice.block(&bob_profile.id).await;
    assert_eq!(
        user_block_count(&remote, &alice_profile.id, &bob_profile.id).await,
        1,
        "block_user should insert a user_block row"
    );

    // block_user nulls alice's accepted_at; list_dm_requests filters blocked peers.
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

    // The block is invisible to the blocked party by design.
    let bob_requests_after_block = bob.list_dm_requests().await;
    assert!(
        bob_requests_after_block.iter().any(|c| c["id"] == dm_id),
        "post-block: bob must continue to see the DM on his side, got: {bob_requests_after_block:?}"
    );

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

    alice.unblock(&bob_profile.id).await;
    assert_eq!(
        user_block_count(&remote, &alice_profile.id, &bob_profile.id).await,
        0,
        "unblock_user should remove the user_block row"
    );

    // accepted_at was nulled, so it may resurface as a request or a DM.
    let alice_dms_ids: Vec<serde_json::Value> = alice.list_dms().await;
    let alice_req_ids: Vec<serde_json::Value> = alice.list_dm_requests().await;
    let in_dms = alice_dms_ids.iter().any(|c| c["id"] == dm_id);
    let in_reqs = alice_req_ids.iter().any(|c| c["id"] == dm_id);
    assert!(
        in_dms || in_reqs,
        "post-unblock: alice should see the original DM again in either list, \
         dms={alice_dms_ids:?} reqs={alice_req_ids:?}"
    );

    // A fresh peer, so the lingering alice↔bob DM doesn't tangle.
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

/// #1041: the recipient's epoch-0 external join loses to the creator's Add and must defer to the Welcome.
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

    // The reconcile half is driven by hand so the recipient can be interleaved.
    let dm_id = dm_with_only_the_creators_group(&alice, &alice_profile.id, &bob_profile.id).await;

    // A sync loop and a UI refresh loop, as in the TUI.
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

    // Bob parks with the external-join branch built and the CAS unsent.
    let release = tokio::time::timeout(Duration::from_secs(30), parked.recv())
        .await
        .expect("bob never reached the external-join submit point")
        .expect("rendezvous closed");

    reconcile_group_mls_impl(&alice.state, &dm_id, &alice_profile.id)
        .await
        .expect("reconcile");
    // Bob's UI loop sees the Welcome while his join is parked, as in `pollis-tui`.
    tokio::time::sleep(Duration::from_millis(500)).await;
    rendezvous::disarm(rendezvous::Point::ExternalJoinBeforeSubmit);
    let _ = release.send(());

    bob.accept_dm_request(&dm_id).await;

    alice.send_channel_message(&dm_id, "PING_ACROSS_THE_UI").await;

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
        // A race, likely seen only on CI: classify the failure while the state exists.
        let diag = diagnose_missing_delivery(&bob_profile.id, &dm_id).await;
        panic!(
            "bob lost the creator's message after losing the epoch-0 race: \
             {bob_view:?}\n{diag}"
        );
    }
    assert_cursor_never_passes_an_unprocessed_envelope(&bob, &bob_profile.id, &alice, &dm_id).await;

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

/// DM rows and the creator's epoch-0 group, before reconcile adds the recipient.
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

/// Poll `cond` until it holds (30s); never to paper over an ordering.
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

/// #1220: a recipient's cursor never sits at or above a sender message it did not ingest.
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

/// #1220 ordering A: a Welcome landing between the lost CAS and the retry must not be clobbered.
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

    // Bob's Welcome lands; the poll waits on the conversation lock.
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await })
    };
    wait_until("bob's welcome to be applied", || holds_local_group(&bob, &dm_id)).await;

    alice.send_channel_message(&dm_id, "SEALED_AT_THE_WELCOME_EPOCH").await;

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

    bob.send_channel_message(&dm_id, "PONG").await;
    assert!(alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .any(|m| m["content"] == "PONG"));

    drop(alice);
    drop(bob);
}

/// #1220 ordering B: an unconfirmed branch yields to a Welcome at its own epoch.
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

    // The poll acknowledges the Welcome whatever it decides.
    let poll = {
        let state = bob.state.clone();
        let user = bob_profile.id.clone();
        tokio::spawn(async move { pollis_core::commands::mls::poll_mls_welcomes(&state, user).await })
    };
    wait_until("bob's welcome to be acknowledged", || {
        welcomes_acknowledged(&bob_profile.id, &dm_id)
    })
    .await;

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

/// #1041: a message sealed between a committer's catch-up and its merge must still be delivered.
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

    let dm_id = alice.create_dm(&[&bob_profile.id]).await;

    // Bob parks with his post-join self-update built and unsent.
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

    alice.send_channel_message(&dm_id, "PING_ACROSS_THE_UI").await;

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

/// #1041 sender side: a post refused as `epoch_behind` is re-sealed and posted again.
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
    bob.accept_dm_request(&dm_id).await;
    alice.send_channel_message(&dm_id, "settle").await;
    assert!(bob
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .any(|m| m["content"] == "settle"));

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

    assert!(bob.self_update(&dm_id).await, "bob's self-update must land");

    rendezvous::disarm(Point::EnvelopeBeforePost);
    let _ = release.send(());
    let sent = send.await.expect("send task").expect("send_message");
    assert_eq!(sent.content.as_deref(), Some("PING_BEHIND_THE_HEAD"));

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

    // Alice's copy carries the stamp that actually landed.
    let alice_view = alice.fetch_dm_messages(&dm_id).await;
    let own = alice_view
        .iter()
        .find(|m| m["content"] == "PING_BEHIND_THE_HEAD")
        .expect("alice keeps her own message");
    assert_eq!(own["sent_at"].as_str(), Some(sent.sent_at.as_str()));

    drop(alice);
    drop(bob);
}

/// #1079: an encrypt-only caller (a receipt) must not merge a staged commit.
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

    let dm_id = alice.create_dm(&[&bob_profile.id]).await;
    alice.send_channel_message(&dm_id, "READ_ME").await;

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

    let (epoch_parked, pending_parked) =
        pollis_lib::commands::mls::local_epoch_and_pending(&bob.state, &dm_id)
            .await
            .expect("bob has a local group while parked");
    assert!(
        pending_parked,
        "the rendezvous must park bob with a staged commit, or this test proves nothing"
    );

    // Ids from Alice: Bob holds the MLS lock, so his own fetch would deadlock.
    let read_ids: Vec<String> = alice
        .fetch_dm_messages(&dm_id)
        .await
        .iter()
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    assert!(!read_ids.is_empty(), "bob must have alice's message to acknowledge");

    // Takes no MLS lock, on purpose.
    pollis_core::commands::messages::mark_messages_read(
        dm_id.clone(),
        bob_profile.id.clone(),
        read_ids,
        &bob.state,
    )
    .await
    .expect("mark_messages_read is best-effort and must not fail");

    // The epoch has not moved and the commit is still staged.
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

    // The envelope the won-race arm used to lose.
    alice.send_channel_message(&dm_id, "SEALED_AT_THE_CLOSING_EPOCH").await;

    // Bob's pre-merge sweep must pick it up.
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

/// Which link broke: never stored, or at/below the cursor (else it failed to decrypt).
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

async fn dm_contents(client: &TestClient, dm_id: &str) -> Vec<String> {
    client
        .fetch_dm_messages(dm_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_owned))
        .collect()
}

async fn dm_ids(client: &TestClient) -> Vec<String> {
    client
        .list_dms()
        .await
        .iter()
        .filter_map(|d| d["id"].as_str().map(str::to_owned))
        .collect()
}

fn dm_created_for(dm_id: &str) -> impl Fn(&serde_json::Value) -> bool + '_ {
    move |p| p["type"] == "dm_created" && p["conversation_id"] == dm_id
}

/// #1410: a DM started on one device reaches the user's other devices via their own inbox.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dm_created_on_one_device_reaches_the_creators_other_devices() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;
    assert!(alice_d2.list_dms().await.is_empty(), "alice_d2 starts with no DMs");

    let dm_id = alice_d1.create_dm(&[alice.id.as_str(), bob_profile.id.as_str()]).await;

    // No LiveKit here: accepted = 503, refused = 4xx.
    let inbox = format!("inbox-{}", alice.id);
    let wake = await_wake_up(&inbox, dm_created_for(&dm_id))
        .await
        .unwrap_or_else(|| {
            panic!("create_dm_channel must wake the creator's other devices on {inbox}")
        });
    assert!(!(400..500).contains(&wake.status), "the DS refused the wake-up: {wake:?}");

    bob.accept_dm_request(&dm_id).await;
    alice_d1.send_channel_message(&dm_id, "alice-d1 one").await;
    bob.send_channel_message(&dm_id, "bob one").await;
    alice_d1.send_channel_message(&dm_id, "alice-d1 two").await;

    assert_eq!(
        dm_ids(&alice_d2).await,
        vec![dm_id.clone()],
        "alice_d2 must list the DM alice_d1 created"
    );
    assert!(
        alice_d2.list_dm_requests().await.is_empty(),
        "the creator's own DM is never a request on their other device"
    );
    let got = dm_contents(&alice_d2, &dm_id).await;
    for want in ["alice-d1 one", "bob one", "alice-d1 two"] {
        assert!(got.iter().any(|c| c == want), "alice_d2 must read {want:?}, got {got:?}");
    }

    alice_d2.send_channel_message(&dm_id, "alice-d2 reply").await;
    for (client, label) in [(&alice_d1, "alice_d1"), (&bob, "bob")] {
        let got = dm_contents(client, &dm_id).await;
        assert!(
            got.iter().any(|c| c == "alice-d2 reply"),
            "{label} must read alice_d2's reply, got {got:?}"
        );
    }

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}

/// #1410: a request accepted on one device stops being a request on the others.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dm_accepted_on_one_device_reaches_the_accepters_other_devices() {
    wipe().await;

    let mut alice_d1 = TestClient::new().await;
    let alice = alice_d1.sign_up("alice@test.local").await;
    let alice_d2 = enroll_second_device(&alice_d1, "alice@test.local").await;
    let mut bob = TestClient::new().await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = bob.create_dm(&[bob_profile.id.as_str(), alice.id.as_str()]).await;
    let inbox = format!("inbox-{}", alice.id);
    await_wake_up(&inbox, dm_created_for(&dm_id))
        .await
        .expect("bob's request wakes alice's devices");
    assert_eq!(alice_d2.list_dm_requests().await.len(), 1, "alice_d2 sees the request");
    assert_eq!(wake_up_count(&inbox, dm_created_for(&dm_id)), 1);

    alice_d1.accept_dm_request(&dm_id).await;

    let wake = await_nth_wake_up(&inbox, dm_created_for(&dm_id), 2)
        .await
        .unwrap_or_else(|| {
            panic!("accept_dm_request must wake the accepter's other devices on {inbox}")
        });
    assert!(!(400..500).contains(&wake.status), "the DS refused the wake-up: {wake:?}");

    bob.send_channel_message(&dm_id, "bob one").await;
    alice_d1.send_channel_message(&dm_id, "alice-d1 one").await;

    assert!(
        alice_d2.list_dm_requests().await.is_empty(),
        "accepted on alice_d1, so no longer a request on alice_d2"
    );
    assert_eq!(dm_ids(&alice_d2).await, vec![dm_id.clone()], "alice_d2 must list the accepted DM");
    let got = dm_contents(&alice_d2, &dm_id).await;
    for want in ["bob one", "alice-d1 one"] {
        assert!(got.iter().any(|c| c == want), "alice_d2 must read {want:?}, got {got:?}");
    }

    drop(alice_d1);
    drop(alice_d2);
    drop(bob);
}
