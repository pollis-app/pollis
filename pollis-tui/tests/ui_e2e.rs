//! THE UI e2e GATE: a headless, in-process, two-client scenario that drives the
//! REAL ratatui state machine (keystrokes → `App::on_key` → `App::run` →
//! `ui::render`) against a `TestBackend`, and proves a message typed on one
//! client's UI surfaces on the OTHER client's RENDERED screen.
//!
//! Send and receive both go through the real UI and every assertion is on rendered text; DM
//! membership is set up through the core layer, as in the smokes.

mod common;

use std::time::Duration;

use common::{spawn_world, Driver};
use crossterm::event::{KeyCode, KeyModifiers};

// Hang guards, not latency bounds: `wait_for` returns as soon as the text appears; tighter
// bounds only flaked on busy machines (#923).

/// Message pane surfacing can need several MLS sync rounds (welcome + commit +
/// ingest).
const SURFACE_TIMEOUT: Duration = Duration::from_secs(120);
/// The DM row appearing in the sidebar is a single sync round away.
const SIDEBAR_TIMEOUT: Duration = Duration::from_secs(60);

#[tokio::test(flavor = "multi_thread")]
async fn message_typed_on_one_ui_surfaces_on_the_others_rendered_screen() {
    let world = spawn_world().await;

    let mut alice = Driver::new(&world, "alice-device");
    let mut bob = Driver::new(&world, "bob-device");

    alice.signup_dev("alice@e2e.local").await;
    bob.signup_dev("bob@e2e.local").await;

    // A creates the DM through the core layer; B accepts through the real UI, which does the
    // MLS membership work the bare core accept skips.
    let _dm_id = alice.create_dm(&bob.user_id()).await;

    // B accepts the pending request, its only selectable row.
    bob.wait_for("pending", SIDEBAR_TIMEOUT).await;
    bob.press(KeyCode::Char('a'), KeyModifiers::NONE).await;
    // Graduates to "Direct Messages" once B joined and the accept synced; this is where #1041
    // lost A's first message.
    bob.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;

    // Wait for the accepted-DM section, not just the peer's name, so the row opens as an
    // accepted DM.
    alice.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;
    alice.enter().await;
    alice.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    alice.send_keys("PING_ACROSS_THE_UI").await;
    alice.enter().await;
    // The text alone proves nothing (a failed send leaves it in the composer); "Send failed"
    // would show.
    let alice_after_send = alice.buffer_text();
    assert!(
        alice_after_send.contains("PING_ACROSS_THE_UI")
            && !alice_after_send.contains("Send failed"),
        "A's own rendered pane should show the message it just sent, buffer:\n{alice_after_send}"
    );

    bob.enter().await;
    bob.wait_for("PING_ACROSS_THE_UI", SURFACE_TIMEOUT).await;

    bob.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    bob.send_keys("PONG_BACK_ACROSS_UI").await;
    bob.enter().await;
    assert!(
        bob.buffer_text().contains("PONG_BACK_ACROSS_UI"),
        "B's own rendered pane should show its reply, buffer:\n{}",
        bob.buffer_text()
    );

    alice.wait_for("PONG_BACK_ACROSS_UI", SURFACE_TIMEOUT).await;

    let alice_final = alice.buffer_text();
    assert!(
        alice_final.contains("PING_ACROSS_THE_UI") && alice_final.contains("PONG_BACK_ACROSS_UI"),
        "A's rendered pane should hold both messages, buffer:\n{alice_final}",
    );
}
