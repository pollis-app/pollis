//! THE M2 GATE (verifiable core): a TUI client RECEIVES a message sent by
//! another client, decrypted end-to-end over real MLS — driven purely through
//! `pollis_tui::sync::sync_once`.
//!
//! Two clients share ONE writable main `RemoteDb` + ONE log `RemoteDb` + ONE
//! in-process `pollis-delivery`, exactly like the flows harness's shared world.
//! Each client has its OWN read-only `query_only_view` + its OWN `AppState`.
//!
//! ## Conversation type: DM (not group), and why
//!
//! The task allows a DM as the "receives a message from another client" gate if
//! it needs fewer DS routes. It does: a DM needs only `dm/create` + `dm/accept`
//! for membership, versus a group's `groups/create` + `channels/create` +
//! `invites/create` + `invites/accept`. The MLS add-to-tree + Welcome + commit +
//! message path is identical either way (it's the same reconcile-on-create /
//! send machinery), so the DM proves the same crypto round-trip with a smaller
//! DS surface.
//!
//! ## Scenario
//! 1. A signs up; B signs up (first-device signup through the DS).
//! 2. A opens a DM to B (`create_dm_channel` reconciles B into the MLS tree and
//!    queues B's Welcome).
//! 3. B accepts the pending DM request (through the DS).
//! 4. A sends "hello from A" — while B is offline.
//! 5. B is driven ONLY through `sync::sync_once` (a few rounds): welcomes →
//!    commits → read. B ends up decrypting exactly one message: "hello from A".
//! 6. Assert B never needed a direct remote write (its handle is `query_only`).

mod common;

use common::{spawn_world, TestClient};

#[tokio::test(flavor = "multi_thread")]
async fn tui_client_receives_message_from_another_client_via_sync() {
    let world = spawn_world().await;

    // ── Two clients, each its own AppState + read-only main view ──
    let mut alice = TestClient::new(&world);
    let mut bob = TestClient::new(&world);

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // ── A opens a DM to B; B sees it as a pending request ──
    let dm_id = alice.create_dm(&bob_profile.id).await;

    let requests = bob.dm_requests().await;
    assert_eq!(
        requests.len(),
        1,
        "B should see exactly one pending DM request from A"
    );
    assert_eq!(requests[0].id, dm_id, "the pending request is A's DM");

    // B accepts (routes through the DS — B's own handle is read-only).
    bob.accept_dm(&dm_id).await;

    // ── A sends while B is offline ──
    alice.send(&dm_id, "hello from A").await;

    // ── Drive B purely through the sync loop. ~4 rounds settle an interleaved
    //    welcomes→commits→read catch-up (spec §6). ──
    bob.sync_rounds(4).await;

    // ── B must now have exactly ONE decrypted message: "hello from A". ──
    let page = bob.read_dm(&dm_id).await;
    let contents: Vec<&str> = page
        .messages
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(
        contents,
        vec!["hello from A"],
        "B should have decrypted exactly A's one message, got: {:#?}",
        page.messages
    );
    // The message is attributed to A (proves it came from the other client).
    assert_eq!(
        page.messages.len(),
        1,
        "exactly one message envelope for B"
    );
    assert_eq!(
        page.messages[0].sender_id, _alice_profile.id,
        "the message's sender is A"
    );

    // ── Invariant: B routed EVERYTHING through the DS — its main handle is a
    //    read-only view, so a direct write must fail. This is what proves the
    //    receive path never reached around the DS to write Turso directly. ──
    let conn = bob
        .state
        .remote_db
        .conn()
        .await
        .expect("B main conn");
    let direct_write = conn
        .execute("CREATE TABLE _b_should_not_be_able_to_write (x)", ())
        .await;
    assert!(
        direct_write.is_err(),
        "B's main handle must reject direct writes (query_only) — everything goes through the DS"
    );
}
