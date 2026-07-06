// ── Tests ─────────────────────────────────────────────────────────────────────

use super::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

// Re-export the private items from submodules into this `tests` module
// so the test bodies (which were originally `use super::*` from the
// single-file module) can keep referencing them by short name.
use super::group_state::load_group_with_signer;
use super::provider::CS;

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
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
        .unwrap();

    sig_keys
}

/// Generate a key package for `user_id` in `conn` and return the TLS bytes.
fn gen_key_package(conn: &rusqlite::Connection, user_id: &str) -> Vec<u8> {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS, &provider, &sig_keys, cred_with_key)
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
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
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
    let sig_keys = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS, &provider, &sig_keys, cred_with_key)
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
        CS.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS, &provider, sig_keys, cred_with_key)
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
        let sk = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
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
        let sk = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
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
