//! #454 P4 + #669 — a live conversation crossing a ciphersuite boundary, end to
//! end through the real command pipeline.
//!
//! MLS binds the ciphersuite at group creation and RFC 9420 offers no way to
//! change it, so moving a conversation to another suite is not an operation on
//! the group: it stands up a **successor group** in the current suite and moves
//! the roster across by Welcome (`pollis-core/src/commands/mls/migrate.rs`). The
//! commit log's monotone key is therefore `(conversation, generation, epoch)`,
//! and the successor restarts at epoch 0 of generation 1.
//!
//! ## Why these scenarios still exist, and what they now simulate
//!
//! #454 wrote them for the classic→hybrid transition and #669 finished it: there
//! is one suite now, so nothing in production is waiting to migrate and two of
//! the original scenarios (the fleet gate and the `pq_capable` roster gate) test
//! machinery that no longer exists. What survives is the part that is *not*
//! about the classic suite at all — `CS_PQ`'s code point is provisional,
//! `draft-ietf-mls-pq-ciphersuites` has renumbered once already, and a renumber
//! is a wire break for every existing group. Migration is what carries them
//! across, and the day it runs it has to be right first time.
//!
//! So the trigger is simulated the way production would experience it: the
//! harness pins the *current suite* to `SUITE_LEGACY` before the fleet signs up
//! (a world running on the old code point), then clears the pin (the new build
//! ships) and lets each client's own `ensure_mls_key_package` and sweep carry it
//! across. Nothing about the migration path is stubbed — only the calendar.
//!
//! The four claims, one per scenario:
//!
//! * **A group already on the current suite never migrates** (S1) — there is
//!   nowhere better to go, and a spurious successor would re-Welcome the whole
//!   roster for nothing.
//! * **Nobody is stranded** (S3). A group can only admit KeyPackages in its own
//!   suite, so a successor built before every roster device has republished
//!   would leave one outside with no way in and no way back. The migration must
//!   refuse to start rather than move everyone else.
//! * **Nothing is lost across the boundary** (S2) — a member offline for the
//!   whole transition must still read everything sent before it — and a member
//!   whose successor Welcome never arrives must still recover into the new
//!   lineage (S6).
//! * **The boundary actually buys something** (S4): an adversary holding
//!   pre-migration key material is evicted at it, with the successor's Welcome
//!   structurally proving ML-KEM carried the group secrets across.
//!
//! S5, the marathon fuzzer across the boundary, lives in `flows/model.rs` with
//! the rest of the model-based suite.

use crate::harness::{
    arm_ds_fault, ds_group_info_suite, ds_head_generation, local_generation_of, max_commit_bytes,
    republish_key_packages, set_current_suite, steal_leaf, welcome_blobs, wipe, DsFault,
    TestClient, SUITE_LEGACY,
};
use serial_test::serial;

/// `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44` — the one suite
/// production ships since #669. #668 moved it from `0x004D`; the KEM is the same
/// X-Wing, the signature is now ML-DSA-44.
const SUITE_PQ: u16 = 0x0052;

/// X25519 HPKE encapsulation: one ephemeral public key.
const KEM_OUTPUT_X25519: usize = 32;
/// X-Wing encapsulation: an ML-KEM-768 ciphertext (1088) plus the X25519
/// ephemeral (32). Seeing this length in a Welcome is the structural proof that
/// the group secrets crossing the boundary were ML-KEM-encapsulated, not merely
/// labelled as such.
const KEM_OUTPUT_XWING: usize = 1120;

/// Ceiling for a single commit on the PQ suite, in bytes.
///
/// A PQ commit runs an order of magnitude above a classic one — every HPKE
/// encapsulation in the tree carries an ML-KEM-768 ciphertext, and since #668
/// every leaf carries a 1,312 B ML-DSA-44 key and every signature is 2,420 B —
/// so the number is large. But it must still be a *number*, because the failure
/// this guards is commits that grow without bound. Matches the 20 KiB ceiling
/// `pq_payloads_stay_under_their_ceilings` pins for an 8-member merged commit in
/// `pollis-core`'s unit suite.
const MAX_COMMIT_BYTES: usize = 20_480;

async fn contents(client: &TestClient, channel_id: &str) -> Vec<String> {
    client
        .fetch_channel_messages(channel_id)
        .await
        .iter()
        .filter_map(|m| m["content"].as_str().map(str::to_string))
        .collect()
}

/// Read a QUIC-style variable-length integer (RFC 9420 §2.1.3), returning
/// `(value, bytes_consumed)`.
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

/// `(ciphersuite, kem_output_len)` of the first recipient's group secrets in a
/// serialized `Welcome`.
///
/// Parsed by hand against the RFC 9420 §12.4.3.1 wire layout because openmls
/// keeps `Welcome::ciphersuite()` and `EncryptedGroupSecrets::
/// encrypted_group_secrets()` crate-private, so there is no public accessor to
/// reach the HPKE `kem_output` — and it is exactly the field that distinguishes
/// a genuinely post-quantum key exchange from a hybrid-looking label.
///
/// Layout, after the `MLSMessage` framing (`version: u16`, `wire_format: u16`):
/// `cipher_suite: u16`, `secrets<V>`, and inside the first `EncryptedGroupSecrets`
/// a `new_member<V>` KeyPackageRef followed by the `HPKECiphertext`, whose first
/// field is `kem_output<V>`.
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
    // secrets<V> — total byte length of the recipient vector.
    let (_secrets_bytes, n) = read_varint(&blob[off..]);
    off += n;
    // EncryptedGroupSecrets.new_member: an opaque KeyPackageRef.
    let (new_member_len, n) = read_varint(&blob[off..]);
    off += n + new_member_len;
    // HPKECiphertext.kem_output — the encapsulation itself.
    let (kem_output_len, _) = read_varint(&blob[off..]);
    (suite, kem_output_len)
}

/// The `(suite, kem_output_len)` of the newest Welcome the DS holds for a
/// conversation. Panics if there is none — every scenario here produces one, so
/// an empty set means the setup, not the assertion, is wrong.
async fn newest_welcome_shape(conversation_id: &str) -> (u16, usize) {
    let blobs = welcome_blobs(conversation_id).await;
    let last = blobs
        .last()
        .unwrap_or_else(|| panic!("no Welcome stored for {conversation_id}"));
    welcome_suite_and_kem_output(last)
}

/// The whole world moves onto the build's own suite: the pin comes off (the new
/// build ships) and every client republishes its pool at its next login.
///
/// Both halves are production paths. The order matters and is production's own:
/// a device that has not republished has no KeyPackage in the target suite, and
/// migration refuses to move a group with such a device on its roster.
async fn ship_the_new_build(clients: [&TestClient; 2]) {
    set_current_suite(None);
    for client in clients {
        republish_key_packages(client).await;
    }
}

// ─── S1 — a group on the current suite never migrates ────────────────────────

/// **Invalid state it attacks:** a fleet that creates groups on some other suite
/// and then has to migrate every one of them — paying the successor-lineage cost
/// forever for a transition that is already over.
///
/// With no suite pin in play this is simply today's production world: a
/// brand-new group is born on `CS_PQ` at **generation 0** and never migrates at
/// all. The assertions are on server-visible bytes — the published GroupInfo's
/// ciphersuite and the Welcome's `kem_output` length — so this proves ML-KEM
/// actually carried the group secrets to bob, not that a client believes it did.
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

    // The structural claim: bob's group secrets crossed under ML-KEM-768.
    let (welcome_suite, kem_output) = newest_welcome_shape(&group_id).await;
    assert_eq!(welcome_suite, SUITE_PQ, "bob's Welcome must be on the PQ suite");
    assert_eq!(
        kem_output, KEM_OUTPUT_XWING,
        "a PQ Welcome's kem_output must be an ML-KEM-768 ciphertext (1088) plus the \
         X25519 ephemeral (32) — {kem_output} bytes means the key exchange is not hybrid"
    );

    // Traffic works in both directions.
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

    // Membership churn: carol joins, reads, and is removed. An add draws from
    // carol's X-Wing KeyPackage pool and a remove re-keys her copath under
    // X-Wing — both are the paths that would surface a suite mistake as a hard
    // failure rather than a silent downgrade.
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

    // (The voice-key path — `export_secret` agreeing byte-for-byte across
    // devices — is proved in `pollis-core`'s MLS unit tests; `voice_e2ee` is
    // media-gated and cannot build in the headless harness.)

    // Born current means never migrated: still generation 0, one lineage.
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "a group born on the current suite must never open a successor lineage"
    );
    assert!(
        max_commit_bytes(&group_id).await <= MAX_COMMIT_BYTES,
        "commits must stay under the {MAX_COMMIT_BYTES}-byte budget, got {}",
        max_commit_bytes(&group_id).await
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── S2 — a group spanning the boundary loses nothing ────────────────────────

/// **Invalid state it attacks:** a member who was offline across the migration
/// losing the messages sent before it.
///
/// The successor lineage restarts at epoch 0 with fresh keys and
/// `max_past_epochs = 0` discards the predecessor's ratchet the moment the group
/// advances, so "drain the old lineage completely, then adopt" is the only order
/// that works. Bob asserts it by reading pre-boundary and post-boundary traffic
/// in one pass, having been offline for the entire transition — he republishes
/// his pool (his app updates), then goes dark until well after alice has moved
/// the group.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_member_offline_across_the_boundary_loses_nothing() {
    wipe().await;

    // The world runs on the old code point.
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

    // Bob reads one message, then goes offline for the rest of the transition.
    alice.send_channel_message(&channel_id, "before-bob-read").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"before-bob-read".to_string()),
        "bob must read pre-migration traffic while online"
    );
    alice.send_channel_message(&channel_id, "before-bob-missed").await;

    // Nothing has changed yet, so nothing may migrate: a spurious successor here
    // would be churn plus a re-Welcome of the whole roster.
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "a group already on the current suite must not open a successor lineage"
    );

    // The new build ships. Both clients republish at their next login; bob then
    // goes dark.
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

    // Post-boundary traffic, then bob comes back for the first time since before
    // the migration. He must land on the successor having lost nothing.
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

    // And he is a working member of it, not merely a reader of its backlog.
    bob.send_channel_message(&channel_id, "bob-on-the-new-suite").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"bob-on-the-new-suite".to_string()),
        "alice must decrypt bob's first message on the successor lineage"
    );

    assert!(
        max_commit_bytes(&group_id).await <= MAX_COMMIT_BYTES,
        "commits must stay under the {MAX_COMMIT_BYTES}-byte budget across the boundary, got {}",
        max_commit_bytes(&group_id).await
    );

    drop(alice);
    drop(bob);
}

// ─── S3 — the no-stranding gate ──────────────────────────────────────────────

/// **Invalid state it attacks:** a group that migrates out from under one of its
/// own roster. Dave is on this conversation's desired roster with no KeyPackage
/// in the target suite, so a successor built now would be built without him and
/// he would never get in — reconcile only ever *skips* a device it cannot add at
/// the group's suite, and rebuilding the successor on the retired suite to let
/// him in is the downgrade this whole mechanism exists to prevent.
///
/// This is the one gate #669 kept. #454 additionally gated on `pq_capable`, a
/// server-derived count of devices that had published a hybrid pool, both for
/// the roster and fleet-wide; those were a *predictor* of the claim below,
/// needed because a classic-only device could be invited after the migration and
/// stranded then rather than now. With one suite there is no such device, and
/// the claim itself is the direct test the flags stood in for.
///
/// Dave is a pending invitee rather than a joined member for a mundane reason:
/// `desired_roster_user_ids` counts pending invites (they are given a leaf at
/// invite time so they can join unattended), and a client that never runs is the
/// cleanest way to hold one device on the old code point while the rest of the
/// world moves.
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

    // Put dave on THIS conversation's roster. He never accepts, so his client
    // never runs and never republishes — but reconcile has already given his
    // device a leaf in the predecessor tree, and a successor built now would
    // leave it behind.
    alice.invite(&group_id, &dave_p.username).await;

    // The new build ships to alice and bob, but not to dave.
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

    // Dave updates too. Every roster device can now be carried across.
    republish_key_packages(&dave).await;
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "once every roster device has republished, the migration must fire"
    );

    // Bob crosses with his history and keeps working.
    alice.send_channel_message(&channel_id, "new-suite-era").await;
    bob.poll().await;
    bob.sweep().await;
    let bobs = contents(&bob, &channel_id).await;
    assert!(
        bobs.contains(&"old-suite-era".to_string()) && bobs.contains(&"new-suite-era".to_string()),
        "bob must keep his pre-boundary history and read post-boundary traffic, got: {bobs:?}"
    );
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    // And dave — the device the gate was protecting — can still take the invite
    // up and land in the successor, which is the whole point of having waited.
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

    drop(alice);
    drop(bob);
    drop(dave);
}

// ─── S4 — the boundary evicts a pre-migration compromise ─────────────────────

/// **Invalid state it attacks:** a migration that is cosmetic — the group is
/// relabelled onto the new suite but the successor's key material is derivable
/// from the predecessor's, so an adversary holding a stolen leaf keeps reading
/// straight through the transition.
///
/// The attacker exfiltrates bob's entire MLS state before the boundary, and is
/// *proved* to be a real compromise by reading a pre-boundary message. The
/// migration then stands up a successor whose group secrets are distributed
/// under fresh X-Wing KeyPackages that the stolen state never held — so the
/// attacker is evicted at the boundary.
///
/// The honest scope: a suite migration is forward-only. Anything the attacker
/// captured and decrypted before the boundary stays decrypted — no boundary can
/// retract that — so the migration's value is that every message *after* it is
/// ML-KEM-protected, which is what the `kem_output` assertion on the successor's
/// Welcome pins. That is also why the theft below is staged before the traffic it
/// is proved on rather than after.
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

    // Steal bob's whole MLS state — the pre-migration compromise. The theft comes
    // *before* the traffic it is proved on: the group runs `max_past_epochs = 0`,
    // so a stolen state only ever reads its own epoch forward, and a message sent
    // before the steal would be unreadable for reasons that have nothing to do
    // with the suite boundary.
    let attacker = steal_leaf(&bob).await;
    alice.send_channel_message(&channel_id, "before-the-boundary").await;
    assert!(
        attacker.can_read(&channel_id, &group_id, "before-the-boundary").await,
        "the theft must be real — an attacker that cannot read pre-boundary traffic \
         proves nothing about the boundary"
    );
    bob.sweep().await;

    // Migrate.
    ship_the_new_build([&alice, &bob]).await;
    alice.sweep().await;
    assert_eq!(ds_head_generation(&group_id).await, 1, "the migration must land");
    bob.poll().await;
    bob.sweep().await;
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    // The successor's Welcome is what moved the roster across. It must be on the
    // new suite, and its encapsulation must actually be ML-KEM.
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

    // THE headline: the stolen pre-migration state cannot follow the group across.
    assert!(
        !attacker.can_read(&channel_id, &group_id, "after-the-boundary").await,
        "a leaf stolen before the migration must not decrypt anything sent after it — \
         victim {}",
        attacker.victim()
    );

    drop(alice);
    drop(bob);
}

// ─── S6 — recovering into the successor with no Welcome ──────────────────────

/// **Invalid state it attacks:** a member whose successor Welcome is lost sitting
/// forever on a retired lineage. The predecessor accepts no further commits and
/// carries no further messages, so "wait for the Welcome" is a permanent stall
/// dressed up as patience.
///
/// The Welcome carried by the migration commit is dropped at the DS. Bob drains
/// the old lineage, sees a head generation above his own in the published
/// GroupInfo — which the DS writes in the same transaction as the commit, so it
/// is proof the successor exists — and external-joins it instead
/// (`maybe_advance_generation`, path 2).
///
/// This is also where "does the cross-signing cert survive the suite change"
/// gets answered, and it is answered by consequence rather than by a separate
/// assertion: an externally-joined device whose device cert failed
/// `verify_added_devices` is removed by the next reconcile, so alice could not
/// then decrypt bob's message and bob would not appear exactly once on the
/// roster. Both are asserted below.
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

    // Ship the new build, then lose bob's successor Welcome on the way out.
    ship_the_new_build([&alice, &bob]).await;
    arm_ds_fault(DsFault::DropWelcome);
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "the migration itself must still land — only the Welcome was dropped"
    );

    // Bob has no Welcome for the successor. He must find it anyway.
    bob.poll().await;
    bob.sweep().await;
    assert_eq!(
        local_generation_of(&bob, &group_id).await,
        1,
        "bob must external-join the successor rather than stall on the retired lineage"
    );

    // A recovered member is a working member, in both directions.
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

    // He kept his pre-boundary history through the recovery.
    assert!(
        contents(&bob, &channel_id).await.contains(&"old-suite-era".to_string()),
        "external-joining the successor must not cost bob the history he already had"
    );

    // The roster lists bob exactly once — no residue from the external join.
    let members = alice.group_member_ids(&group_id).await;
    assert_eq!(
        members.iter().filter(|m| **m == bob_p.id).count(),
        1,
        "bob must appear exactly once after recovering into the successor, got: {members:?}"
    );

    drop(alice);
    drop(bob);
}
