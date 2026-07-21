//! THE UI e2e GATE: a headless, in-process, two-client scenario that drives the
//! REAL ratatui state machine (keystrokes → `App::on_key` → `App::run` →
//! `ui::render`) against a `TestBackend`, and proves a message typed on one
//! client's UI surfaces on the OTHER client's RENDERED screen.
//!
//! This closes the gap the `*_smoke.rs` tests leave open: they exercise only the
//! library core (`auth`/`data`/`send`/`sync`/`enroll`) and never touch
//! `app.rs`/`ui.rs`. Here the send AND the receive-surfacing both go through the
//! real UI, and every assertion is on visible RENDERED text (`buffer_text`), not
//! model state — that is what makes this a UI e2e rather than another core smoke.
//!
//! ## What goes through the real UI vs. the core
//! - Through the UI: signup (Email → OTP → PIN screens), opening the DM,
//!   composing + sending a message, and the receive-surfacing (Refresh → render).
//! - Through the core command layer: establishing DM MEMBERSHIP (create + accept).
//!   Wiring the full start-DM prompt + accept handshake through the UI is
//!   disproportionately fiddly for a first test (it needs username resolution and
//!   a synced pending-request row on the peer); the smokes establish membership
//!   the same way. The SEND + RECEIVE path — the point of this test — is 100% UI.
//!
//! Determinism: no fixed unconditional sleep is the mechanism of correctness. The
//! background sync loop runs at a short cadence and surfacing is asserted with the
//! bounded poll-until-visible `Driver::wait_for`.

mod common;

use std::time::Duration;

use common::{spawn_world, Driver};
use crossterm::event::{KeyCode, KeyModifiers};

/// Message pane surfacing can need several MLS sync rounds (welcome + commit +
/// ingest), so give it a generous bound; `wait_for` returns as soon as the text
/// appears, so a healthy run finishes well under this.
const SURFACE_TIMEOUT: Duration = Duration::from_secs(25);
/// The DM row appearing in the sidebar is a single sync round away.
const SIDEBAR_TIMEOUT: Duration = Duration::from_secs(15);

#[tokio::test(flavor = "multi_thread")]
async fn message_typed_on_one_ui_surfaces_on_the_others_rendered_screen() {
    let world = spawn_world().await;

    // Two headless UI clients, each its own AppState/keystore against the shared
    // in-process DS.
    let mut alice = Driver::new(&world, "alice-device");
    let mut bob = Driver::new(&world, "bob-device");

    // ── Sign both up through the REAL Email → OTP → PIN screens ──
    alice.signup_dev("alice@e2e.local").await;
    bob.signup_dev("bob@e2e.local").await;

    // ── Establish the 1:1 DM. A creates it through the core layer (the DS path);
    //    B accepts it through the REAL UI. The UI accept (`a` → Action::AcceptDm →
    //    send::accept_dm) does the MLS membership work that the bare core
    //    `dm::accept_dm_request` skips — without it B's conversation stays a
    //    DmRequest and the UI gates compose ("Accept this request first"). ──
    let _dm_id = alice.create_dm(&bob.user_id()).await;

    // B: the pending request syncs into the sidebar under "Requests" (row
    // "@ alice (pending)"). Its row is the only selectable one on a fresh account,
    // so `a` accepts it; then it graduates to a compose-able "Direct Messages" row.
    bob.wait_for("pending", SIDEBAR_TIMEOUT).await;
    bob.press(KeyCode::Char('a'), KeyModifiers::NONE).await;
    // The DM graduates from a pending "Requests" row to a compose-able "Direct
    // Messages" row once B has joined the MLS group and the accept has synced.
    // NOTE: this is the reproducer for the DM-accept convergence race — with the
    // background sync loop running, B's inbound Welcome (`apply_welcome`, which
    // holds only the local_db lock) races B's external-join recovery
    // (`process_pending_commits` → `external_join_group`, which holds the
    // per-conversation `mls_group_lock`); the unsynchronised "delete stale group
    // → rejoin" in both can strand B. Once that core race is fixed this is a
    // reliable UI e2e; until then it is expected to be flaky.
    bob.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;

    // ── Direction 1: A opens the DM, composes, and sends — all through the UI ──
    // Wait for the accepted-DM section to render (not just the peer's name, which
    // also appears on a pending "Requests" row): the DM only moves under "Direct
    // Messages" once B's accept has synced, so this waits out any stale snapshot
    // and guarantees the row opens as an accepted DM (compose-able), not a request.
    alice.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;
    // Enter on the sidebar opens the highlighted DM (selection defaults onto it,
    // headers being unselectable).
    alice.enter().await;
    // `i` enters compose; type the message; Enter sends it (Action::SendMessage).
    alice.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    alice.send_keys("PING_ACROSS_THE_UI").await;
    alice.enter().await;
    // A's own rendered message pane shows the just-sent message.
    assert!(
        alice.buffer_text().contains("PING_ACROSS_THE_UI"),
        "A's own rendered pane should show the message it just sent, buffer:\n{}",
        alice.buffer_text()
    );

    // B opens the now-accepted DM and the message surfaces on B's RENDERED screen
    // — driven purely by the background sync loop + Refresh.
    bob.enter().await;
    bob.wait_for("PING_ACROSS_THE_UI", SURFACE_TIMEOUT).await;

    // ── Direction 2 (prove it isn't one-directional): B replies, A sees it ──
    bob.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    bob.send_keys("PONG_BACK_ACROSS_UI").await;
    bob.enter().await;
    assert!(
        bob.buffer_text().contains("PONG_BACK_ACROSS_UI"),
        "B's own rendered pane should show its reply, buffer:\n{}",
        bob.buffer_text()
    );

    alice.wait_for("PONG_BACK_ACROSS_UI", SURFACE_TIMEOUT).await;

    // ── Both directions surfaced on the RENDERED screen. Final sanity: A's pane
    //    holds BOTH messages at once. ──
    let alice_final = alice.buffer_text();
    assert!(
        alice_final.contains("PING_ACROSS_THE_UI") && alice_final.contains("PONG_BACK_ACROSS_UI"),
        "A's rendered pane should hold both messages, buffer:\n{alice_final}",
    );

    // ── Invariant (mirrors the smokes): every write went through the DS. Each
    //    client's main handle is a read-only view, so a direct write MUST fail —
    //    proving neither the UI's send nor receive path reached around the DS. ──
    for (label, state) in [("A", alice.state()), ("B", bob.state())] {
        let conn = state.remote_db.conn().await.expect("main conn");
        let direct_write = conn.execute("CREATE TABLE _should_not_write (x)", ()).await;
        assert!(
            direct_write.is_err(),
            "{label}'s main handle must reject direct writes (query_only) — all writes go through the DS",
        );
    }
}
