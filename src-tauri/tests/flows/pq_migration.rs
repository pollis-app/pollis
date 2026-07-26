//! #454 P4 + P5 — a live conversation crossing the classic→hybrid suite
//! boundary, end to end through the real command pipeline.
//!
//! MLS binds the ciphersuite at group creation and RFC 9420 offers no way to
//! change it, so "going post-quantum" is not an operation on a group: it stands
//! up a **successor group** in [`CS_HYBRID`] for the same conversation and moves
//! the roster across by Welcome (`pollis-core/src/commands/mls/migrate.rs`). The
//! commit log's monotone key is therefore `(conversation, generation, epoch)`,
//! and the successor restarts at epoch 0 of generation 1.
//!
//! Two things have to be true at once for that to be shippable rather than
//! merely correct, and these scenarios are split along exactly that line:
//!
//! * **Nobody is stranded.** A hybrid group can only ever admit hybrid
//!   KeyPackages, so a device that has not upgraded has no way into one and no
//!   way back — reconcile skips it rather than downgrade the group, which is the
//!   right call and also a permanent lockout. So going hybrid is gated twice: on
//!   the conversation's own roster, and on the whole live fleet (#454 P5).
//!   Scenarios S2 and S3 pin each gate independently.
//! * **Nothing is lost, and the boundary actually buys something.** A member who
//!   was offline across the migration must still read every message from before
//!   it (S2), a member with no successor Welcome must still recover into the new
//!   lineage (S6), and an adversary holding pre-migration key material must be
//!   evicted at the boundary — with the successor's Welcome structurally proving
//!   ML-KEM was what moved the group secrets across (S4).
//!
//! S5, the marathon fuzzer across the boundary, lives in `flows/model.rs` with
//! the rest of the model-based suite.

use crate::harness::{
    arm_ds_fault, backdate_last_seen, downgrade_to_classic_only, ds_group_info_suite,
    ds_head_generation, local_generation_of, max_commit_bytes, steal_leaf, upgrade_to_hybrid,
    welcome_blobs, wipe, DsFault, TestClient,
};
use serial_test::serial;

/// `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` — the classic suite every
/// group runs today.
const SUITE_CLASSIC: u16 = 0x0001;
/// `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` — the PQ hybrid suite.
const SUITE_HYBRID: u16 = 0x004D;

/// X25519 HPKE encapsulation: one ephemeral public key.
const KEM_OUTPUT_X25519: usize = 32;
/// X-Wing encapsulation: an ML-KEM-768 ciphertext (1088) plus the X25519
/// ephemeral (32). Seeing this length in a Welcome is the structural proof that
/// the group secrets crossing the boundary were ML-KEM-encapsulated, not merely
/// labelled as such.
const KEM_OUTPUT_XWING: usize = 1120;

/// Ceiling for a single commit on the hybrid suite, in bytes.
///
/// A hybrid commit runs roughly 8× its classic counterpart (every HPKE
/// encapsulation in the tree carries an ML-KEM-768 ciphertext), so the number is
/// large — but it must still be a *number*, because the failure this guards is
/// commits that grow without bound. Matches the 12 KiB ceiling
/// `hybrid_payloads_stay_under_their_ceilings` pins for an 8-member merged
/// commit in `pollis-core`'s unit suite.
const MAX_HYBRID_COMMIT_BYTES: usize = 12_288;

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

// ─── S1 — a complete fleet births hybrid groups outright ─────────────────────

/// **Invalid state it attacks:** a fully-upgraded fleet that still creates
/// classic groups and then has to migrate every one of them — paying the
/// successor-lineage cost forever for a transition that is already over.
///
/// With every live device hybrid-capable, a brand-new group is born on the
/// hybrid suite at **generation 0** and never migrates at all. The assertions are
/// on server-visible bytes — the published GroupInfo's ciphersuite and the
/// Welcome's `kem_output` length — so this proves ML-KEM actually carried the
/// group secrets to bob, not that a client believes it did.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_complete_fleet_births_hybrid_groups() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    let group_id = alice.create_group("BornHybrid").await;
    let channel_id = alice.general_channel_id(&group_id).await;

    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_HYBRID,
        "with the whole fleet hybrid-capable, a new group must be born hybrid"
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
    assert_eq!(welcome_suite, SUITE_HYBRID, "bob's Welcome must be hybrid");
    assert_eq!(
        kem_output, KEM_OUTPUT_XWING,
        "a hybrid Welcome's kem_output must be an ML-KEM-768 ciphertext (1088) plus the \
         X25519 ephemeral (32) — {kem_output} bytes means the key exchange is not hybrid"
    );

    // Traffic works in both directions on the hybrid suite.
    alice.send_channel_message(&channel_id, "hybrid-a-to-b").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"hybrid-a-to-b".to_string()),
        "bob must decrypt alice's message on the hybrid suite"
    );

    bob.send_channel_message(&channel_id, "hybrid-b-to-a").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"hybrid-b-to-a".to_string()),
        "alice must decrypt bob's message on the hybrid suite"
    );

    // Membership churn on the hybrid suite: carol joins, reads, and is removed.
    // A hybrid add draws from carol's X-Wing KeyPackage pool and a hybrid remove
    // re-keys her copath under X-Wing — both are the paths that would surface a
    // provider-routing mistake as a hard failure rather than a silent downgrade.
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
    alice.send_channel_message(&channel_id, "hybrid-with-carol").await;
    carol.sweep().await;
    assert!(
        contents(&carol, &channel_id).await.contains(&"hybrid-with-carol".to_string()),
        "carol must decrypt on the hybrid suite after joining it"
    );

    alice.remove_member(&group_id, &carol_p.id).await;
    alice.sweep().await;
    bob.sweep().await;
    alice.send_channel_message(&channel_id, "hybrid-after-carol").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"hybrid-after-carol".to_string()),
        "the survivors must keep reading after a hybrid remove re-keys the tree"
    );
    assert!(
        !contents(&carol, &channel_id).await.contains(&"hybrid-after-carol".to_string()),
        "a removed member must not decrypt traffic sealed after its removal"
    );

    // (The voice-key path — `export_secret` agreeing byte-for-byte across the two
    // providers on the hybrid suite — is proved in `pollis-core`'s MLS unit tests;
    // `voice_e2ee` is media-gated and cannot build in the headless harness.)

    // Born hybrid means never migrated: still generation 0, one lineage.
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "a group born hybrid must never open a successor lineage"
    );
    assert!(
        max_commit_bytes(&group_id).await <= MAX_HYBRID_COMMIT_BYTES,
        "hybrid commits must stay under the {MAX_HYBRID_COMMIT_BYTES}-byte budget, got {}",
        max_commit_bytes(&group_id).await
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── S2 — the FLEET gate, and a group spanning the boundary ──────────────────

/// **Invalid state it attacks:** two of them, and they are the same bug seen from
/// each side.
///
/// First, a group that goes hybrid while a classic-only device still exists
/// anywhere in the fleet. That device is one invite away from a permanent
/// lockout — it has no hybrid KeyPackage, so reconcile can only skip it, and
/// rebuilding the group classic to let it in is precisely the downgrade the
/// suite routing exists to prevent. Carol is never on this group's roster, so
/// only the fleet-wide gate can hold the migration back.
///
/// Second, a member who was offline across the migration losing the messages
/// sent before it. The successor lineage restarts at epoch 0 with fresh keys and
/// `max_past_epochs = 0` discards the predecessor's ratchet the moment the group
/// advances, so "drain the old lineage completely, then adopt" is the only order
/// that works — bob asserts it by reading pre-boundary and post-boundary traffic
/// in one pass.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_fleet_gate_holds_until_the_last_classic_device_upgrades() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    // Carol runs a pre-PQ build. She is a bystander — never on this group's
    // roster — so she can only be blocking the migration through the FLEET gate.
    downgrade_to_classic_only(&carol_p.id).await;

    let group_id = alice.create_group("SpansTheBoundary").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_CLASSIC,
        "one classic-only device in the fleet must keep new groups classic"
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

    // Alice's roster is fully capable, but carol is not — so no migration.
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "the fleet gate must hold the migration back while carol is classic-only, \
         even though this group's own roster is fully hybrid-capable"
    );
    assert_eq!(ds_group_info_suite(&group_id).await, SUITE_CLASSIC);

    // Carol updates her app. The fleet is complete; the gate opens.
    upgrade_to_hybrid(&carol).await;
    alice.sweep().await;

    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "with the fleet complete, the next sweep must open the successor lineage"
    );
    assert_eq!(
        ds_group_info_suite(&group_id).await,
        SUITE_HYBRID,
        "the published GroupInfo must now describe the hybrid successor"
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
        max_commit_bytes(&group_id).await <= MAX_HYBRID_COMMIT_BYTES,
        "commits must stay under the {MAX_HYBRID_COMMIT_BYTES}-byte budget across the \
         boundary, got {}",
        max_commit_bytes(&group_id).await
    );

    drop(alice);
    drop(bob);
    drop(carol);
}

// ─── S3 — the ROSTER gate, isolated from the fleet gate ──────────────────────

/// **Invalid state it attacks:** a group that migrates out from under one of its
/// own roster. Dave is on this conversation's desired roster with no hybrid
/// KeyPackage, so a successor would be built without him and he would never get
/// in — reconcile only ever *skips* a device it cannot add at the group's suite.
///
/// The two gates normally fail together — a live classic-only device breaks the
/// fleet count as well as any roster it is on — so this scenario prises them
/// apart by putting dave *past the dormancy window*. The fleet gate then excludes
/// him and is satisfied; only the roster gate is left holding the migration, and
/// it must hold it alone.
///
/// Dave is a pending invitee rather than a joined member for a mundane reason:
/// `desired_roster_user_ids` counts pending invites (they are given a leaf at
/// invite time so they can join unattended), and a client that never runs is the
/// only way the harness can hold a device classic-only. Every `TestClient` is
/// today's binary, so the moment one sweeps, its replenish republishes the hybrid
/// pool and the DS flips `pq_capable` straight back on.
///
/// Then dave updates, and the migration must fire and carry the group across with
/// bob's history intact.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn the_roster_gate_holds_the_migration_on_its_own() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut dave = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let dave_p = dave.sign_up("dave@test.local").await;

    // Dave is the deployment's last pre-PQ device. While he is live the fleet gate
    // is shut, so the group is born classic and nothing migrates.
    downgrade_to_classic_only(&dave_p.id).await;

    let group_id = alice.create_group("RosterGate").await;
    let channel_id = alice.general_channel_id(&group_id).await;
    assert_eq!(ds_group_info_suite(&group_id).await, SUITE_CLASSIC);

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

    alice.send_channel_message(&channel_id, "classic-era").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"classic-era".to_string()),
        "bob must be a working member of the classic group first"
    );

    // Put dave on THIS conversation's roster. He never accepts, so his client
    // never runs and he stays classic-only — but reconcile has already given his
    // device a leaf in the classic tree, and a successor built now would leave it
    // behind.
    alice.invite(&group_id, &dave_p.username).await;

    // Push dave past the dormancy window: the FLEET gate now ignores him
    // entirely, so if the migration fires here it can only be because the roster
    // gate is not doing its job.
    backdate_last_seen(&dave_p.id, 200).await;
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        0,
        "the roster gate must hold the migration back on its own — dave is on this \
         roster with no hybrid KeyPackage, so a successor would strand him"
    );

    // Dave updates his app. Both gates now pass.
    upgrade_to_hybrid(&dave).await;
    alice.sweep().await;
    assert_eq!(
        ds_head_generation(&group_id).await,
        1,
        "once every roster device can take hybrid, the migration must fire"
    );

    // Bob crosses with his history and keeps working.
    alice.send_channel_message(&channel_id, "hybrid-era").await;
    bob.poll().await;
    bob.sweep().await;
    let bobs = contents(&bob, &channel_id).await;
    assert!(
        bobs.contains(&"classic-era".to_string()) && bobs.contains(&"hybrid-era".to_string()),
        "bob must keep his classic-era history and read hybrid-era traffic, got: {bobs:?}"
    );
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    // And dave — the device the gate was protecting — can still take the invite up
    // and land in the successor, which is the whole point of having waited.
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
        "the device the roster gate held the migration for must end up inside the \
         successor, not stranded outside it"
    );

    drop(alice);
    drop(bob);
    drop(dave);
}

// ─── S4 — the boundary evicts a pre-migration compromise ─────────────────────

/// **Invalid state it attacks:** a migration that is cosmetic — the group is
/// relabelled hybrid but the successor's key material is derivable from the
/// predecessor's, so an adversary holding a stolen leaf keeps reading straight
/// through the transition.
///
/// The attacker exfiltrates bob's entire MLS state while the group is classic,
/// and is *proved* to be a real compromise by reading a pre-boundary message.
/// The migration then stands up a successor whose group secrets are distributed
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

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    // Carol holds the fleet gate shut so the group starts classic.
    downgrade_to_classic_only(&carol_p.id).await;

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
    upgrade_to_hybrid(&carol).await;
    alice.sweep().await;
    assert_eq!(ds_head_generation(&group_id).await, 1, "the migration must land");
    bob.poll().await;
    bob.sweep().await;
    assert_eq!(local_generation_of(&bob, &group_id).await, 1);

    // The successor's Welcome is what moved the roster across. It must be hybrid,
    // and its encapsulation must actually be ML-KEM.
    let (welcome_suite, kem_output) = newest_welcome_shape(&group_id).await;
    assert_eq!(
        welcome_suite, SUITE_HYBRID,
        "the successor's Welcome must be on the hybrid suite"
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
    drop(carol);
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
/// This is also where the design doc's "does the cross-signing cert survive the
/// KEM change" question gets answered, and it is answered by consequence rather
/// than by a separate assertion: an externally-joined device whose Ed25519 cert
/// failed `verify_added_devices` is removed by the next reconcile, so alice could
/// not then decrypt bob's message and bob would not appear exactly once on the
/// roster. Both are asserted below. The signature path is untouched by #454 —
/// only the KEM changed — and this is the scenario that would notice if it were.
#[tokio::test(flavor = "multi_thread")]
#[serial]
async fn a_dropped_successor_welcome_recovers_by_external_join() {
    wipe().await;

    let mut alice = TestClient::new().await;
    let mut bob = TestClient::new().await;
    let mut carol = TestClient::new().await;
    alice.sign_up("alice@test.local").await;
    let bob_p = bob.sign_up("bob@test.local").await;
    let carol_p = carol.sign_up("carol@test.local").await;

    downgrade_to_classic_only(&carol_p.id).await;

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

    alice.send_channel_message(&channel_id, "classic-era").await;
    bob.sweep().await;

    // Open the fleet gate, then lose bob's successor Welcome on the way out.
    upgrade_to_hybrid(&carol).await;
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
    alice.send_channel_message(&channel_id, "hybrid-era").await;
    bob.sweep().await;
    assert!(
        contents(&bob, &channel_id).await.contains(&"hybrid-era".to_string()),
        "bob must decrypt post-recovery traffic on the successor lineage"
    );

    bob.send_channel_message(&channel_id, "bob-recovered").await;
    alice.sweep().await;
    assert!(
        contents(&alice, &channel_id).await.contains(&"bob-recovered".to_string()),
        "alice must decrypt bob's message — a duplicate leaf would have forked the successor"
    );

    // He kept his classic-era history through the recovery.
    assert!(
        contents(&bob, &channel_id).await.contains(&"classic-era".to_string()),
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
    drop(carol);
}
