//! Integration tests for multi-device MLS messaging.
//!
//! These tests simulate the actual user journey through Pollis:
//!   1. User creates a group (on one device)
//!   2. Creator's other device receives a Welcome
//!   3. Other users are invited and accept
//!   4. All devices send messages
//!   5. All devices decrypt each other's messages
//!
//! Each "device" is a separate in-memory SQLite DB (local MLS state) that
//! interacts through serialised Welcome/commit/ciphertext blobs — the same
//! data that flows through Turso in production.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::types::Ciphersuite;
use pollis_lib::commands::mls::{
    has_local_group, make_credential, try_mls_decrypt, try_mls_encrypt, PollisProvider,
};
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

// ── Device abstraction ──────────────────────────────────────────────────────

/// A simulated device: owns a local MLS database, a user identity, and a
/// device identity.  Mirrors the per-process state in Pollis (local SQLite +
/// keystore device_id).
struct Device {
    db: rusqlite::Connection,
    user_id: String,
    device_id: String,
}

impl Device {
    fn new(user_id: &str, device_id: &str) -> Self {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE mls_kv (
                scope TEXT NOT NULL,
                key   BLOB NOT NULL,
                value BLOB NOT NULL,
                PRIMARY KEY (scope, key)
            );",
        )
        .unwrap();
        Self {
            db,
            user_id: user_id.to_string(),
            device_id: device_id.to_string(),
        }
    }

    /// Create an MLS group with this device as sole member.
    /// Mirrors: init_mls_group in auth flow.
    fn create_group(&self, conv_id: &str) {
        let provider = PollisProvider::new(&self.db);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(&self.user_id, &self.device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        )
        .unwrap();
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let group_id = GroupId::from_slice(conv_id.as_bytes());
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CS)
            .use_ratchet_tree_extension(true)
            .build();

        MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
            .unwrap();
    }

    /// Generate a KeyPackage for this device.  Returns TLS-serialised bytes
    /// (what gets stored in `mls_key_package` in Turso).
    fn generate_key_package(&self) -> Vec<u8> {
        let provider = PollisProvider::new(&self.db);
        let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        sig_keys.store(provider.storage()).unwrap();

        let credential = make_credential(&self.user_id, &self.device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_keys.to_public_vec().into(),
            CS.signature_algorithm(),
        )
        .unwrap();
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let bundle = KeyPackage::builder()
            .build(CS, &provider, &sig_keys, cred_with_key)
            .unwrap();

        bundle.key_package().tls_serialize_detached().unwrap()
    }

    /// Add members (by their key packages) to this device's group.
    /// Returns (commit_bytes, welcome_bytes).
    /// Mirrors: add_member_mls_impl
    fn add_members(&self, conv_id: &str, kp_list: &[Vec<u8>]) -> (Vec<u8>, Vec<u8>) {
        let provider = PollisProvider::new(&self.db);
        let (mut group, signer) = load_group(&provider, conv_id);

        let validated: Vec<KeyPackage> = kp_list
            .iter()
            .map(|raw| {
                let mut reader: &[u8] = raw;
                let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
                kp_in
                    .validate(provider.crypto(), ProtocolVersion::Mls10)
                    .unwrap()
            })
            .collect();

        let (commit, welcome, _) = group.add_members(&provider, &signer, &validated).unwrap();
        group.merge_pending_commit(&provider).unwrap();

        (
            commit.tls_serialize_detached().unwrap(),
            welcome.tls_serialize_detached().unwrap(),
        )
    }

    /// Apply a Welcome message (join the group for the first time).
    /// Mirrors: apply_welcome / poll_mls_welcomes_inner
    fn apply_welcome(&self, welcome_bytes: &[u8]) {
        let provider = PollisProvider::new(&self.db);

        // Delete any stale group first (mirrors apply_welcome in production).
        let mut reader: &[u8] = welcome_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };

        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let processed = ProcessedWelcome::new_from_welcome(&provider, &join_config, welcome)
            .unwrap();
        let new_group_id = processed.unverified_group_info().group_id().clone();
        if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &new_group_id) {
            let _ = old.delete(provider.storage());
        }
        let staged = processed.into_staged_welcome(&provider, None).unwrap();
        staged.into_group(&provider).unwrap();
    }

    /// Apply a serialised commit to advance this device's epoch.
    /// Mirrors: process_pending_commits_inner
    fn apply_commit(&self, conv_id: &str, commit_bytes: &[u8]) {
        let provider = PollisProvider::new(&self.db);
        let group_id = GroupId::from_slice(conv_id.as_bytes());
        let mut group = MlsGroup::load(provider.storage(), &group_id)
            .unwrap()
            .expect("group must exist to apply commit");

        let mut reader: &[u8] = commit_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let protocol_msg = msg_in.try_into_protocol_message().unwrap();
        let processed = group.process_message(&provider, protocol_msg).unwrap();
        if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
            group.merge_staged_commit(&provider, *staged).unwrap();
        }
    }

    /// Encrypt plaintext.  Mirrors: try_mls_encrypt
    fn encrypt(&self, conv_id: &str, plaintext: &[u8]) -> Vec<u8> {
        try_mls_encrypt(&self.db, conv_id, plaintext).expect("encrypt should succeed")
    }

    /// Decrypt ciphertext.  Mirrors: try_mls_decrypt
    fn decrypt(&self, conv_id: &str, ciphertext: &[u8]) -> Vec<u8> {
        try_mls_decrypt(&self.db, conv_id, ciphertext).expect("decrypt should succeed")
    }

    fn has_group(&self, conv_id: &str) -> bool {
        has_local_group(&self.db, conv_id)
    }

    fn epoch(&self, conv_id: &str) -> u64 {
        let provider = PollisProvider::new(&self.db);
        let group_id = GroupId::from_slice(conv_id.as_bytes());
        MlsGroup::load(provider.storage(), &group_id)
            .unwrap()
            .expect("group must exist")
            .epoch()
            .as_u64()
    }
}

fn load_group<'a>(
    provider: &PollisProvider<'a>,
    conv_id: &str,
) -> (MlsGroup, SignatureKeyPair) {
    let group_id = GroupId::from_slice(conv_id.as_bytes());
    let group = MlsGroup::load(provider.storage(), &group_id)
        .unwrap()
        .expect("group must exist");

    let own_leaf = group.own_leaf_node().unwrap();
    let sig_ref = own_leaf.signature_key().as_slice();
    let signer = SignatureKeyPair::read(
        provider.storage(),
        sig_ref,
        group.ciphersuite().signature_algorithm(),
    )
    .expect("signer must exist");

    (group, signer)
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test: full multi-device group creation and messaging
//
//  This mirrors the exact dev-multi.sh scenario:
//    dan-d1  creates a group
//    dan-d2  (same user, different device) receives Welcome
//    guy     is invited by dan-d1, accepts
//    ants    is invited by dan-d1, accepts
//    Everyone sends a message, everyone decrypts all messages.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn full_group_lifecycle_with_multi_device() {
    let conv_id = "01JINTEGRATION_FULL_LIFECYCLE";

    // ── Set up devices ──────────────────────────────────────────────────
    let dan_d1 = Device::new("dan", "dan_device_1");
    let dan_d2 = Device::new("dan", "dan_device_2");
    let guy = Device::new("guy", "guy_device_1");
    let ants = Device::new("ants", "ants_device_1");

    // ── Step 1: dan-d1 creates the group ────────────────────────────────
    dan_d1.create_group(conv_id);
    assert!(dan_d1.has_group(conv_id));
    assert!(!dan_d2.has_group(conv_id));

    // ── Step 2: add dan-d2 (creator's other device) ─────────────────────
    // Mirrors: add_member_mls_for_own_devices in create_group
    let dan_d2_kp = dan_d2.generate_key_package();
    let (_add_d2_commit, dan_d2_welcome) = dan_d1.add_members(conv_id, &[dan_d2_kp]);
    // dan-d2 receives Welcome (via poll_mls_welcomes)
    dan_d2.apply_welcome(&dan_d2_welcome);
    assert!(dan_d2.has_group(conv_id));

    // ── Step 3: dan-d1 invites guy ──────────────────────────────────────
    // Mirrors: send_group_invite → add_member_mls_inner
    let guy_kp = guy.generate_key_package();
    let (add_guy_commit, guy_welcome) = dan_d1.add_members(conv_id, &[guy_kp]);

    // dan-d2 must process the commit to stay in sync (via process_pending_commits)
    dan_d2.apply_commit(conv_id, &add_guy_commit);

    // guy accepts invite, polls welcomes
    guy.apply_welcome(&guy_welcome);
    assert!(guy.has_group(conv_id));

    // ── Step 4: dan-d1 invites ants ─────────────────────────────────────
    let ants_kp = ants.generate_key_package();
    let (add_ants_commit, ants_welcome) = dan_d1.add_members(conv_id, &[ants_kp]);

    // Both dan-d2 and guy must process this commit
    dan_d2.apply_commit(conv_id, &add_ants_commit);
    guy.apply_commit(conv_id, &add_ants_commit);

    ants.apply_welcome(&ants_welcome);
    assert!(ants.has_group(conv_id));

    // ── Step 5: verify all devices are at the same epoch ────────────────
    let expected_epoch = dan_d1.epoch(conv_id);
    assert_eq!(dan_d2.epoch(conv_id), expected_epoch, "dan-d2 epoch mismatch");
    assert_eq!(guy.epoch(conv_id), expected_epoch, "guy epoch mismatch");
    assert_eq!(ants.epoch(conv_id), expected_epoch, "ants epoch mismatch");

    // ── Step 6: everyone sends, everyone decrypts ───────────────────────
    let all_devices: Vec<&Device> = vec![&dan_d1, &dan_d2, &guy, &ants];
    let messages = [
        (&dan_d1, b"message from dan device 1" as &[u8]),
        (&dan_d2, b"message from dan device 2" as &[u8]),
        (&guy, b"message from guy" as &[u8]),
        (&ants, b"message from ants" as &[u8]),
    ];

    for (sender, plaintext) in &messages {
        let ct = sender.encrypt(conv_id, plaintext);

        for receiver in &all_devices {
            // The sender can't decrypt their own MLS application message
            // (MLS spec: sender skips self), so skip same-device.
            if std::ptr::eq(*sender, *receiver) {
                continue;
            }
            let decrypted = receiver.decrypt(conv_id, &ct);
            assert_eq!(
                &decrypted, plaintext,
                "{} failed to decrypt message from {}",
                receiver.device_id, sender.device_id
            );
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test: late-joining device catches up via commits
//
//  dan-d1 creates group, invites guy and ants. THEN dan-d2 joins via
//  Welcome and must process all pending commits to reach current epoch
//  before it can decrypt messages.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn late_joining_device_catches_up_via_commits() {
    let conv_id = "01JINTEGRATION_LATE_JOIN";

    let dan_d1 = Device::new("dan", "dan_d1");
    let dan_d2 = Device::new("dan", "dan_d2");
    let guy = Device::new("guy", "guy_d1");
    let ants = Device::new("ants", "ants_d1");

    // dan-d1 creates group
    dan_d1.create_group(conv_id);

    // dan-d1 adds dan-d2 immediately (but d2 doesn't apply Welcome yet)
    let dan_d2_kp = dan_d2.generate_key_package();
    let (_add_d2_commit, dan_d2_welcome) = dan_d1.add_members(conv_id, &[dan_d2_kp]);

    // dan-d1 adds guy
    let guy_kp = guy.generate_key_package();
    let (add_guy_commit, guy_welcome) = dan_d1.add_members(conv_id, &[guy_kp]);
    guy.apply_welcome(&guy_welcome);

    // dan-d1 adds ants
    let ants_kp = ants.generate_key_package();
    let (add_ants_commit, ants_welcome) = dan_d1.add_members(conv_id, &[ants_kp]);
    guy.apply_commit(conv_id, &add_ants_commit);
    ants.apply_welcome(&ants_welcome);

    // NOW dan-d2 finally comes online, applies Welcome, then catches up
    // with all the commits it missed.
    dan_d2.apply_welcome(&dan_d2_welcome);
    // dan-d2 is at epoch 1 (from Welcome).  Needs to process commits from
    // epoch 1 (add guy) and epoch 2 (add ants) to reach epoch 3.
    dan_d2.apply_commit(conv_id, &add_guy_commit);
    dan_d2.apply_commit(conv_id, &add_ants_commit);

    // Verify all at same epoch
    let expected = dan_d1.epoch(conv_id);
    assert_eq!(dan_d2.epoch(conv_id), expected);
    assert_eq!(guy.epoch(conv_id), expected);
    assert_eq!(ants.epoch(conv_id), expected);

    // guy sends — all 4 devices decrypt
    let ct = guy.encrypt(conv_id, b"hey everyone");
    assert_eq!(dan_d1.decrypt(conv_id, &ct), b"hey everyone");
    assert_eq!(dan_d2.decrypt(conv_id, &ct), b"hey everyone");
    assert_eq!(ants.decrypt(conv_id, &ct), b"hey everyone");
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test: invite flow (accept_group_invite path)
//
//  Mirrors the invite flow: dan creates group, sends invite to guy.
//  guy accepts → polls welcomes → enters channel → decrypts.
//  dan's second device also receives messages from guy.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn invite_flow_both_creator_devices_and_invitee_communicate() {
    let conv_id = "01JINTEGRATION_INVITE_FLOW";

    let dan_d1 = Device::new("dan", "dan_d1");
    let dan_d2 = Device::new("dan", "dan_d2");
    let guy = Device::new("guy", "guy_d1");

    // dan-d1 creates, immediately adds dan-d2
    dan_d1.create_group(conv_id);
    let d2_kp = dan_d2.generate_key_package();
    let (d2_commit, d2_welcome) = dan_d1.add_members(conv_id, &[d2_kp]);
    dan_d2.apply_welcome(&d2_welcome);
    let _ = d2_commit;

    // dan-d1 sends invite to guy (pre-generates Welcome)
    let guy_kp = guy.generate_key_package();
    let (guy_commit, guy_welcome) = dan_d1.add_members(conv_id, &[guy_kp]);

    // dan-d2 processes the commit (stays in sync)
    dan_d2.apply_commit(conv_id, &guy_commit);

    // guy accepts invite → polls welcomes
    guy.apply_welcome(&guy_welcome);

    // guy sends a message
    let ct = guy.encrypt(conv_id, b"I'm in!");

    // Both dan devices decrypt
    assert_eq!(dan_d1.decrypt(conv_id, &ct), b"I'm in!");
    assert_eq!(dan_d2.decrypt(conv_id, &ct), b"I'm in!");

    // dan-d1 replies — guy and dan-d2 decrypt
    let reply = dan_d1.encrypt(conv_id, b"welcome!");
    assert_eq!(guy.decrypt(conv_id, &reply), b"welcome!");
    assert_eq!(dan_d2.decrypt(conv_id, &reply), b"welcome!");

    // dan-d2 replies — guy and dan-d1 decrypt
    let d2_msg = dan_d2.encrypt(conv_id, b"hello from d2");
    assert_eq!(guy.decrypt(conv_id, &d2_msg), b"hello from d2");
    assert_eq!(dan_d1.decrypt(conv_id, &d2_msg), b"hello from d2");
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test: device without Welcome can't decrypt but doesn't break others
//
//  Simulates a stale device that has no key package and thus never gets a
//  Welcome.  It should NOT be able to decrypt, but it must not corrupt the
//  group state for other devices.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn stale_device_without_welcome_cannot_decrypt_but_others_unaffected() {
    let conv_id = "01JINTEGRATION_STALE_DEVICE";

    let dan_d1 = Device::new("dan", "dan_d1");
    let dan_stale = Device::new("dan", "dan_stale");
    let guy = Device::new("guy", "guy_d1");

    // dan-d1 creates group.  dan_stale has no KPs so is NOT added.
    dan_d1.create_group(conv_id);

    // Only add guy (dan_stale is skipped in production because no KPs).
    let guy_kp = guy.generate_key_package();
    let (_commit, guy_welcome) = dan_d1.add_members(conv_id, &[guy_kp]);
    guy.apply_welcome(&guy_welcome);

    // dan_stale does NOT have the group
    assert!(!dan_stale.has_group(conv_id));

    // dan-d1 and guy can still communicate
    let ct = dan_d1.encrypt(conv_id, b"only d1 and guy");
    assert_eq!(guy.decrypt(conv_id, &ct), b"only d1 and guy");

    let reply = guy.encrypt(conv_id, b"yep, works");
    assert_eq!(dan_d1.decrypt(conv_id, &reply), b"yep, works");

    // dan_stale cannot decrypt (no group state)
    assert!(try_mls_decrypt(&dan_stale.db, conv_id, &ct).is_none());
}

// ═══════════════════════════════════════════════════════════════════════════
//  Test: multiple messages in sequence (epoch ratchet)
//
//  MLS application messages advance the sender's ratchet.  Verify that
//  a burst of messages from one device is still decryptable by all others.
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn burst_of_messages_all_decrypt_in_order() {
    let conv_id = "01JINTEGRATION_MSG_BURST";

    let dan_d1 = Device::new("dan", "dan_d1");
    let dan_d2 = Device::new("dan", "dan_d2");
    let guy = Device::new("guy", "guy_d1");

    dan_d1.create_group(conv_id);

    let d2_kp = dan_d2.generate_key_package();
    let (d2_commit, d2_welcome) = dan_d1.add_members(conv_id, &[d2_kp]);
    dan_d2.apply_welcome(&d2_welcome);
    let _ = d2_commit;

    let guy_kp = guy.generate_key_package();
    let (guy_commit, guy_welcome) = dan_d1.add_members(conv_id, &[guy_kp]);
    dan_d2.apply_commit(conv_id, &guy_commit);
    guy.apply_welcome(&guy_welcome);

    // dan-d1 sends 5 messages in a row
    let mut ciphertexts = Vec::new();
    for i in 0..5 {
        let msg = format!("message {i}");
        ciphertexts.push((msg.clone(), dan_d1.encrypt(conv_id, msg.as_bytes())));
    }

    // dan-d2 and guy decrypt all 5 in order
    for (expected, ct) in &ciphertexts {
        assert_eq!(
            String::from_utf8(dan_d2.decrypt(conv_id, ct)).unwrap(),
            *expected
        );
        assert_eq!(
            String::from_utf8(guy.decrypt(conv_id, ct)).unwrap(),
            *expected
        );
    }

    // Now guy sends 3 messages
    let mut guy_cts = Vec::new();
    for i in 0..3 {
        let msg = format!("guy says {i}");
        guy_cts.push((msg.clone(), guy.encrypt(conv_id, msg.as_bytes())));
    }

    // Both dan devices decrypt
    for (expected, ct) in &guy_cts {
        assert_eq!(
            String::from_utf8(dan_d1.decrypt(conv_id, ct)).unwrap(),
            *expected
        );
        assert_eq!(
            String::from_utf8(dan_d2.decrypt(conv_id, ct)).unwrap(),
            *expected
        );
    }
}
