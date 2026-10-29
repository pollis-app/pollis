//! THE M2 GATE (verifiable core): a TUI client RECEIVES a message sent by
//! another client, decrypted end-to-end over real MLS — driven purely through
//! `pollis_tui::sync::sync_once`.
//!
//! Two clients share one in-process DS; neither holds a database handle (#987).
//!
//! ## Conversation type: DM (not group), and why
//!
//! A DM proves the same MLS round trip as a group with a smaller DS surface.
//!
//! ## Scenario
//! 1. A signs up; B signs up (first-device signup through the DS).
//! 2. A opens a DM to B (`create_dm_channel` reconciles B into the MLS tree and
//!    queues B's Welcome).
//! 3. B accepts the pending DM request (through the DS).
//! 4. A sends "hello from A" — while B is offline.
//! 5. B is driven ONLY through `sync::sync_once` (a few rounds): welcomes →
//!    commits → read. B ends up decrypting exactly one message: "hello from A".

mod common;

use common::{spawn_world, TestClient};

#[tokio::test(flavor = "multi_thread")]
async fn tui_client_receives_message_from_another_client_via_sync() {
    let world = spawn_world().await;

    let mut alice = TestClient::new(&world);
    let mut bob = TestClient::new(&world);

    let _alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&bob_profile.id).await;

    let requests = bob.dm_requests().await;
    assert_eq!(
        requests.len(),
        1,
        "B should see exactly one pending DM request from A"
    );
    assert_eq!(requests[0].id, dm_id, "the pending request is A's DM");

    bob.accept_dm(&dm_id).await;

    alice.send(&dm_id, "hello from A").await;

    // ~4 rounds settle an interleaved catch-up (spec §6).
    bob.sync_rounds(4).await;

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
    assert_eq!(
        page.messages.len(),
        1,
        "exactly one message envelope for B"
    );
    assert_eq!(
        page.messages[0].sender_id, _alice_profile.id,
        "the message's sender is A"
    );
}
