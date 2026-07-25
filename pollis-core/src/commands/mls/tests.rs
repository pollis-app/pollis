// ── Tests ─────────────────────────────────────────────────────────────────────

use super::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::types::CryptoError;
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

// Re-export the private items from submodules into this `tests` module
// so the test bodies (which were originally `use super::*` from the
// single-file module) can keep referencing them by short name.
use super::group_state::{create_mls_group_in_suite, load_group_with_signer};
use super::key_packages::build_key_package_in_suite;
use super::provider::{MlsProvider, PollisPqProvider, CS_CLASSIC, CS_HYBRID, SIGNATURE_SCHEME};

/// Create an in-memory SQLite DB with the `mls_kv` table.
fn make_db() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE mls_kv (
            scope TEXT NOT NULL,
            key   BLOB NOT NULL,
            value BLOB NOT NULL,
            PRIMARY KEY (scope, key)
        );",
    ).unwrap();
    conn
}

/// Synthetic device ID for test users. In tests each "user" maps to a
/// single device so we derive a deterministic device_id from the user_id.
fn test_device_id(user_id: &str) -> String {
    format!("{user_id}_dev")
}

/// Create an MLS group with `user_id` as sole member and return the
/// `SignatureKeyPair` so the caller can later call `create_message`.
fn create_group(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    user_id: &str,
) -> SignatureKeyPair {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS_CLASSIC)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
        .unwrap();

    sig_keys
}

/// Generate a key package for `user_id` in `conn` and return the TLS bytes.
fn gen_key_package(conn: &rusqlite::Connection, user_id: &str) -> Vec<u8> {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_CLASSIC, &provider, &sig_keys, cred_with_key)
        .unwrap();

    bundle.key_package().tls_serialize_detached().unwrap()
}

/// Alice creates a group, adds Bob, then Alice encrypts and Bob decrypts.
#[test]
fn encrypt_decrypt_roundtrip() {
    let conv_id = "01JTEST00000000000000000AB";

    let alice_db = make_db();
    let bob_db = make_db();

    // Alice creates the group.
    create_group(&alice_db, conv_id, "alice");

    // Bob generates a key package.
    let bob_kp_bytes = gen_key_package(&bob_db, "bob");

    // Alice adds Bob: add_members → commit + welcome.
    let welcome_bytes: Vec<u8> = {
        let alice_provider = PollisProvider::new(&alice_db);
        let (mut alice_group, alice_signer) =
            load_group_with_signer(&alice_provider, conv_id).unwrap();

        let mut kp_reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut kp_reader).unwrap();
        let kp = kp_in.validate(alice_provider.crypto(), ProtocolVersion::Mls10).unwrap();

        let (commit_msg, welcome_msg, _) =
            alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();

        alice_group.merge_pending_commit(&alice_provider).unwrap();

        // Keep commit_msg in scope to avoid "unused" warn — it would be posted
        // to mls_commit_log in production.
        let _ = commit_msg.tls_serialize_detached().unwrap();
        welcome_msg.tls_serialize_detached().unwrap()
    };

    // Bob processes the Welcome.
    {
        let bob_provider = PollisProvider::new(&bob_db);
        let mut reader: &[u8] = &welcome_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };
        let join_config = MlsGroupJoinConfig::default();
        StagedWelcome::new_from_welcome(&bob_provider, &join_config, welcome, None)
            .unwrap()
            .into_group(&bob_provider)
            .unwrap();
    }

    // Alice encrypts.
    let plaintext = b"hello mls";
    let ciphertext = try_mls_encrypt(&alice_db, conv_id, plaintext)
        .expect("try_mls_encrypt failed");

    // Bob decrypts.
    let (decrypted, sender) = try_mls_decrypt(&bob_db, conv_id, &ciphertext)
        .expect("try_mls_decrypt failed");

    assert_eq!(decrypted, plaintext);
    // Attribution comes from the MLS-authenticated credential, not any envelope
    // column (sealed sender, `docs/metadata-minimization-design.md` §2).
    assert_eq!(sender, "alice");
}

/// The full text-message path through the real MLS crypto: pad the plaintext,
/// encrypt, decrypt, strip the padding — and get the EXACT original back, for a
/// spread of sizes. Also proves version-byte back-compat: an OLD, unpadded send
/// (raw bytes through `try_mls_encrypt`) still decodes when the new reader runs
/// `strip`. (Issue #331 v2, `docs/metadata-minimization-design.md` §4.1.)
#[test]
fn padded_text_roundtrip_through_mls() {
    use crate::commands::messages::framing;

    let conv_id = "01JTEST00000000000000000EF";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group(&alice_db, conv_id, "alice");
    let bob_kp_bytes = gen_key_package(&bob_db, "bob");

    // Alice adds Bob.
    let welcome_bytes: Vec<u8> = {
        let alice_provider = PollisProvider::new(&alice_db);
        let (mut alice_group, alice_signer) =
            load_group_with_signer(&alice_provider, conv_id).unwrap();
        let mut kp_reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut kp_reader).unwrap();
        let kp = kp_in.validate(alice_provider.crypto(), ProtocolVersion::Mls10).unwrap();
        let (_commit, welcome_msg, _) =
            alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();
        alice_group.merge_pending_commit(&alice_provider).unwrap();
        welcome_msg.tls_serialize_detached().unwrap()
    };

    // Bob joins.
    {
        let bob_provider = PollisProvider::new(&bob_db);
        let mut reader: &[u8] = &welcome_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };
        StagedWelcome::new_from_welcome(&bob_provider, &MlsGroupJoinConfig::default(), welcome, None)
            .unwrap()
            .into_group(&bob_provider)
            .unwrap();
    }

    // Padded path: exact round-trip AND same ciphertext-driving bucket for
    // very different plaintext lengths.
    let short = "ok".as_bytes();
    let long = "x".repeat(200);
    let short_ct = {
        let padded = framing::pad(short);
        try_mls_encrypt(&alice_db, conv_id, &padded).expect("encrypt short")
    };
    let long_ct = {
        let padded = framing::pad(long.as_bytes());
        try_mls_encrypt(&alice_db, conv_id, &padded).expect("encrypt long")
    };
    // Two very different plaintext lengths collapse to one ciphertext size.
    assert_eq!(
        short_ct.len(),
        long_ct.len(),
        "padding must collapse distinct plaintext lengths to one ciphertext size"
    );

    let (short_pt, _) = try_mls_decrypt(&bob_db, conv_id, &short_ct).expect("decrypt short");
    assert_eq!(framing::strip(&short_pt), short);
    let (long_pt, sender) = try_mls_decrypt(&bob_db, conv_id, &long_ct).expect("decrypt long");
    assert_eq!(framing::strip(&long_pt), long.as_bytes());
    assert_eq!(sender, "alice");

    // Back-compat: an OLD client sends unpadded bytes. The new reader's `strip`
    // must leave them untouched.
    let legacy_plain = "legacy unpadded message".as_bytes();
    let legacy_ct = try_mls_encrypt(&alice_db, conv_id, legacy_plain).expect("encrypt legacy");
    let (legacy_pt, _) = try_mls_decrypt(&bob_db, conv_id, &legacy_ct).expect("decrypt legacy");
    assert_eq!(framing::strip(&legacy_pt), legacy_plain);
}

/// A solo group (no other members) returns None for decrypt — only a member
/// of the group can decrypt.  But the creator can still encrypt successfully.
#[test]
fn solo_group_encrypt_returns_some() {
    let conv_id = "01JTEST00000000000000000CD";
    let alice_db = make_db();
    create_group(&alice_db, conv_id, "alice");

    let ct = try_mls_encrypt(&alice_db, conv_id, b"test");
    assert!(ct.is_some(), "creator should be able to encrypt in a solo group");
}

/// A missing group returns None without panicking.
#[test]
fn missing_group_returns_none() {
    let db = make_db();
    let ct = try_mls_encrypt(&db, "no-such-group", b"test");
    assert!(ct.is_none());

    let result = try_mls_decrypt(&db, "no-such-group", b"\x00\x01\x02");
    assert!(result.is_none());
}

// ── helpers shared by scenario tests ─────────────────────────────────────

/// Alice adds a member to her group. Returns (commit_bytes, welcome_bytes).
fn add_member_to_group(
    adder_db: &rusqlite::Connection,
    conv_id: &str,
    kp_bytes: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let provider = PollisProvider::new(adder_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

    let mut reader: &[u8] = kp_bytes;
    let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
    let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();

    let (commit_msg, welcome_msg, _) =
        group.add_members(&provider, &signer, &[kp]).unwrap();
    group.merge_pending_commit(&provider).unwrap();

    (
        commit_msg.tls_serialize_detached().unwrap(),
        welcome_msg.tls_serialize_detached().unwrap(),
    )
}

/// Join a group by applying a serialised Welcome message.
fn join_via_welcome(joiner_db: &rusqlite::Connection, welcome_bytes: &[u8]) {
    let provider = PollisProvider::new(joiner_db);
    let mut reader: &[u8] = welcome_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
    let welcome = match msg_in.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected Welcome"),
    };
    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();
    StagedWelcome::new_from_welcome(&provider, &join_config, welcome, None)
        .unwrap()
        .into_group(&provider)
        .unwrap();
}

/// Apply a serialised commit to advance a member's epoch.
fn apply_commit(member_db: &rusqlite::Connection, conv_id: &str, commit_bytes: &[u8]) {
    let provider = PollisProvider::new(member_db);
    let group_id = GroupId::from_slice(conv_id.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .unwrap()
        .expect("group must exist");

    let mut reader: &[u8] = commit_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
    let protocol_msg = msg_in.try_into_protocol_message().unwrap();
    let processed = group.process_message(&provider, protocol_msg).unwrap();
    if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
        group.merge_staged_commit(&provider, *staged).unwrap();
    }
}

/// Remove a member from the group. Returns the commit bytes (for remaining
/// members to apply via `apply_commit`).
fn remove_member(
    remover_db: &rusqlite::Connection,
    conv_id: &str,
    target_user_id: &str,
) -> Vec<u8> {
    let provider = PollisProvider::new(remover_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

    // Find all leaves for the target user (may have multiple devices).
    let leaf_indices: Vec<LeafNodeIndex> = group.members()
        .filter_map(|m| {
            let cred_user = parse_credential_user_id(&m.credential);
            if cred_user == target_user_id {
                Some(m.index)
            } else {
                None
            }
        })
        .collect();
    assert!(!leaf_indices.is_empty(), "target must be in group");

    let (commit_msg, _, _) =
        group.remove_members(&provider, &signer, &leaf_indices).unwrap();
    group.merge_pending_commit(&provider).unwrap();

    commit_msg.tls_serialize_detached().unwrap()
}

// ── scenario tests ────────────────────────────────────────────────────────

/// Alice, Bob, and Carol are all in the same group. Each can encrypt a
/// message that the other two can decrypt.
#[test]
fn three_way_group_messaging() {
    let conv_id = "01JTEST00000000000000000EF";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    // Alice creates the group.
    create_group(&alice_db, conv_id, "alice");

    // Alice adds Bob.
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Alice adds Carol. Bob must also apply this commit.
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    // Suppress unused-variable warning — add_bob_commit would go to mls_commit_log in prod.
    let _ = add_bob_commit;

    // Alice sends → Bob and Carol both decrypt.
    let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"hello from alice").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, conv_id, &alice_ct).unwrap().0, b"hello from alice");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap().0, b"hello from alice");

    // Bob sends → Alice and Carol both decrypt.
    let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello from bob").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &bob_ct).unwrap().0, b"hello from bob");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &bob_ct).unwrap().0, b"hello from bob");

    // Carol sends → Alice and Bob both decrypt.
    let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"hello from carol").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap().0, b"hello from carol");
    assert_eq!(try_mls_decrypt(&bob_db, conv_id, &carol_ct).unwrap().0, b"hello from carol");
}

/// After Bob is removed, messages Alice sends are encrypted at the new epoch.
/// Bob's local state is stuck at the old epoch, so he cannot decrypt them.
#[test]
fn removed_member_cannot_decrypt_new_messages() {
    let conv_id = "01JTEST00000000000000000GH";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Confirm Bob can read messages before removal.
    let pre_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"pre-removal").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &pre_remove_ct).unwrap().0,
        b"pre-removal"
    );

    // Alice removes Bob. Alice's epoch advances; Bob's does not.
    let _remove_commit = remove_member(&alice_db, conv_id, "bob");

    // Alice sends a message at the new epoch.
    let post_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"secret").unwrap();

    // Bob cannot decrypt it — forward secrecy enforced.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &post_remove_ct).is_none(),
        "removed member must not decrypt messages from new epoch"
    );
}

/// A newly added member cannot decrypt messages that were sent before they joined.
#[test]
fn new_member_cannot_read_history() {
    let conv_id = "01JTEST00000000000000000IJ";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    // Alice sends a message before Bob exists.
    let history_ct = try_mls_encrypt(&alice_db, conv_id, b"private history").unwrap();

    // Alice adds Bob.
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Bob cannot decrypt the pre-join message.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &history_ct).is_none(),
        "new member must not decrypt history from before they joined"
    );

    // But Bob can decrypt messages sent after he joined.
    let new_ct = try_mls_encrypt(&alice_db, conv_id, b"welcome bob").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &new_ct).unwrap().0,
        b"welcome bob"
    );
}

/// When a new member is added, all existing members must apply the commit
/// (epoch advance) before they can send/receive further messages.
#[test]
fn epoch_sync_via_commit_processing() {
    let conv_id = "01JTEST00000000000000000KL";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    // Alice adds Bob (epoch 0→1).
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Bob adds Carol (epoch 1→2). Alice hasn't applied this commit yet.
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&bob_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);

    // Alice applies Bob's add-Carol commit to advance to epoch 2.
    apply_commit(&alice_db, conv_id, &add_carol_commit);

    let _ = add_bob_commit;

    // Now all three members are at epoch 2 and can communicate.
    let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"carol here").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap().0,
        b"carol here"
    );
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &carol_ct).unwrap().0,
        b"carol here"
    );
}

/// When a member leaves (is removed), the remaining members can still
/// communicate, the removed member cannot decrypt, and a newly added
/// member can participate in the group.
#[test]
fn leave_group_remaining_members_communicate_then_new_member_joins() {
    let conv_id = "01JTEST0000000000000LEAVE1";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();
    let dave_db = make_db();

    // Alice creates group, adds Bob and Carol.
    create_group(&alice_db, conv_id, "alice");

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);
    let _ = add_bob_commit;

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    // Verify all three can communicate before removal.
    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"all three here").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap().0, b"all three here");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &pre_ct).unwrap().0, b"all three here");

    // Alice removes Bob. Carol applies the commit.
    let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
    apply_commit(&carol_db, conv_id, &remove_bob_commit);

    // Alice and Carol can still communicate.
    let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap().0, b"bob is gone");

    let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"confirmed").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap().0, b"confirmed");

    // Bob cannot decrypt post-removal messages.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
        "removed bob must not decrypt"
    );

    // Alice adds Dave. Carol applies the commit.
    let dave_kp = gen_key_package(&dave_db, "dave");
    let (add_dave_commit, dave_welcome) = add_member_to_group(&alice_db, conv_id, &dave_kp);
    join_via_welcome(&dave_db, &dave_welcome);
    apply_commit(&carol_db, conv_id, &add_dave_commit);

    // All three current members (Alice, Carol, Dave) can communicate.
    let dave_ct = try_mls_encrypt(&dave_db, conv_id, b"dave here").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &dave_ct).unwrap().0, b"dave here");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &dave_ct).unwrap().0, b"dave here");

    // Bob still cannot decrypt.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &dave_ct).is_none(),
        "bob must still be locked out after new member joins"
    );
}

/// After multiple add/remove cycles the group epoch is consistent and
/// only current members can decrypt.
#[test]
fn key_rotation_across_multiple_membership_changes() {
    let conv_id = "01JTEST00000000000000000MN";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    // Add Bob (epoch 0→1).
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Add Carol (epoch 1→2). Bob applies commit.
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    // Remove Bob (epoch 2→3). Carol applies commit.
    let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
    apply_commit(&carol_db, conv_id, &remove_bob_commit);

    // Alice and Carol can communicate at epoch 3.
    let ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
    assert_eq!(
        try_mls_decrypt(&carol_db, conv_id, &ct).unwrap().0,
        b"bob is gone"
    );

    // Bob (stuck at epoch 2) cannot decrypt epoch-3 messages.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &ct).is_none(),
        "removed Bob must not decrypt after key rotation"
    );
}

/// Simulates account deletion: when a user is removed from a group (as
/// part of account deletion), the remaining members' keys should rotate
/// so the deleted user cannot decrypt future messages.
///
/// The `delete_account` command (auth.rs) enumerates all groups and DM
/// channels the user belongs to and broadcasts `membership_changed` for
/// each before deleting DB rows.  This test verifies the underlying MLS
/// removal + epoch advance works across multiple groups.
///
/// See: https://github.com/actuallydan/pollis/issues/103
#[test]
fn account_deletion_rotates_keys_for_remaining_members() {
    let group1 = "01JTEST000000000000ACCTDEL1";
    let group2 = "01JTEST000000000000ACCTDEL2";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    // --- Group 1: Alice + Bob + Carol ---
    create_group(&alice_db, group1, "alice");

    let bob_kp1 = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome1) = add_member_to_group(&alice_db, group1, &bob_kp1);
    join_via_welcome(&bob_db, &bob_welcome1);

    let carol_kp1 = gen_key_package(&carol_db, "carol");
    let (add_carol_commit1, carol_welcome1) = add_member_to_group(&alice_db, group1, &carol_kp1);
    join_via_welcome(&carol_db, &carol_welcome1);
    apply_commit(&bob_db, group1, &add_carol_commit1);

    // --- Group 2: Alice + Bob (Carol not in this one) ---
    create_group(&alice_db, group2, "alice");

    let bob_kp2 = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome2) = add_member_to_group(&alice_db, group2, &bob_kp2);
    join_via_welcome(&bob_db, &bob_welcome2);

    // Verify Bob can read in both groups before deletion.
    let pre_g1 = try_mls_encrypt(&alice_db, group1, b"pre-delete g1").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, group1, &pre_g1).unwrap().0, b"pre-delete g1");

    let pre_g2 = try_mls_encrypt(&alice_db, group2, b"pre-delete g2").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, group2, &pre_g2).unwrap().0, b"pre-delete g2");

    // --- Simulate account deletion for Bob ---
    // In production this would be done by delete_account iterating all
    // groups and broadcasting membership_changed for each. Here we do
    // the MLS removal manually per group.

    // Remove Bob from group 1 — Carol applies commit.
    let remove_g1 = remove_member(&alice_db, group1, "bob");
    apply_commit(&carol_db, group1, &remove_g1);

    // Remove Bob from group 2 — no other non-alice members to notify.
    let _remove_g2 = remove_member(&alice_db, group2, "bob");

    // --- Verify key rotation: Bob locked out of both groups ---
    let post_g1 = try_mls_encrypt(&alice_db, group1, b"post-delete g1").unwrap();
    assert!(
        try_mls_decrypt(&bob_db, group1, &post_g1).is_none(),
        "deleted Bob must not decrypt group1 messages after account deletion"
    );

    let post_g2 = try_mls_encrypt(&alice_db, group2, b"post-delete g2").unwrap();
    assert!(
        try_mls_decrypt(&bob_db, group2, &post_g2).is_none(),
        "deleted Bob must not decrypt group2 messages after account deletion"
    );

    // --- Verify remaining members still work ---
    // Group 1: Alice and Carol can still communicate.
    assert_eq!(
        try_mls_decrypt(&carol_db, group1, &post_g1).unwrap().0,
        b"post-delete g1"
    );
    let carol_msg = try_mls_encrypt(&carol_db, group1, b"carol still here").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_db, group1, &carol_msg).unwrap().0,
        b"carol still here"
    );
}

// ── multi-device helpers ────────────────────────────────────────────────

/// Create an MLS group with an explicit device_id in the credential.
fn create_group_with_device(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
) -> SignatureKeyPair {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS_CLASSIC)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
        .unwrap();

    sig_keys
}

/// Generate a key package with an explicit device_id in the credential.
fn gen_key_package_with_device(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
) -> Vec<u8> {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_CLASSIC, &provider, &sig_keys, cred_with_key)
        .unwrap();

    bundle.key_package().tls_serialize_detached().unwrap()
}

/// Generate a key package using a pre-existing signing keypair. This
/// simulates the production "stable per-device signing key" from
/// `load_or_create_device_signer`: every KP the device publishes is
/// signed by the same key, so the credential's signature key bytes
/// match on every re-issue.
fn gen_key_package_with_existing_signer(
    conn: &rusqlite::Connection,
    user_id: &str,
    device_id: &str,
    sig_keys: &SignatureKeyPair,
) -> Vec<u8> {
    let provider = PollisProvider::new(conn);

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_CLASSIC, &provider, sig_keys, cred_with_key)
        .unwrap();

    bundle.key_package().tls_serialize_detached().unwrap()
}

/// Add multiple key packages to a group in a single `add_members` call.
/// Mirrors production reconcile which batches all devices.
fn add_members_batch(
    adder_db: &rusqlite::Connection,
    conv_id: &str,
    kp_bytes_list: &[Vec<u8>],
) -> (Vec<u8>, Vec<u8>) {
    let provider = PollisProvider::new(adder_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

    let validated_kps: Vec<KeyPackage> = kp_bytes_list.iter().map(|kp_raw| {
        let mut reader: &[u8] = kp_raw;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap()
    }).collect();

    let (commit_msg, welcome_msg, _) =
        group.add_members(&provider, &signer, &validated_kps).unwrap();
    group.merge_pending_commit(&provider).unwrap();

    (
        commit_msg.tls_serialize_detached().unwrap(),
        welcome_msg.tls_serialize_detached().unwrap(),
    )
}

// ── multi-device credential tests ───────────────────────────────────────

/// Credential encodes user_id:device_id; parsing extracts user_id.
#[test]
fn credential_roundtrip_with_device() {
    let cred = make_credential("alice", "dev_01ABCDEF");
    assert_eq!(parse_credential_user_id(&cred), "alice");
    assert_eq!(
        String::from_utf8_lossy(cred.serialized_content()),
        "alice:dev_01ABCDEF"
    );
}

/// Legacy credentials (no colon) still parse correctly.
#[test]
fn credential_legacy_no_device_id() {
    let cred: Credential = BasicCredential::new("alice".as_bytes().to_vec()).into();
    assert_eq!(parse_credential_user_id(&cred), "alice");
}

// ── multi-device scenario tests ─────────────────────────────────────────

/// Alice has two devices in the group. Bob sends a message. Both Alice
/// devices decrypt it.
#[test]
fn multi_device_both_devices_decrypt() {
    let conv_id = "01JTEST00000000000MULTIDEV1";

    let alice_d1_db = make_db();
    let alice_d2_db = make_db();
    let bob_db = make_db();

    // Alice device 1 creates the group.
    create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");

    // Add Bob.
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (add_bob_commit, bob_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);
    let _ = add_bob_commit;

    // Add Alice's second device.
    let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
    let (add_d2_commit, alice_d2_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
    join_via_welcome(&alice_d2_db, &alice_d2_welcome);
    apply_commit(&bob_db, conv_id, &add_d2_commit);

    // Bob sends.
    let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello both alices").unwrap();

    // Both Alice devices decrypt.
    assert_eq!(
        try_mls_decrypt(&alice_d1_db, conv_id, &bob_ct).unwrap().0,
        b"hello both alices"
    );
    assert_eq!(
        try_mls_decrypt(&alice_d2_db, conv_id, &bob_ct).unwrap().0,
        b"hello both alices"
    );
}

/// Two devices for the same user are added in a single add_members commit
/// (matching production reconcile). Both join via the same
/// Welcome and can decrypt.
#[test]
fn multi_device_batch_add_single_commit() {
    let conv_id = "01JTEST00000000000MULTIDEV2";

    let alice_db = make_db();
    let bob_d1_db = make_db();
    let bob_d2_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    // Generate KPs for both Bob devices.
    let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
    let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");

    // Add both in a single commit.
    let (_commit, welcome) =
        add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);

    // Both Bob devices process the same Welcome.
    join_via_welcome(&bob_d1_db, &welcome);
    join_via_welcome(&bob_d2_db, &welcome);

    // Alice sends — both Bob devices decrypt.
    let ct = try_mls_encrypt(&alice_db, conv_id, b"hello bob devices").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_d1_db, conv_id, &ct).unwrap().0,
        b"hello bob devices"
    );
    assert_eq!(
        try_mls_decrypt(&bob_d2_db, conv_id, &ct).unwrap().0,
        b"hello bob devices"
    );

    // Bob device 1 sends — Alice and Bob device 2 both decrypt.
    let bob_ct = try_mls_encrypt(&bob_d1_db, conv_id, b"from bob d1").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_db, conv_id, &bob_ct).unwrap().0,
        b"from bob d1"
    );
    assert_eq!(
        try_mls_decrypt(&bob_d2_db, conv_id, &bob_ct).unwrap().0,
        b"from bob d1"
    );
}

/// Removing a user removes ALL their device leaf nodes. Neither device
/// can decrypt messages from the new epoch.
#[test]
fn remove_multi_device_user_removes_all_leaves() {
    let conv_id = "01JTEST00000000000MULTIDEV3";

    let alice_db = make_db();
    let bob_d1_db = make_db();
    let bob_d2_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    // Add both Bob devices in one commit.
    let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
    let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");
    let (_commit, welcome) =
        add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);
    join_via_welcome(&bob_d1_db, &welcome);
    join_via_welcome(&bob_d2_db, &welcome);

    // Both can decrypt before removal.
    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"before removal").unwrap();
    assert!(try_mls_decrypt(&bob_d1_db, conv_id, &pre_ct).is_some());
    assert!(try_mls_decrypt(&bob_d2_db, conv_id, &pre_ct).is_some());

    // Alice removes "bob" — removes both leaf nodes.
    let _remove_commit = remove_member(&alice_db, conv_id, "bob");

    // Alice sends at new epoch.
    let post_ct = try_mls_encrypt(&alice_db, conv_id, b"after removal").unwrap();

    // Neither Bob device can decrypt.
    assert!(
        try_mls_decrypt(&bob_d1_db, conv_id, &post_ct).is_none(),
        "bob device 1 must not decrypt after removal"
    );
    assert!(
        try_mls_decrypt(&bob_d2_db, conv_id, &post_ct).is_none(),
        "bob device 2 must not decrypt after removal"
    );
}

/// A second device joins later. It cannot read pre-join history but can
/// decrypt new messages.
#[test]
fn second_device_joins_later_cannot_read_history() {
    let conv_id = "01JTEST00000000000MULTIDEV4";

    let alice_d1_db = make_db();
    let alice_d2_db = make_db();
    let bob_db = make_db();

    // Alice device 1 creates group and adds Bob.
    create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Messages sent before alice_d2 joins.
    let history_ct = try_mls_encrypt(&bob_db, conv_id, b"history msg").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_d1_db, conv_id, &history_ct).unwrap().0,
        b"history msg"
    );

    // Alice device 2 joins.
    let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
    let (add_d2_commit, alice_d2_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
    join_via_welcome(&alice_d2_db, &alice_d2_welcome);
    apply_commit(&bob_db, conv_id, &add_d2_commit);

    // Device 2 cannot read pre-join history.
    assert!(
        try_mls_decrypt(&alice_d2_db, conv_id, &history_ct).is_none(),
        "second device must not decrypt messages from before it joined"
    );

    // Both devices decrypt new messages.
    let new_ct = try_mls_encrypt(&bob_db, conv_id, b"new msg").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_d1_db, conv_id, &new_ct).unwrap().0,
        b"new msg"
    );
    assert_eq!(
        try_mls_decrypt(&alice_d2_db, conv_id, &new_ct).unwrap().0,
        b"new msg"
    );
}

/// Regression test for the "re-invited user cannot send messages" bug.
///
/// Scenario: Bob joins, Bob leaves (local-only — no remove commit is
/// posted, which is what production `leave_group` actually does), Alice
/// re-invites Bob. Because Bob's device signing key is STABLE across
/// re-enrollments, a naive `add_members` call fails with
/// `validate_key_uniqueness` — Bob's new leaf signing key is already in
/// Alice's tree. The fix: detect the stale leaf and issue a combined
/// remove+add commit via `commit_builder` (reconcile handles this).
#[test]
fn reinvite_with_stable_signing_key_handles_stale_leaf() {
    let conv_id = "01JTEST000000000REINVITE01";

    let alice_db = make_db();
    let bob_db = make_db();

    // Alice creates the group.
    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    // Bob's device picks a stable signing key it will reuse across
    // enrollments (simulates `load_or_create_device_signer`).
    let bob_signer = {
        let provider = PollisProvider::new(&bob_db);
        let sk = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
        sk.store(provider.storage()).unwrap();
        sk
    };

    // First enrollment: Bob publishes a KP, Alice adds Bob, Bob joins.
    let bob_kp_v1 =
        gen_key_package_with_existing_signer(&bob_db, "bob", "bob_d1", &bob_signer);
    let (_add1_commit, welcome1) = add_members_batch(&alice_db, conv_id, &[bob_kp_v1]);
    join_via_welcome(&bob_db, &welcome1);

    // Verify Alice and Bob can talk.
    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"first life").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap().0,
        b"first life"
    );

    // Bob "leaves" — in production this wipes Bob's local state but does
    // NOT post a remove commit to the group. Alice still has Bob's leaf.
    // We simulate this by leaving Alice's view untouched and dropping
    // Bob's local group state on the floor.

    // Second enrollment: Bob publishes a NEW KP using the SAME signer.
    let bob_db_v2 = make_db();
    let bob_signer_v2 = {
        let provider = PollisProvider::new(&bob_db_v2);
        let sk = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
        sk.store(provider.storage()).unwrap();
        sk
    };

    // Store bob_signer's keypair into bob_db_v2 for Bob to decrypt the welcome.
    {
        let provider_v2 = PollisProvider::new(&bob_db_v2);
        bob_signer.store(provider_v2.storage()).unwrap();
    }
    let _ = bob_signer_v2;
    let bob_kp_v2 =
        gen_key_package_with_existing_signer(&bob_db_v2, "bob", "bob_d1", &bob_signer);

    // Plain add_members should fail: the signing key is already in the
    // tree from bob_kp_v1's still-present leaf.
    {
        let provider = PollisProvider::new(&alice_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();
        let mut reader: &[u8] = &bob_kp_v2;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();
        let naive = group.add_members(&provider, &signer, &[kp]);
        assert!(
            naive.is_err(),
            "plain add_members must reject duplicate signing key — if this \
             starts passing, openmls has changed validation and the stale-leaf \
             branch in reconcile_group_mls_core may no longer be needed"
        );
    }

    // Apply the fix: combined remove+add commit via commit_builder.
    let welcome2_bytes: Vec<u8> = {
        let provider = PollisProvider::new(&alice_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

        // Find Bob's stale leaves.
        let stale: Vec<LeafNodeIndex> = group.members()
            .filter_map(|m| {
                let u = parse_credential_user_id(&m.credential);
                let d = parse_credential_device_id(&m.credential)?;
                if u == "bob" && d == "bob_d1" {
                    Some(m.index)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(stale.len(), 1, "expected exactly one stale bob leaf");

        let mut reader: &[u8] = &bob_kp_v2;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();

        let bundle = group
            .commit_builder()
            .propose_removals(stale.iter().cloned())
            .propose_adds(std::iter::once(kp))
            .load_psks(provider.storage())
            .unwrap()
            .build(provider.rand(), provider.crypto(), &signer, |_| true)
            .unwrap()
            .stage_commit(&provider)
            .unwrap();

        let (_commit, welcome_opt, _gi) = bundle.into_messages();
        let welcome = welcome_opt.expect("welcome must be produced by add proposal");
        group.merge_pending_commit(&provider).unwrap();
        welcome.tls_serialize_detached().unwrap()
    };

    // Bob (new local state) joins via the fresh welcome.
    join_via_welcome(&bob_db_v2, &welcome2_bytes);

    // Alice and Bob-v2 can now send and receive.
    let hello = try_mls_encrypt(&alice_db, conv_id, b"welcome back bob").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db_v2, conv_id, &hello).unwrap().0,
        b"welcome back bob"
    );
    let reply = try_mls_encrypt(&bob_db_v2, conv_id, b"thanks alice").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_db, conv_id, &reply).unwrap().0,
        b"thanks alice"
    );
}

// ── reconcile core tests ────────────────────────────────────────────────

/// Helper: call `reconcile_group_mls_core` with pre-constructed inputs.
/// Validates raw KP bytes and delegates to the sync core.
fn reconcile(
    actor_db: &rusqlite::Connection,
    conv_id: &str,
    roster_user_ids: &[&str],
    available_kp_bytes: &[(String, String, Vec<u8>)],
    actor_user_id: &str,
    actor_device_id: &str,
) -> (ReconcileOutcome, Option<ReconcileCommitData>) {
    let provider = PollisProvider::new(actor_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id).unwrap();

    let roster: std::collections::HashSet<String> =
        roster_user_ids.iter().map(|s| s.to_string()).collect();

    let available_kps: Vec<(String, String, KeyPackage)> = available_kp_bytes
        .iter()
        .map(|(uid, did, bytes)| {
            let mut reader: &[u8] = bytes;
            let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
            let kp = kp_in
                .validate(provider.crypto(), ProtocolVersion::Mls10)
                .unwrap();
            (uid.clone(), did.clone(), kp)
        })
        .collect();

    reconcile_group_mls_core(
        &provider,
        &signer,
        &mut group,
        &roster,
        &available_kps,
        actor_user_id,
        actor_device_id,
        None,
    )
    .unwrap()
}

/// 1. Clean state: tree matches roster, no available KPs → no-op.
#[test]
fn reconcile_no_drift() {
    let conv_id = "01JTEST00000000000RECONCILE1";
    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice", "bob"],
        &[],
        "alice",
        "alice_d1",
    );

    assert!(outcome.added.is_empty());
    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.epoch_before, outcome.epoch_after);
    assert!(!outcome.skipped_self_removal);
    assert!(commit_data.is_none());
}

/// 2. Stale leaf: user removed from roster but leaf remains → remove.
#[test]
fn reconcile_remove_stale_leaf() {
    let conv_id = "01JTEST00000000000RECONCILE2";
    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[],
        "alice",
        "alice_d1",
    );

    assert_eq!(outcome.removed.len(), 1);
    assert_eq!(
        outcome.removed[0],
        ("bob".to_string(), "bob_d1".to_string())
    );
    assert!(outcome.added.is_empty());
    assert!(outcome.epoch_after > outcome.epoch_before);
    assert!(commit_data.is_some());
}

/// 3. Missing device: user in roster with available KP but not in tree → add.
#[test]
fn reconcile_add_missing_device() {
    let conv_id = "01JTEST00000000000RECONCILE3";
    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let bob_kp_bytes = gen_key_package_with_device(&bob_db, "bob", "bob_d1");

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice", "bob"],
        &[("bob".into(), "bob_d1".into(), bob_kp_bytes)],
        "alice",
        "alice_d1",
    );

    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.added.len(), 1);
    assert_eq!(
        outcome.added[0],
        ("bob".to_string(), "bob_d1".to_string())
    );
    assert!(outcome.epoch_after > outcome.epoch_before);
    let data = commit_data.unwrap();
    assert!(
        data.welcome_bytes.is_some(),
        "additions must produce a Welcome"
    );
}

/// 4. Combined add + remove in a single commit.
#[test]
fn reconcile_combined_add_and_remove() {
    let conv_id = "01JTEST00000000000RECONCILE4";
    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let carol_kp_bytes = gen_key_package_with_device(&carol_db, "carol", "carol_d1");

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice", "carol"],
        &[("carol".into(), "carol_d1".into(), carol_kp_bytes)],
        "alice",
        "alice_d1",
    );

    assert_eq!(outcome.removed.len(), 1);
    assert_eq!(
        outcome.removed[0],
        ("bob".to_string(), "bob_d1".to_string())
    );
    assert_eq!(outcome.added.len(), 1);
    assert_eq!(
        outcome.added[0],
        ("carol".to_string(), "carol_d1".to_string())
    );
    assert!(outcome.epoch_after > outcome.epoch_before);
    assert!(commit_data.is_some());
}

/// 5. Committer's own leaf is in the remove set → skipped, flag set.
#[test]
fn reconcile_committer_skip_self_removal() {
    let conv_id = "01JTEST00000000000RECONCILE5";
    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // Alice NOT in roster — she can't remove herself.
    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["bob"],
        &[],
        "alice",
        "alice_d1",
    );

    assert!(outcome.skipped_self_removal);
    assert!(outcome.removed.is_empty());
    assert!(outcome.added.is_empty());
    assert!(commit_data.is_none());
}

/// 6. Idempotence: reconcile twice with same desired state → second is no-op.
#[test]
fn reconcile_idempotent() {
    let conv_id = "01JTEST00000000000RECONCILE6";
    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    // First reconcile: remove bob.
    let (outcome1, _) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[],
        "alice",
        "alice_d1",
    );
    assert_eq!(outcome1.removed.len(), 1);

    // Second reconcile: same roster → no-op.
    let (outcome2, commit_data2) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[],
        "alice",
        "alice_d1",
    );
    assert!(outcome2.removed.is_empty());
    assert!(outcome2.added.is_empty());
    assert!(commit_data2.is_none());
}

/// 7. User in roster but not in tree and no KP → not added, no error.
#[test]
fn reconcile_no_kp_user_not_added() {
    let conv_id = "01JTEST00000000000RECONCILE7";
    let alice_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice", "bob"],
        &[],
        "alice",
        "alice_d1",
    );

    assert!(outcome.added.is_empty());
    assert!(outcome.removed.is_empty());
    assert!(commit_data.is_none());
}

/// 8. Add a second device for a user already in the tree.
#[test]
fn reconcile_add_second_device() {
    let conv_id = "01JTEST00000000000RECONCILE8";
    let alice_db = make_db();
    let alice_d2_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[("alice".into(), "alice_d2".into(), alice_d2_kp)],
        "alice",
        "alice_d1",
    );

    assert_eq!(outcome.added.len(), 1);
    assert_eq!(
        outcome.added[0],
        ("alice".to_string(), "alice_d2".to_string())
    );
    assert!(outcome.removed.is_empty());
    assert!(commit_data.is_some());
}

/// 9. Remove all leaves for a multi-device user removed from roster.
#[test]
fn reconcile_remove_multi_device_user() {
    let conv_id = "01JTEST00000000000RECONCILE9";
    let alice_db = make_db();
    let bob_d1_db = make_db();
    let bob_d2_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
    let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");
    let (_, welcome) = add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);
    join_via_welcome(&bob_d1_db, &welcome);
    join_via_welcome(&bob_d2_db, &welcome);

    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[],
        "alice",
        "alice_d1",
    );

    assert_eq!(outcome.removed.len(), 2);
    let removed_ids: std::collections::HashSet<(String, String)> =
        outcome.removed.into_iter().collect();
    assert!(removed_ids.contains(&("bob".to_string(), "bob_d1".to_string())));
    assert!(removed_ids.contains(&("bob".to_string(), "bob_d2".to_string())));
    assert!(outcome.added.is_empty());
    assert!(commit_data.is_some());
}

/// 10. End-to-end: reconcile removes a stale leaf, remaining members apply
/// the commit, and the removed member cannot decrypt new messages.
#[test]
fn reconcile_e2e_remove_then_communicate() {
    let conv_id = "01JTEST00000000000RECONCILEA";
    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);
    let _ = add_bob_commit;

    let carol_kp = gen_key_package_with_device(&carol_db, "carol", "carol_d1");
    let (add_carol_commit, carol_welcome) =
        add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    // Bob "leaves" — removed from roster. Alice reconciles.
    let (outcome, commit_data) = reconcile(
        &alice_db,
        conv_id,
        &["alice", "carol"],
        &[],
        "alice",
        "alice_d1",
    );

    assert_eq!(outcome.removed.len(), 1);
    assert_eq!(
        outcome.removed[0],
        ("bob".to_string(), "bob_d1".to_string())
    );

    // Carol applies the reconcile commit.
    let data = commit_data.unwrap();
    apply_commit(&carol_db, conv_id, &data.commit_bytes);

    // Alice and Carol can communicate.
    let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
    assert_eq!(
        try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap().0,
        b"bob is gone"
    );

    let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"confirmed").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap().0,
        b"confirmed"
    );

    // Bob cannot decrypt post-reconcile messages.
    assert!(
        try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
        "removed member must not decrypt after reconcile"
    );
}

// ── libcrux-provider / post-quantum readiness (#454 P1a) ─────────────────────
//
// These tests pin the properties that make the RustCrypto→libcrux crypto-provider
// swap (see `provider.rs`) safe to ship and useful for later PQ phases:
//   T1 — a RustCrypto app and a libcrux app share ONE classic MLS group, both
//        directions, so a STAGED desktop rollout can never split a group.
//   T2 — the libcrux `supports()` self-contradiction is documented and pinned,
//        so a future OpenMLS bump that starts consulting it fails loudly HERE
//        rather than bricking classic groups in production.
//   T3 — the PQ hybrid suite stays reachable through the exact provider Pollis
//        ships, with a structural proof the key exchange is genuinely hybrid.

/// Build a `CredentialWithKey` for `user`/`device` from an existing signer.
fn cred_with_key(sig: &SignatureKeyPair, user: &str, device: &str) -> CredentialWithKey {
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig.to_public_vec().into(),
        CS_CLASSIC.signature_algorithm(),
    )
    .unwrap();
    CredentialWithKey {
        credential: make_credential(user, device),
        signature_key: sig_pub.into(),
    }
}

/// A FULL two-party MLS flow in the CLASSIC suite between two clients backed by
/// DIFFERENT crypto providers (`creator` and `joiner` are distinct concrete
/// `OpenMlsProvider` types). Exercises, across the provider boundary:
///   - a KeyPackage built by the joiner and validated + consumed by the creator,
///   - Welcome / join,
///   - an application message in EACH direction,
///   - a self-update commit produced by the joiner and applied by the creator,
///   - `export_secret` on both sides, asserted EQUAL (the voice-frame-key path).
///
/// Bare in-memory storage is intentional — this is a crypto-backend
/// compatibility test, not a Pollis-storage test (P1a task note).
fn cross_provider_classic_flow<A: OpenMlsProvider, B: OpenMlsProvider>(
    creator: &A,
    joiner: &B,
) {
    let group_id = GroupId::from_slice(b"01JTEST0000000000CROSSPROV");

    // Creator ("alice") identity.
    let alice_sig = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    alice_sig.store(creator.storage()).unwrap();
    let alice_cwk = cred_with_key(&alice_sig, "alice", "alice_dev");

    // Joiner ("bob") identity + KeyPackage, built with the JOINER's provider.
    let bob_sig = SignatureKeyPair::new(CS_CLASSIC.signature_algorithm()).unwrap();
    bob_sig.store(joiner.storage()).unwrap();
    let bob_cwk = cred_with_key(&bob_sig, "bob", "bob_dev");
    let bob_kp_bytes = KeyPackage::builder()
        .build(CS_CLASSIC, joiner, &bob_sig, bob_cwk)
        .unwrap()
        .key_package()
        .tls_serialize_detached()
        .unwrap();

    // Creator creates the group.
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS_CLASSIC)
        .use_ratchet_tree_extension(true)
        .build();
    let mut alice_group = MlsGroup::new_with_group_id(
        creator,
        &alice_sig,
        &config,
        group_id.clone(),
        alice_cwk,
    )
    .unwrap();

    // Creator VALIDATES the joiner-built KeyPackage with ITS OWN crypto, then adds.
    let bob_kp = {
        let mut reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        kp_in
            .validate(creator.crypto(), ProtocolVersion::Mls10)
            .expect("joiner-built KeyPackage must validate under the creator's crypto")
    };
    let (_commit, welcome, _) = alice_group
        .add_members(creator, &alice_sig, &[bob_kp])
        .unwrap();
    alice_group.merge_pending_commit(creator).unwrap();

    // Joiner processes the Welcome and joins.
    let welcome_bytes = welcome.tls_serialize_detached().unwrap();
    let mut bob_group = {
        let mut reader: &[u8] = &welcome_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        let welcome = match msg_in.extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };
        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        StagedWelcome::new_from_welcome(joiner, &join_config, welcome, None)
            .unwrap()
            .into_group(joiner)
            .unwrap()
    };

    // Application message: creator → joiner.
    {
        let out = alice_group
            .create_message(creator, &alice_sig, b"hello from creator")
            .unwrap();
        let bytes = out.tls_serialize_detached().unwrap();
        let mut reader: &[u8] = &bytes;
        let pm = MlsMessageIn::tls_deserialize(&mut reader)
            .unwrap()
            .try_into_protocol_message()
            .unwrap();
        let processed = bob_group.process_message(joiner, pm).unwrap();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(m) => {
                assert_eq!(m.into_bytes(), b"hello from creator");
            }
            _ => panic!("expected application message creator→joiner"),
        }
    }

    // Application message: joiner → creator.
    {
        let out = bob_group
            .create_message(joiner, &bob_sig, b"hello from joiner")
            .unwrap();
        let bytes = out.tls_serialize_detached().unwrap();
        let mut reader: &[u8] = &bytes;
        let pm = MlsMessageIn::tls_deserialize(&mut reader)
            .unwrap()
            .try_into_protocol_message()
            .unwrap();
        let processed = alice_group.process_message(creator, pm).unwrap();
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(m) => {
                assert_eq!(m.into_bytes(), b"hello from joiner");
            }
            _ => panic!("expected application message joiner→creator"),
        }
    }

    // Self-update commit produced by the JOINER, applied by the CREATOR.
    {
        let bundle = bob_group
            .self_update(joiner, &bob_sig, LeafNodeParameters::default())
            .unwrap();
        let commit_bytes = bundle.commit().tls_serialize_detached().unwrap();
        bob_group.merge_pending_commit(joiner).unwrap();

        let mut reader: &[u8] = &commit_bytes;
        let pm = MlsMessageIn::tls_deserialize(&mut reader)
            .unwrap()
            .try_into_protocol_message()
            .unwrap();
        let processed = alice_group.process_message(creator, pm).unwrap();
        match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                alice_group.merge_staged_commit(creator, *staged).unwrap();
            }
            _ => panic!("expected staged commit from cross-provider self-update"),
        }
    }

    // Both sides now at the same epoch → `export_secret` MUST match exactly.
    // This is the voice-frame-key derivation path (`commands/voice_e2ee.rs`).
    let label = "pollis/voice/v1";
    let context = b"cross-provider-context";
    let alice_secret = alice_group
        .export_secret(creator.crypto(), label, context, 32)
        .unwrap();
    let bob_secret = bob_group
        .export_secret(joiner.crypto(), label, context, 32)
        .unwrap();
    assert_eq!(
        alice_secret, bob_secret,
        "cross-provider export_secret must be byte-identical (voice frame key)"
    );
}

/// T1. Cross-provider interop — the rollout-safety test.
///
/// Runs the full classic-suite two-party flow BOTH ways: a RustCrypto client
/// (the "old app") with a libcrux client (the "new app") in the SAME group,
/// old→new and new→old. This is what proves a STAGED desktop rollout cannot
/// split a group: old and new binaries coexist in one MLS group during the
/// update window. If this ever fails, the crypto swap in `provider.rs` is NOT
/// safe to ship and the rollout must be halted.
#[test]
fn cross_provider_rustcrypto_libcrux_interop() {
    // old (RustCrypto) creates → new (libcrux) joins.
    {
        let old_app = openmls_rust_crypto::OpenMlsRustCrypto::default();
        let new_app = openmls_libcrux_crypto::Provider::new()
            .expect("libcrux provider must initialise");
        cross_provider_classic_flow(&old_app, &new_app);
    }
    // new (libcrux) creates → old (RustCrypto) joins.
    {
        let new_app = openmls_libcrux_crypto::Provider::new()
            .expect("libcrux provider must initialise");
        let old_app = openmls_rust_crypto::OpenMlsRustCrypto::default();
        cross_provider_classic_flow(&new_app, &old_app);
    }
}

/// T2. The `supports()` trap.
///
/// The libcrux provider's `OpenMlsCrypto::supports()` returns
/// `Err(UnsupportedCiphersuite)` for Pollis's CURRENT classic AES-GCM suite —
/// its `supports()` demands `hpke_aead == ChaCha20Poly1305` — YET its own
/// `supported_ciphersuites()` lists that exact suite. This is an upstream
/// self-contradiction (openmls_libcrux_crypto 0.3.1, `crypto.rs`).
///
/// It is harmless TODAY only because NO code path in the full MLS flow consults
/// `supports()` (KeyPackage build, add, welcome, join, app messages, commits,
/// and export_secret all work — see T1 and every existing test above). This
/// test pins BOTH halves so the hazard cannot regress silently: if a future
/// OpenMLS bump starts calling `supports()` during any flow, classic groups
/// would brick in production — and THIS test must be the thing that fails first,
/// loudly, pointing right back at this note. Either an upstream FIX (supports()
/// starts returning Ok) or an upstream CHANGE (suite dropped from the list) trips
/// an assertion here.
#[test]
fn libcrux_supports_self_contradiction_is_pinned() {
    let db = make_db();
    let provider = PollisPqProvider::new(&db);
    let crypto = provider.crypto();

    // Half 1: `supports()` REJECTS the very suite Pollis ships on.
    assert!(
        matches!(crypto.supports(CS_CLASSIC), Err(CryptoError::UnsupportedCiphersuite)),
        "upstream changed: libcrux supports() now accepts our AES-GCM suite — \
         re-evaluate whether any flow consults supports() before relying on it"
    );

    // Half 2: …yet `supported_ciphersuites()` INCLUDES that same suite.
    assert!(
        crypto.supported_ciphersuites().contains(&CS_CLASSIC),
        "upstream changed: libcrux dropped our AES-GCM suite from \
         supported_ciphersuites() — classic groups may no longer be serviceable"
    );
}

/// T3. Hybrid suite reachability — landing the P0 proof as a permanent test.
///
/// Pollis does NOT yet use the post-quantum hybrid suite: every production call
/// site passes `CS_CLASSIC`, and choosing `CS_HYBRID` for a real conversation is
/// #454 P2. This test guards only that the CAPABILITY stays reachable through
/// the exact PQ provider Pollis would use (`PollisPqProvider` = libcrux crypto +
/// our `MlsStore`), since every later phase depends on it.
///
/// It asserts the suite is advertised, runs a full two-party round-trip on it,
/// and — the structural proof that the key exchange is GENUINELY hybrid and not
/// silently falling back to classical X25519 — asserts the joiner's
/// `hpke_init_key` is 1216 bytes: ML-KEM-768 public key (1184) + X25519 (32).
#[test]
fn hybrid_xwing_suite_reachable_and_genuinely_hybrid() {
    let alice_db = make_db();
    let bob_db = make_db();

    // The suite must be advertised by the PQ provider.
    {
        let provider = PollisPqProvider::new(&alice_db);
        assert!(
            provider.crypto().supported_ciphersuites().contains(&CS_HYBRID),
            "libcrux provider must advertise the PQ hybrid suite — later #454 \
             phases depend on it"
        );
    }

    // Bob builds a KeyPackage on the CS_HYBRID suite.
    let (bob_kp_bytes, bob_hpke_init_len) = {
        let provider = PollisPqProvider::new(&bob_db);
        let sig = SignatureKeyPair::new(CS_HYBRID.signature_algorithm()).unwrap();
        sig.store(provider.storage()).unwrap();
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig.to_public_vec().into(),
            CS_HYBRID.signature_algorithm(),
        )
        .unwrap();
        let cwk = CredentialWithKey {
            credential: make_credential("bob", "bob_dev"),
            signature_key: sig_pub.into(),
        };
        let bundle = KeyPackage::builder()
            .build(CS_HYBRID, &provider, &sig, cwk)
            .unwrap();
        let init_len = bundle.key_package().hpke_init_key().as_slice().len();
        (
            bundle.key_package().tls_serialize_detached().unwrap(),
            init_len,
        )
    };

    // Structural hybrid proof: ML-KEM-768 pubkey (1184) + X25519 (32) = 1216.
    // A classical X25519-only init key would be 32 bytes; anything but 1216
    // means the key exchange is NOT the hybrid we think it is.
    assert_eq!(
        bob_hpke_init_len, 1216,
        "hybrid hpke_init_key must be ML-KEM-768 (1184) + X25519 (32) = 1216 bytes"
    );

    // Alice creates the group on the hybrid suite and adds Bob.
    let alice_sig = SignatureKeyPair::new(CS_HYBRID.signature_algorithm()).unwrap();
    let (welcome_bytes, mut alice_group) = {
        let provider = PollisPqProvider::new(&alice_db);
        alice_sig.store(provider.storage()).unwrap();
        let sig_pub = OpenMlsSignaturePublicKey::new(
            alice_sig.to_public_vec().into(),
            CS_HYBRID.signature_algorithm(),
        )
        .unwrap();
        let cwk = CredentialWithKey {
            credential: make_credential("alice", "alice_dev"),
            signature_key: sig_pub.into(),
        };
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CS_HYBRID)
            .use_ratchet_tree_extension(true)
            .build();
        let group_id = GroupId::from_slice(b"01JTEST0000000000XWINGHYBRD");
        let mut group = MlsGroup::new_with_group_id(
            &provider,
            &alice_sig,
            &config,
            group_id,
            cwk,
        )
        .unwrap();

        let mut reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
        let kp = kp_in
            .validate(provider.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        let (_commit, welcome, _) =
            group.add_members(&provider, &alice_sig, &[kp]).unwrap();
        group.merge_pending_commit(&provider).unwrap();
        (welcome.tls_serialize_detached().unwrap(), group)
    };

    // Bob joins via the hybrid Welcome, then an application message must
    // decrypt end-to-end on the hybrid suite.
    //
    // Driven through openmls directly rather than through `try_mls_encrypt` /
    // `try_mls_decrypt`: those production helpers construct `PollisProvider`
    // (RustCrypto), which is CORRECT — no production path selects the hybrid
    // suite until #454 P2. Reaching for them here would only prove that
    // RustCrypto panics on X-Wing.
    {
        let provider = PollisPqProvider::new(&bob_db);
        let mut reader: &[u8] = &welcome_bytes;
        let welcome = match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected Welcome"),
        };
        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();
        let mut bob_group =
            StagedWelcome::new_from_welcome(&provider, &join_config, welcome, None)
                .unwrap()
                .into_group(&provider)
                .unwrap();
        assert_eq!(bob_group.ciphersuite(), CS_HYBRID);

        let ct = {
            let provider = PollisPqProvider::new(&alice_db);
            alice_group
                .create_message(&provider, &alice_sig, b"pq hello")
                .unwrap()
                .tls_serialize_detached()
                .unwrap()
        };
        let mut reader: &[u8] = &ct;
        let pm = MlsMessageIn::tls_deserialize(&mut reader)
            .unwrap()
            .try_into_protocol_message()
            .unwrap();
        let processed = bob_group.process_message(&provider, pm).unwrap();
        assert_eq!(
            parse_credential_user_id(processed.credential()),
            "alice"
        );
        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(m) => {
                assert_eq!(m.into_bytes(), b"pq hello");
            }
            _ => panic!("expected application message on the hybrid suite"),
        }
    }
}

/// T4. `validate_key_package` must not panic on a hybrid KeyPackage.
///
/// `validate_key_package` used to take a concrete `&RustCrypto`. That backend
/// `unimplemented!()`-PANICS on the X-Wing suite, so the moment #454 P2 mints
/// real hybrid KeyPackages, validating one would have aborted the process
/// rather than returning an error. The signature is now generic over
/// `OpenMlsCrypto`, so each caller passes the backend that matches the suite it
/// is serving — `PollisProvider` for classic, `PollisPqProvider` for hybrid.
///
/// Asserts the returned `KeyPackageRef` equals the one openmls computes, so this
/// proves validation actually happened rather than merely not crashing, and pins
/// the credential-mismatch path as a normal `Err` on both suites.
fn validate_key_package_round_trip(provider: &impl OpenMlsProvider, suite: Ciphersuite) {
    let sig = SignatureKeyPair::new(suite.signature_algorithm()).unwrap();
    sig.store(provider.storage()).unwrap();
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig.to_public_vec().into(),
        suite.signature_algorithm(),
    )
    .unwrap();
    let cwk = CredentialWithKey {
        credential: make_credential("alice", "alice_dev"),
        signature_key: sig_pub.into(),
    };
    let bundle = KeyPackage::builder().build(suite, provider, &sig, cwk).unwrap();
    let kp_bytes = bundle.key_package().tls_serialize_detached().unwrap();
    let expected_ref = bundle.key_package().hash_ref(provider.crypto()).unwrap();

    // The call that would have panicked under RustCrypto on the hybrid suite.
    let got = validate_key_package(&kp_bytes, "alice", provider.crypto())
        .unwrap_or_else(|e| panic!("validate failed on {suite:?}: {e}"));
    assert_eq!(
        got,
        hex::encode(expected_ref.as_slice()),
        "validate_key_package must return the real KeyPackageRef on {suite:?}"
    );

    assert!(
        validate_key_package(&kp_bytes, "mallory", provider.crypto()).is_err(),
        "credential mismatch must be rejected on {suite:?}"
    );
}

#[test]
fn validate_key_package_handles_both_suites() {
    let classic_db = make_db();
    validate_key_package_round_trip(&PollisProvider::new(&classic_db), CS_CLASSIC);

    let hybrid_db = make_db();
    validate_key_package_round_trip(&PollisPqProvider::new(&hybrid_db), CS_HYBRID);
}

/// T5. **The routing invariant.** `PollisProvider` — the provider EVERY
/// production path constructs — must be backed by RustCrypto, never libcrux.
///
/// This is not stylistic. `deny.toml` carries an ignore for RUSTSEC-2026-0211
/// (libcrux AES-GCM decryption uses a non-constant-time authentication-tag
/// check, unpatched in `libcrux-aesgcm`) whose stated reason is that Pollis
/// never routes AES-GCM through libcrux. Pollis's classic suite IS AES-128-GCM,
/// so if someone "simplifies" the two providers into one libcrux-backed
/// provider, that ignore silently becomes a lie and every message decrypt picks
/// up a timing side channel.
///
/// Type-level, so it cannot be satisfied by accident at runtime.
#[test]
fn classic_provider_never_routes_to_libcrux() {
    fn backend_of<P: OpenMlsProvider>(_: &P) -> &'static str {
        std::any::type_name::<P::CryptoProvider>()
    }

    let db = make_db();
    let classic = PollisProvider::new(&db);
    assert!(
        backend_of(&classic).contains("openmls_rust_crypto"),
        "PollisProvider must stay RustCrypto-backed (RUSTSEC-2026-0211: libcrux \
         AES-GCM decrypt is not constant-time and our classic suite is AES-GCM); \
         got {}",
        backend_of(&classic)
    );
    assert_eq!(
        CS_CLASSIC.aead_algorithm(),
        AeadType::Aes128Gcm,
        "the classic suite is AES-GCM — the premise of the routing split above"
    );

    // …and the PQ provider is the libcrux one, since it is the only backend
    // that implements X-Wing.
    let pq = PollisPqProvider::new(&db);
    assert!(
        backend_of(&pq).contains("libcrux"),
        "PollisPqProvider must stay libcrux-backed; got {}",
        backend_of(&pq)
    );
}

// ── #454 P1b: the ciphersuite is a real parameter, not a constant ────────────

/// The signature scheme is shared by both suites, which is why the many call
/// sites that only need `signature_algorithm()` take no suite argument and use
/// `SIGNATURE_SCHEME` instead (see `provider.rs`).
///
/// If a future suite ever signs with something other than Ed25519, this fails
/// first and points at every site that assumed otherwise — including the device
/// cross-signing cert, which certifies ONE key per device, not one per suite.
#[test]
fn signature_scheme_is_suite_invariant() {
    assert_eq!(CS_CLASSIC.signature_algorithm(), SIGNATURE_SCHEME);
    assert_eq!(CS_HYBRID.signature_algorithm(), SIGNATURE_SCHEME);
}

/// Drive a full two-party flow — create group, build KeyPackage, validate, add,
/// Welcome, join, application message — entirely through the PRODUCTION suite
/// seams (`create_mls_group_in_suite`, `build_key_package_in_suite`,
/// `load_group_with_signer`, `validate_key_package`) at an arbitrary suite.
///
/// Deliberately not hand-rolled openmls: the point is that the parametrisation
/// is real in the code Pollis actually ships, not merely that openmls can do it.
fn suite_seam_round_trip<CA, CB>(
    alice: &MlsProvider<'_, CA>,
    bob: &MlsProvider<'_, CB>,
    suite: Ciphersuite,
    conversation_id: &str,
) where
    CA: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
    CB: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(alice, conversation_id, "alice", "alice_dev", suite)
        .unwrap_or_else(|e| panic!("create group on {suite:?}: {e}"));

    let (_bob_ref, bob_kp_bytes) = build_key_package_in_suite(bob, "bob", "bob_dev", suite)
        .unwrap_or_else(|e| panic!("build key package on {suite:?}: {e}"));

    let (mut alice_group, alice_sig) =
        load_group_with_signer(alice, conversation_id).expect("reload alice's group");
    assert_eq!(
        alice_group.ciphersuite(),
        suite,
        "the group must be created in the suite the caller asked for"
    );

    // The production validator must accept the package on this suite (it is
    // generic over the backend precisely so the hybrid case is not a panic).
    validate_key_package(&bob_kp_bytes, "bob", alice.crypto())
        .unwrap_or_else(|e| panic!("validate key package on {suite:?}: {e}"));

    let welcome_bytes = {
        let mut reader: &[u8] = &bob_kp_bytes;
        let kp = KeyPackageIn::tls_deserialize(&mut reader)
            .unwrap()
            .validate(alice.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        let (_commit, welcome, _) = alice_group.add_members(alice, &alice_sig, &[kp]).unwrap();
        alice_group.merge_pending_commit(alice).unwrap();
        welcome.tls_serialize_detached().unwrap()
    };

    let mut reader: &[u8] = &welcome_bytes;
    let welcome = match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected a Welcome"),
    };
    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();
    let mut bob_group = StagedWelcome::new_from_welcome(bob, &join_config, welcome, None)
        .unwrap()
        .into_group(bob)
        .unwrap();
    assert_eq!(
        bob_group.ciphersuite(),
        suite,
        "the joiner must land in the same suite the creator chose"
    );

    let ct = alice_group
        .create_message(alice, &alice_sig, b"seam check")
        .unwrap()
        .tls_serialize_detached()
        .unwrap();
    let mut reader: &[u8] = &ct;
    let pm = MlsMessageIn::tls_deserialize(&mut reader)
        .unwrap()
        .try_into_protocol_message()
        .unwrap();
    let processed = bob_group.process_message(bob, pm).unwrap();
    assert_eq!(parse_credential_user_id(processed.credential()), "alice");
    match processed.into_content() {
        ProcessedMessageContent::ApplicationMessage(m) => {
            assert_eq!(m.into_bytes(), b"seam check");
        }
        _ => panic!("expected an application message on {suite:?}"),
    }
}

/// The classic suite still round-trips through the (now parametrised) seams,
/// and reports the classic code point. This is the no-behaviour-change half of
/// P1b: production passes `CS_CLASSIC` everywhere, so this is the path every
/// user is on.
#[test]
fn classic_suite_round_trips_through_the_production_seams() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisProvider::new(&alice_db);
    let bob = PollisProvider::new(&bob_db);

    suite_seam_round_trip(&alice, &bob, CS_CLASSIC, "01JTESTSEAMCLASSIC00000000");
    assert_eq!(u16::from(CS_CLASSIC), 0x0001);
}

/// The seam is REAL, not cosmetic: passing `CS_HYBRID` produces a genuinely
/// post-quantum group through the same production functions, with the PQ
/// provider that serves that suite.
///
/// No GROUP selects this yet — `init_mls_group` and every add path still pass
/// `CS_CLASSIC` (#454 P2 gives devices a hybrid KeyPackage pool but keeps every
/// group classic; making a real conversation hybrid is P3/P4). This test
/// guarantees those later phases have a working group seam to use.
#[test]
fn hybrid_suite_round_trips_through_the_production_seams() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisPqProvider::new(&alice_db);
    let bob = PollisPqProvider::new(&bob_db);

    suite_seam_round_trip(&alice, &bob, CS_HYBRID, "01JTESTSEAMHYBRID000000000");
    assert_eq!(u16::from(CS_HYBRID), 0x004D);
}

/// The two suites are genuinely different key exchanges, which is the entire
/// point of #454. Asserted structurally on the KeyPackage the production builder
/// emits: X25519 alone is a 32-byte HPKE init key, X-Wing is ML-KEM-768's 1184
/// plus X25519's 32.
#[test]
fn the_two_suites_produce_structurally_different_key_packages() {
    let classic_db = make_db();
    let hybrid_db = make_db();

    let classic = PollisProvider::new(&classic_db);
    let (_, classic_kp) =
        build_key_package_in_suite(&classic, "bob", "bob_dev", CS_CLASSIC).unwrap();
    let hybrid = PollisPqProvider::new(&hybrid_db);
    let (_, hybrid_kp) =
        build_key_package_in_suite(&hybrid, "bob", "bob_dev", CS_HYBRID).unwrap();

    // Each package is validated by the backend that serves its suite — the
    // classic one cannot validate an X-Wing package, it would have to implement
    // X-Wing to do so.
    fn init_key_len(bytes: &[u8], crypto: &impl openmls_traits::crypto::OpenMlsCrypto) -> usize {
        let mut reader: &[u8] = bytes;
        KeyPackageIn::tls_deserialize(&mut reader)
            .unwrap()
            .validate(crypto, ProtocolVersion::Mls10)
            .unwrap()
            .hpke_init_key()
            .as_slice()
            .len()
    }

    assert_eq!(
        init_key_len(&classic_kp, classic.crypto()),
        32,
        "the classic init key is a bare X25519 public key"
    );
    assert_eq!(
        init_key_len(&hybrid_kp, hybrid.crypto()),
        1216,
        "the hybrid init key must be ML-KEM-768 (1184) + X25519 (32)"
    );
}
