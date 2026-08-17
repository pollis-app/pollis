//! RESTART → UNLOCK → RESYNC gate (folds in #15; restores the DoD's
//! quit→relaunch→unlock→resync cycle that was consolidated away in M2a).
//!
//! A message a device received before a restart must STILL be readable after the
//! device quits, relaunches, unlocks with its PIN, and re-syncs — the persistence
//! contract of "the TUI is its own device" (spec §1, §4). This drives the exact
//! returning-launch path from spec §7: `auth::boot` → `auth::unlock` → sync loop.
//!
//! Single restarting client (A), one peer (B) to originate a message:
//! 1. A (file-backed keystore, so identity survives a `drop`) and B sign up.
//! 2. A opens a DM to B; B accepts; B sends "before restart".
//! 3. A syncs and reads it once (proves the pre-restart baseline).
//! 4. A's `AppState` is DROPPED and rebuilt on the SAME data dir + libsql
//!    with a FRESH `default_os_keystore` — a genuine quit→relaunch.
//! 5. `auth::boot` must report `Returning`; `auth::unlock` with A's PIN succeeds.
//! 6. A re-syncs and can STILL read "before restart" — resync after restart.

mod common;

use common::{spawn_world, TestClient, TEST_PIN};
use pollis_tui::auth::{self, Boot};

#[tokio::test(flavor = "multi_thread")]
async fn restart_then_unlock_then_resync_recovers_received_message() {
    let world = spawn_world().await;

    // A uses the file-backed keystore so its identity/session persist across the
    // `AppState` drop; B is an ordinary in-memory client (never restarts).
    let mut alice = TestClient::new_persistent(&world);
    let mut bob = TestClient::new(&world);

    let alice_profile = alice.sign_up("alice@test.local").await;
    let bob_profile = bob.sign_up("bob@test.local").await;

    // ── A opens a DM to B; B accepts and sends a message ──
    let dm_id = alice.create_dm(&bob_profile.id).await;
    let requests = bob.dm_requests().await;
    assert_eq!(requests.len(), 1, "B should see one pending DM request from A");
    bob.accept_dm(&dm_id).await;
    bob.send(&dm_id, "before restart").await;

    // ── A syncs and reads it — the pre-restart baseline ──
    alice.sync_rounds(4).await;
    let before = alice.read_dm(&dm_id).await;
    let before_contents: Vec<&str> = before
        .messages
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(
        before_contents,
        vec!["before restart"],
        "A should have B's message before restarting, got: {:#?}",
        before.messages,
    );

    // ── Quit → relaunch: drop A's AppState + keystore, rebuild on the same paths ──
    alice.restart(&world);

    // ── Returning-launch path (spec §7): boot rehydrates the session ──
    let boot = auth::boot(&alice.state).await.expect("boot after restart");
    match boot {
        Boot::Returning(profile) => {
            assert_eq!(
                profile.id, alice_profile.id,
                "boot should rehydrate A's own profile from the persisted accounts index",
            );
        }
        Boot::Fresh => panic!("boot returned Fresh after a restart — session did not persist"),
    }

    // ── Unlock with A's PIN re-opens the local SQLCipher DB ──
    auth::unlock(&alice.state, &alice_profile.id, TEST_PIN)
        .await
        .expect("unlock with A's PIN after restart");

    // ── Resync, then A can STILL read the pre-restart message ──
    alice.sync_rounds(4).await;
    let after = alice.read_dm(&dm_id).await;
    let after_contents: Vec<&str> = after
        .messages
        .iter()
        .filter_map(|m| m.content.as_deref())
        .collect();
    assert_eq!(
        after_contents,
        vec!["before restart"],
        "after restart→unlock→resync A must still read B's message, got: {:#?}",
        after.messages,
    );
    assert_eq!(
        after.messages[0].sender_id, bob_profile.id,
        "the recovered message is still attributed to B",
    );
}
