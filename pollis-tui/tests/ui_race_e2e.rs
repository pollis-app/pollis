//! The `ui_e2e` scenario forced into the ordering the flake was first blamed
//! on (#1041). ONE test per binary, like every other file in this directory:
//! the client's data dir is process-global (`set_data_dir`), so two tests in
//! one binary — which cargo runs on parallel threads — clobber each other's
//! `accounts.json` mid-signup ("no active user; call verify_otp first").

mod common;

use std::time::Duration;

use common::{spawn_world, Driver};
use crossterm::event::{KeyCode, KeyModifiers};

/// Hang guards, not latency assertions — see `ui_e2e.rs`.
const SURFACE_TIMEOUT: Duration = Duration::from_secs(120);
const SIDEBAR_TIMEOUT: Duration = Duration::from_secs(60);

/// The `ui_e2e` scenario, forced into the ordering the flake was first blamed
/// on (#1041): B's sync loop external-joins onto A's epoch-0 GroupInfo BEFORE A's
/// reconcile adds B, and A's Add wins the epoch. Locally B nearly always wins
/// that race, so without the rendezvous this ordering was only ever seen on
/// CI. A lost join converges (it defers to the Welcome); the loss itself was
/// the committer window pinned by `dms::message_sealed_*` in the flows suite.
#[tokio::test(flavor = "multi_thread")]
async fn message_surfaces_when_the_recipient_loses_the_epoch_zero_race() {
    use pollis_core::commands::mls::rendezvous::{self, Point};

    let world = spawn_world().await;
    let mut alice = Driver::new(&world, "alice-device");
    let mut bob = Driver::new(&world, "bob-device");
    alice.signup_dev("alice@e2e.local").await;
    bob.signup_dev("bob@e2e.local").await;

    // A's reconcile parks at entry (after `init_mls_group` published the epoch-0
    // GroupInfo); B's external join parks with its commit built and unsent.
    let mut reconcile_parked = rendezvous::arm(Point::ReconcileEntry);
    let mut join_parked = rendezvous::arm(Point::ExternalJoinBeforeSubmit);

    let alice_state = alice.state();
    let alice_id = alice.user_id();
    let bob_id = bob.user_id();
    let create = tokio::spawn(async move {
        pollis_core::commands::dm::create_dm_channel(
            alice_id.clone(),
            vec![alice_id, bob_id],
            &alice_state,
        )
        .await
        .expect("create_dm_channel")
        .id
    });
    let release_reconcile = tokio::time::timeout(SIDEBAR_TIMEOUT, reconcile_parked.recv())
        .await
        .expect("alice never reached reconcile")
        .expect("rendezvous closed");
    // B's sync loop sees the membership row + GroupInfo and no Welcome, so it
    // external-joins; pump B's UI meanwhile as the real app would.
    let release_join = loop {
        bob.refresh().await;
        match tokio::time::timeout(Duration::from_millis(50), join_parked.recv()).await {
            Ok(r) => break r.expect("rendezvous closed"),
            Err(_) => continue,
        }
    };
    // A's Add lands first ...
    rendezvous::disarm(Point::ReconcileEntry);
    let _ = release_reconcile.send(());
    let _dm_id = create.await.expect("create task");
    // ... and B's CAS loses.
    rendezvous::disarm(Point::ExternalJoinBeforeSubmit);
    let _ = release_join.send(());

    bob.wait_for("pending", SIDEBAR_TIMEOUT).await;
    bob.press(KeyCode::Char('a'), KeyModifiers::NONE).await;
    bob.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;

    alice.wait_for("Direct Messages", SIDEBAR_TIMEOUT).await;
    alice.enter().await;
    alice.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    alice.send_keys("PING_ACROSS_THE_UI").await;
    alice.enter().await;

    bob.enter().await;
    bob.wait_for("PING_ACROSS_THE_UI", SURFACE_TIMEOUT).await;

    bob.press(KeyCode::Char('i'), KeyModifiers::NONE).await;
    bob.send_keys("PONG_BACK_ACROSS_UI").await;
    bob.enter().await;
    alice.wait_for("PONG_BACK_ACROSS_UI", SURFACE_TIMEOUT).await;
}
