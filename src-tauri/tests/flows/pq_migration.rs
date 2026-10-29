//! #454 P4 + #669 — a live conversation crossing a ciphersuite boundary, end to
//! end through the real command pipeline.
//!
//! The suite is fixed at creation, so migration builds a successor group (generation 1) by Welcome.
//! Simulated by pinning the current suite to `SUITE_LEGACY`, then clearing the pin; nothing else is stubbed.

use crate::harness::{
    arm_ds_fault, commit_budget_violation, ds_group_info_suite, ds_head_generation,
    local_generation_of, republish_key_packages, set_current_suite, steal_leaf, welcome_blobs,
    wipe, DsFault, TestClient, SUITE_LEGACY,
};
use serial_test::serial;

/// The one production suite since #669 (moved from `0x004D` by #668).
const SUITE_PQ: u16 = 0x0052;

const KEM_OUTPUT_X25519: usize = 32;
/// X-Wing: ML-KEM-768 ciphertext (1088) + X25519 (32); this length proves ML-KEM was used.
const KEM_OUTPUT_XWING: usize = 1120;

// Commit budgets live in `harness::commit_budget_violation`.

async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

fn read_varint(b: &[u8]) -> (usize, usize) {
    match b[0] >> 6 {
        0 => ((b[0] & 0x3f) as usize, 1),
        1 => ((((b[0] & 0x3f) as usize) << 8) | b[1] as usize, 2),
        2 => (
            (((b[0] & 0x3f) as usize) << 24)
                | ((b[1] as usize) << 16)
                | ((b[2] as usize) << 8)
                | b[3] as usize,
            4,
        ),
        _ => panic!("8-byte varint in a Welcome — impossible at these sizes"),
    }
}

/// Hand-parsed (RFC 9420 §12.4.3.1): openmls keeps the needed accessors crate-private.
fn welcome_suite_and_kem_output(blob: &[u8]) -> (u16, usize) {
    const MLS_VERSION_1_0: u16 = 1;
    const WIRE_FORMAT_WELCOME: u16 = 3;

    assert_eq!(
        u16::from_be_bytes([blob[0], blob[1]]),
        MLS_VERSION_1_0,
        "Welcome blob is not an MLS 1.0 message"
    );
    assert_eq!(
        u16::from_be_bytes([blob[2], blob[3]]),
        WIRE_FORMAT_WELCOME,
        "stored blob is not a Welcome"
    );

    let suite = u16::from_be_bytes([blob[4], blob[5]]);
    let mut off = 6;
    let (_secrets_bytes, n) = read_varint(&blob[off..]);
    off += n;
    let (new_member_len, n) = read_varint(&blob[off..]);
    off += n + new_member_len;
    let (kem_output_len, _) = read_varint(&blob[off..]);
    (suite, kem_output_len)
}

/// Panics if there is none: then the setup, not the assertion, is wrong.
async fn newest_welcome_shape(conversation_id: &str) -> (u16, usize) {
    let blobs = welcome_blobs(conversation_id).await;
    let last = blobs
        .last()
        .unwrap_or_else(|| panic!("no Welcome stored for {conversation_id}"));
    welcome_suite_and_kem_output(last)
}

/// Unpin, then republish: migration refuses a roster device with no target-suite KeyPackage.
async fn ship_the_new_build(clients: [&TestClient; 2]) {
    set_current_suite(None);
    for client in clients {
        republish_key_packages(client).await;
    }
}

/// S1: a group born on `CS_PQ` stays at generation 0, asserted on server-visible bytes.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_new_group_is_born_on_the_current_suite() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("BornCurrent").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_PQ,
        "since #669 every new group is born on the one suite the build ships"
    );

    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.sweep().await;

    let (welcome_suite, kem_output) = newest_welcome_shape(&group_id).await;
    assert_eq!(welcome_suite, SUITE_PQ, "bob's Welcome must be on the PQ suite");
    assert_eq!(
        kem_output, KEM_OUTPUT_XWING,
        "a PQ Welcome's kem_output must be an ML-KEM-768 ciphertext (1088) plus the \
         X25519 ephemeral (32) — {kem_output} bytes means the key exchange is not hybrid"
    );

    alice.send_channel_message(&channel_id, "pq-a-to-b").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pq-a-to-b".to_string()),
        "bob must decrypt alice's message on the PQ suite"
    );

    bob.send_channel_message(&channel_id, "pq-b-to-a").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"pq-b-to-a".to_string()),
        "alice must decrypt bob's message on the PQ suite"
    );

    // An add and a remove are where a suite mistake fails hard.
    alice.invite(&group_id, &carol_p.username).await;
    let carol_invite = carol
        .first_pending_invite()
        .await
        .expect("carol should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    carol.accept_invite(&carol_invite).await;
    carol.poll().await;
    carol.sweep().await;
    alice.send_channel_message(&channel_id, "pq-with-carol").await;
    carol.sweep().await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"pq-with-carol".to_string()),
        "carol must decrypt on the PQ suite after joining it"
    );

    alice.remove_member(&group_id, &carol_p.id).await;
    alice.sweep().await;
    bob.sweep().await;
    alice.send_channel_message(&channel_id, "pq-after-carol").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"pq-after-carol".to_string()),
        "the survivors must keep reading after a remove re-keys the tree"
    );
    assert!(
        !contents(&carol, &channel_id).await.contains(&"pq-after-carol".to_string()),
        "a removed member must not decrypt traffic sealed after its removal"
    );

    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "a group born on the current suite must never open a successor lineage"
    );
    assert!(
        commit_budget_violation(&group_id).await.is_none(),
        "commits must stay within their payload budget: {}",
        commit_budget_violation(&group_id).await.unwrap_or_default()
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

/// S2: drain the old lineage, then adopt (`max_past_epochs = 0`); offline bob loses nothing.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_offline_across_the_boundary_loses_nothing() {
    wipe().await;

    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("SpansTheBoundary").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_LEGACY,
        "a group born while the old code point is current must be on it"
    );

    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.sweep().await;

    alice.send_channel_message(&channel_id, "before-bob-read").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"before-bob-read".to_string()),
        "bob must read pre-migration traffic while online"
    );
    alice.send_channel_message(&channel_id, "before-bob-missed").await;

    // Nothing changed, so nothing may migrate.
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "a group already on the current suite must not open a successor lineage"
    );

    ship_the_new_build([&alice, &bob]).await;
    alice.sweep().await;

    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "with every roster device republished, the next sweep must open the successor lineage"
    );
    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_PQ,
        "the published GroupInfo must now describe the successor's suite"
    );
    assert_eq!(
        local_generation_of(&alice, &group_id).await,
        1,
        "the migrator must be on the lineage it opened"
    );

    alice.send_channel_message(&channel_id, "after-the-boundary").await;
    bob.poll().await;
    bob.sweep().await;

    let bobs = contents(&bob, &channel_id).await;
    for expected in [
        "before-bob-read",
        "before-bob-missed",
        "after-the-boundary",
    ] {
        assert!(
            bobs.contains(&expected.to_string()),
            "bob must still have {expected:?} after crossing the suite boundary, got: {bobs:?}"
        );
    }
    assert_eq!(
        local_generation_of(&bob, &group_id).await,
        1,
        "bob must adopt the successor lineage, not stall on the retired one"
    );

    bob.send_channel_message(&channel_id, "bob-on-the-new-suite").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"bob-on-the-new-suite".to_string()),
        "alice must decrypt bob's first message on the successor lineage"
    );

    assert!(
        commit_budget_violation(&group_id).await.is_none(),
        "commits must stay within their payload budget across the boundary: {}",
        commit_budget_violation(&group_id).await.unwrap_or_default()
    );

    drop(alice);
    drop(bob);
}

/// S3: no stranding. A pending invitee with no target-suite KeyPackage blocks migration.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_migration_waits_for_every_roster_device_to_republish() {
    wipe().await;

    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut dave = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    let group_id = alice.create_group("NoStranding").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    assert_eq!(ds_group_info_suite(&group_id).await, SUITE_LEGACY);

    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.sweep().await;

    alice.send_channel_message(&channel_id, "old-suite-era").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"old-suite-era".to_string()),
        "bob must be a working member of the predecessor group first"
    );

    // He never runs, but already holds a leaf in the predecessor tree.
    alice.invite(&group_id, &dave_p.username).await;

    ship_the_new_build([&alice, &bob]).await;
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "the migration must refuse to start — dave is on this roster with no KeyPackage \
         in the target suite, so a successor would strand him"
    );
    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_LEGACY,
        "and the group must still be on the suite it can still admit dave into"
    );

    republish_key_packages(&dave).await;
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "once every roster device has republished, the migration must fire"
    );

    alice.send_channel_message(&channel_id, "new-suite-era").await;
    bob.poll().await;
    bob.sweep().await;
    let bobs = contents(&bob, &channel_id).await;
    assert!(
        bobs.contains(&"old-suite-era".to_string()) && bobs.contains(&"new-suite-era".to_string()),
        "bob must keep his pre-boundary history and read post-boundary traffic, got: {bobs:?}"
    );
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    let dave_invite = dave
        .first_pending_invite()
        .await
        .expect("dave should still have his invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    dave.accept_invite(&dave_invite).await;
    dave.poll().await;
    dave.sweep().await;
    alice.send_channel_message(&channel_id, "dave-is-in").await;
    dave.sweep().await;
    assert!(
        contents(&dave, &channel_id).await.contains(&"dave-is-in".to_string()),
        "the device the gate held the migration for must end up inside the successor, \
         not stranded outside it"
    );

    // The only migration of >2 members, so the only place the roster-linear commit term shows.
    assert!(
        commit_budget_violation(&group_id).await.is_none(),
        "the migration opening commit must stay within the budget its adds justify: {}",
        commit_budget_violation(&group_id).await.unwrap_or_default()
    );

    drop(alice);
    drop(bob);
    drop(dave);
}

/// S4: the boundary evicts an attacker holding bob's pre-migration state.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_suite_boundary_evicts_a_stolen_pre_migration_leaf() {
    wipe().await;

    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("HarvestThenMigrate").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.sweep().await;

    // Steal before the proving traffic: `max_past_epochs = 0` reads forward only.
    let attacker = steal_leaf(&bob).await;
    alice.send_channel_message(&channel_id, "before-the-boundary").await;
    assert!(
        attacker.can_read(&channel_id, &group_id, "before-the-boundary").await,
        "the theft must be real — an attacker that cannot read pre-boundary traffic \
         proves nothing about the boundary"
    );
    bob.sweep().await;

    ship_the_new_build([&alice, &bob]).await;
    alice.sweep().await;
    assert_eq!(ds_head_generation(&group_id).await, 1, "the migration must land");
    bob.poll().await;
    bob.sweep().await;
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    let (welcome_suite, kem_output) = newest_welcome_shape(&group_id).await;
    assert_eq!(
        welcome_suite, SUITE_PQ,
        "the successor's Welcome must be on the PQ suite"
    );
    assert_eq!(
        kem_output, KEM_OUTPUT_XWING,
        "the successor's Welcome must carry an ML-KEM-768 encapsulation ({KEM_OUTPUT_XWING} \
         bytes); {KEM_OUTPUT_X25519} would mean the roster crossed under plain X25519"
    );

    alice.send_channel_message(&channel_id, "after-the-boundary").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"after-the-boundary".to_string()),
        "bob must still be reading — an eviction that also evicts the victim is not PCS"
    );

    assert!(
        !attacker.can_read(&channel_id, &group_id, "after-the-boundary").await,
        "a leaf stolen before the migration must not decrypt anything sent after it — \
         victim {}",
        attacker.victim()
    );

    drop(alice);
    drop(bob);
}

/// S6: a lost successor Welcome is recovered by external join (`maybe_advance_generation`).
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dropped_successor_welcome_recovers_by_external_join() {
    wipe().await;

    set_current_suite(Some(SUITE_LEGACY));

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;

    let group_id = alice.create_group("DroppedSuccessorWelcome").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    alice.invite(&group_id, &bob_p.username).await;
    let invite_id = bob
        .first_pending_invite()
        .await
        .expect("bob should have an invite")["id"]
        .as_str()
        .expect("invite id")
        .to_string();
    bob.accept_invite(&invite_id).await;
    bob.poll().await;
    bob.sweep().await;

    alice.send_channel_message(&channel_id, "old-suite-era").await;
    bob.sweep().await;

    ship_the_new_build([&alice, &bob]).await;
    arm_ds_fault(DsFault::DropWelcome);
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "the migration itself must still land — only the Welcome was dropped"
    );

    bob.poll().await;
    bob.sweep().await;
    assert_eq!(
        local_generation_of(&bob, &group_id).await,
        1,
        "bob must external-join the successor rather than stall on the retired lineage"
    );

    alice.sweep().await;
    alice.send_channel_message(&channel_id, "new-suite-era").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"new-suite-era".to_string()),
        "bob must decrypt post-recovery traffic on the successor lineage"
    );

    bob.send_channel_message(&channel_id, "bob-recovered").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"bob-recovered".to_string()),
        "alice must decrypt bob's message — a duplicate leaf would have forked the successor"
    );

    assert!(
        contents(&bob, &channel_id).await.contains(&"old-suite-era".to_string()),
        "external-joining the successor must not cost bob the history he already had"
    );

    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once after recovering into the successor, got: {members:?}"
    );

    drop(alice);
    drop(bob);
}
