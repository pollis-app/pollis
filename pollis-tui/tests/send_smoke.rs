//! THE M3 GATE (spec §11): a **full MLS round-trip, both directions**, driven
//! purely through the TUI's own `pollis_tui::send` write layer + `pollis_tui::sync`.
//!
//! Both directions, since a bug letting only the creator (or acceptor) send passes a one-way test.
//!
//! A DM for the same reason as `sync_smoke`: same MLS path, smaller DS surface.
//!
//! ## Scenario
//! 1. A and B sign up (first-device signup through the DS).
//! 2. A opens a DM to B; B accepts the pending request (both through the DS).
//! 3. A sends "ping from A" via `pollis_tui::send`; B drives `sync::sync_rounds`
//!    and decrypts exactly "ping from A", attributed to A. (the M2 direction)
//! 4. B sends "pong from B" via `pollis_tui::send`; A drives `sync::sync_rounds`
//!    and decrypts exactly "pong from B", attributed to B. (the NEW direction)
//! 5. Each side ends with BOTH messages, oldest-first in send order.

mod common;

use common::{spawn_world, TestClient};

/// Read a DM and return its messages oldest-first as `(sender_id, content)`
/// pairs; the core returns newest-first, so reverse.
async fn conversation(client: &TestClient, dm_id: &str) -> Vec<(String, String)> {
    let page = client.read_dm(dm_id).await;
    page.messages
        .iter()
        .rev()
        .map(|m| (m.sender_id.clone(), m.content.clone().unwrap_or_default()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn full_mls_round_trip_both_directions_via_send_layer() {
    let world = spawn_world().await;

    let mut alice = TestClient::new(&world);
    let mut bob = TestClient::new(&world);

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    let dm_id = alice.create_dm(&bob_profile.id).await;
    let requests = bob.dm_requests().await;
    assert_eq!(requests.len(), 1, "B should see one pending DM request from A");
    assert_eq!(requests[0].id, dm_id, "the pending request is A's DM");
    bob.accept_dm(&dm_id).await;

    alice.send_text(&dm_id, "ping from A").await;
    bob.sync_rounds(4).await;

    let bob_view = conversation(&bob, &dm_id).await;
    assert_eq!(
        bob_view,
        vec![(alice_profile.id.clone(), "ping from A".to_string())],
        "B should have decrypted exactly A's one message, got: {bob_view:#?}",
    );

    bob.send_text(&dm_id, "pong from B").await;
    alice.sync_rounds(4).await;

    let expected = vec![
        (alice_profile.id.clone(), "ping from A".to_string()),
        (bob_profile.id.clone(), "pong from B".to_string()),
    ];

    let alice_view = conversation(&alice, &dm_id).await;
    assert_eq!(
        alice_view, expected,
        "A should hold both messages in send order (A's own + B's reply), got: {alice_view:#?}",
    );

    // B stored its own send, but a round makes both ends converge identically.
    bob.sync_rounds(2).await;
    let bob_view = conversation(&bob, &dm_id).await;
    assert_eq!(
        bob_view, expected,
        "B should hold both messages in send order (A's + B's own), got: {bob_view:#?}",
    );
}
