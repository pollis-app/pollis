use super::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use super::group_state::{create_mls_group_in_suite, load_group_with_signer};
use super::key_packages::build_key_package_in_suite;
use super::provider::{signature_scheme, MlsProvider, PollisProvider, CS_PQ};
use super::self_update::stage_self_update;

/// A suite that is deliberately **not** [`CS_PQ`], used only by the tests.
///
/// Exercises the not-current-suite paths (`migrate`, suite-mismatch refusals) for a future `CS_PQ` renumber.
///
/// `0x0001`, the pre-#454 suite, differs from `CS_PQ` in every dimension.
const CS_LEGACY: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

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

/// Synthetic device ID: one device per test user.
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
    let sig_keys = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_PQ.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS_PQ)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
        .unwrap();

    sig_keys
}

/// Generate a key package for `user_id` in `conn` and return the TLS bytes.
fn gen_key_package(conn: &rusqlite::Connection, user_id: &str) -> Vec<u8> {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, &test_device_id(user_id));
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_PQ.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_PQ, &provider, &sig_keys, cred_with_key)
        .unwrap();

    bundle.key_package().tls_serialize_detached().unwrap()
}

/// Alice creates a group, adds Bob, then Alice encrypts and Bob decrypts.
#[test]
fn encrypt_decrypt_roundtrip() {
    let conv_id = "01JTEST00000000000000000AB";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    let bob_kp_bytes = gen_key_package(&bob_db, "bob");

    let welcome_bytes: Vec<u8> = {
        let alice_provider = PollisProvider::new(&alice_db);
        let (mut alice_group, alice_signer) =
            load_group_with_signer(&alice_provider, conv_id, 0).unwrap();

        let mut kp_reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut kp_reader).unwrap();
        let kp = kp_in.validate(alice_provider.crypto(), ProtocolVersion::Mls10).unwrap();

        let (commit_msg, welcome_msg, _) =
            alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();

        alice_group.merge_pending_commit(&alice_provider).unwrap();

        let _ = commit_msg.tls_serialize_detached().unwrap();
        welcome_msg.tls_serialize_detached().unwrap()
    };

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

    let plaintext = b"hello mls";
    let ciphertext = try_mls_encrypt(&alice_db, conv_id, plaintext)
        .expect("try_mls_encrypt failed");

    let (decrypted, sender) = try_mls_decrypt(&bob_db, conv_id, &ciphertext)
        .expect("try_mls_decrypt failed");

    assert_eq!(decrypted, plaintext);
    // Attribution comes from the MLS credential (sealed sender).
    assert_eq!(sender, "alice");
}

/// Pad, encrypt, decrypt, strip: exact round-trip across sizes, and old unpadded sends still decode (#331).
#[test]
fn padded_text_roundtrip_through_mls() {
    use crate::commands::messages::framing;

    let conv_id = "01JTEST00000000000000000EF";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group(&alice_db, conv_id, "alice");
    let bob_kp_bytes = gen_key_package(&bob_db, "bob");

    let welcome_bytes: Vec<u8> = {
        let alice_provider = PollisProvider::new(&alice_db);
        let (mut alice_group, alice_signer) =
            load_group_with_signer(&alice_provider, conv_id, 0).unwrap();
        let mut kp_reader: &[u8] = &bob_kp_bytes;
        let kp_in = KeyPackageIn::tls_deserialize(&mut kp_reader).unwrap();
        let kp = kp_in.validate(alice_provider.crypto(), ProtocolVersion::Mls10).unwrap();
        let (_commit, welcome_msg, _) =
            alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();
        alice_group.merge_pending_commit(&alice_provider).unwrap();
        welcome_msg.tls_serialize_detached().unwrap()
    };

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

    // Padded path: exact round-trip and one ciphertext bucket.
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

    // Back-compat: `strip` leaves an old client's unpadded bytes untouched.
    let legacy_plain = "legacy unpadded message".as_bytes();
    let legacy_ct = try_mls_encrypt(&alice_db, conv_id, legacy_plain).expect("encrypt legacy");
    let (legacy_pt, _) = try_mls_decrypt(&bob_db, conv_id, &legacy_ct).expect("decrypt legacy");
    assert_eq!(framing::strip(&legacy_pt), legacy_plain);
}

/// A solo group's creator can encrypt, but decrypt returns None.
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

/// Alice adds a member to her group. Returns (commit_bytes, welcome_bytes).
fn add_member_to_group(
    adder_db: &rusqlite::Connection,
    conv_id: &str,
    kp_bytes: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let provider = PollisProvider::new(adder_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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
    let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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

/// Alice, Bob, and Carol are all in the same group. Each can encrypt a
/// message that the other two can decrypt.
#[test]
fn three_way_group_messaging() {
    let conv_id = "01JTEST00000000000000000EF";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    let _ = add_bob_commit;

    let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"hello from alice").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, conv_id, &alice_ct).unwrap().0, b"hello from alice");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap().0, b"hello from alice");

    let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello from bob").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &bob_ct).unwrap().0, b"hello from bob");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &bob_ct).unwrap().0, b"hello from bob");

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

    let pre_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"pre-removal").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &pre_remove_ct).unwrap().0,
        b"pre-removal"
    );

    let _remove_commit = remove_member(&alice_db, conv_id, "bob");

    let post_remove_ct = try_mls_encrypt(&alice_db, conv_id, b"secret").unwrap();

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

    let history_ct = try_mls_encrypt(&alice_db, conv_id, b"private history").unwrap();

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    assert!(
        try_mls_decrypt(&bob_db, conv_id, &history_ct).is_none(),
        "new member must not decrypt history from before they joined"
    );

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

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&bob_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);

    apply_commit(&alice_db, conv_id, &add_carol_commit);

    let _ = add_bob_commit;

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

/// After a removal, the rest still talk, the removed member cannot decrypt, and a new member can join.
#[test]
fn leave_group_remaining_members_communicate_then_new_member_joins() {
    let conv_id = "01JTEST0000000000000LEAVE1";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();
    let dave_db = make_db();

    create_group(&alice_db, conv_id, "alice");

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (add_bob_commit, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);
    let _ = add_bob_commit;

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"all three here").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap().0, b"all three here");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &pre_ct).unwrap().0, b"all three here");

    let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
    apply_commit(&carol_db, conv_id, &remove_bob_commit);

    let alice_ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &alice_ct).unwrap().0, b"bob is gone");

    let carol_ct = try_mls_encrypt(&carol_db, conv_id, b"confirmed").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &carol_ct).unwrap().0, b"confirmed");

    assert!(
        try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
        "removed bob must not decrypt"
    );

    let dave_kp = gen_key_package(&dave_db, "dave");
    let (add_dave_commit, dave_welcome) = add_member_to_group(&alice_db, conv_id, &dave_kp);
    join_via_welcome(&dave_db, &dave_welcome);
    apply_commit(&carol_db, conv_id, &add_dave_commit);

    let dave_ct = try_mls_encrypt(&dave_db, conv_id, b"dave here").unwrap();
    assert_eq!(try_mls_decrypt(&alice_db, conv_id, &dave_ct).unwrap().0, b"dave here");
    assert_eq!(try_mls_decrypt(&carol_db, conv_id, &dave_ct).unwrap().0, b"dave here");

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

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (add_carol_commit, carol_welcome) = add_member_to_group(&alice_db, conv_id, &carol_kp);
    join_via_welcome(&carol_db, &carol_welcome);
    apply_commit(&bob_db, conv_id, &add_carol_commit);

    let remove_bob_commit = remove_member(&alice_db, conv_id, "bob");
    apply_commit(&carol_db, conv_id, &remove_bob_commit);

    let ct = try_mls_encrypt(&alice_db, conv_id, b"bob is gone").unwrap();
    assert_eq!(
        try_mls_decrypt(&carol_db, conv_id, &ct).unwrap().0,
        b"bob is gone"
    );

    assert!(
        try_mls_decrypt(&bob_db, conv_id, &ct).is_none(),
        "removed Bob must not decrypt after key rotation"
    );
}

/// Account deletion: removing a user from every group rotates keys so they are locked out (#103).
#[test]
fn account_deletion_rotates_keys_for_remaining_members() {
    let group1 = "01JTEST000000000000ACCTDEL1";
    let group2 = "01JTEST000000000000ACCTDEL2";

    let alice_db = make_db();
    let bob_db = make_db();
    let carol_db = make_db();

    create_group(&alice_db, group1, "alice");

    let bob_kp1 = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome1) = add_member_to_group(&alice_db, group1, &bob_kp1);
    join_via_welcome(&bob_db, &bob_welcome1);

    let carol_kp1 = gen_key_package(&carol_db, "carol");
    let (add_carol_commit1, carol_welcome1) = add_member_to_group(&alice_db, group1, &carol_kp1);
    join_via_welcome(&carol_db, &carol_welcome1);
    apply_commit(&bob_db, group1, &add_carol_commit1);

    create_group(&alice_db, group2, "alice");

    let bob_kp2 = gen_key_package(&bob_db, "bob");
    let (_, bob_welcome2) = add_member_to_group(&alice_db, group2, &bob_kp2);
    join_via_welcome(&bob_db, &bob_welcome2);

    let pre_g1 = try_mls_encrypt(&alice_db, group1, b"pre-delete g1").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, group1, &pre_g1).unwrap().0, b"pre-delete g1");

    let pre_g2 = try_mls_encrypt(&alice_db, group2, b"pre-delete g2").unwrap();
    assert_eq!(try_mls_decrypt(&bob_db, group2, &pre_g2).unwrap().0, b"pre-delete g2");

    let remove_g1 = remove_member(&alice_db, group1, "bob");
    apply_commit(&carol_db, group1, &remove_g1);

    let _remove_g2 = remove_member(&alice_db, group2, "bob");

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

/// Create an MLS group with an explicit device_id in the credential.
fn create_group_with_device(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
) -> SignatureKeyPair {
    let provider = PollisProvider::new(conn);
    let sig_keys = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_PQ.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS_PQ)
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
    let sig_keys = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
    sig_keys.store(provider.storage()).unwrap();

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(
        sig_keys.to_public_vec().into(),
        CS_PQ.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_PQ, &provider, &sig_keys, cred_with_key)
        .unwrap();

    bundle.key_package().tls_serialize_detached().unwrap()
}

/// A key package signed with a pre-existing keypair (the stable per-device signer).
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
        CS_PQ.signature_algorithm(),
    ).unwrap();
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let bundle = KeyPackage::builder()
        .build(CS_PQ, &provider, sig_keys, cred_with_key)
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
    let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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

/// Alice has two devices in the group. Bob sends a message. Both Alice
/// devices decrypt it.
#[test]
fn multi_device_both_devices_decrypt() {
    let conv_id = "01JTEST00000000000MULTIDEV1";

    let alice_d1_db = make_db();
    let alice_d2_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");

    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (add_bob_commit, bob_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);
    let _ = add_bob_commit;

    let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
    let (add_d2_commit, alice_d2_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
    join_via_welcome(&alice_d2_db, &alice_d2_welcome);
    apply_commit(&bob_db, conv_id, &add_d2_commit);

    let bob_ct = try_mls_encrypt(&bob_db, conv_id, b"hello both alices").unwrap();

    assert_eq!(
        try_mls_decrypt(&alice_d1_db, conv_id, &bob_ct).unwrap().0,
        b"hello both alices"
    );
    assert_eq!(
        try_mls_decrypt(&alice_d2_db, conv_id, &bob_ct).unwrap().0,
        b"hello both alices"
    );
}

/// Two devices of one user added in a single commit both join via the same Welcome.
#[test]
fn multi_device_batch_add_single_commit() {
    let conv_id = "01JTEST00000000000MULTIDEV2";

    let alice_db = make_db();
    let bob_d1_db = make_db();
    let bob_d2_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    let bob_d1_kp = gen_key_package_with_device(&bob_d1_db, "bob", "bob_d1");
    let bob_d2_kp = gen_key_package_with_device(&bob_d2_db, "bob", "bob_d2");

    let (_commit, welcome) =
        add_members_batch(&alice_db, conv_id, &[bob_d1_kp, bob_d2_kp]);

    join_via_welcome(&bob_d1_db, &welcome);
    join_via_welcome(&bob_d2_db, &welcome);

    let ct = try_mls_encrypt(&alice_db, conv_id, b"hello bob devices").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_d1_db, conv_id, &ct).unwrap().0,
        b"hello bob devices"
    );
    assert_eq!(
        try_mls_decrypt(&bob_d2_db, conv_id, &ct).unwrap().0,
        b"hello bob devices"
    );

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

    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"before removal").unwrap();
    assert!(try_mls_decrypt(&bob_d1_db, conv_id, &pre_ct).is_some());
    assert!(try_mls_decrypt(&bob_d2_db, conv_id, &pre_ct).is_some());

    let _remove_commit = remove_member(&alice_db, conv_id, "bob");

    let post_ct = try_mls_encrypt(&alice_db, conv_id, b"after removal").unwrap();

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

    create_group_with_device(&alice_d1_db, conv_id, "alice", "alice_d1");
    let bob_kp = gen_key_package_with_device(&bob_db, "bob", "bob_d1");
    let (_, bob_welcome) = add_member_to_group(&alice_d1_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &bob_welcome);

    let history_ct = try_mls_encrypt(&bob_db, conv_id, b"history msg").unwrap();
    assert_eq!(
        try_mls_decrypt(&alice_d1_db, conv_id, &history_ct).unwrap().0,
        b"history msg"
    );

    let alice_d2_kp = gen_key_package_with_device(&alice_d2_db, "alice", "alice_d2");
    let (add_d2_commit, alice_d2_welcome) =
        add_member_to_group(&alice_d1_db, conv_id, &alice_d2_kp);
    join_via_welcome(&alice_d2_db, &alice_d2_welcome);
    apply_commit(&bob_db, conv_id, &add_d2_commit);

    assert!(
        try_mls_decrypt(&alice_d2_db, conv_id, &history_ct).is_none(),
        "second device must not decrypt messages from before it joined"
    );

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
/// Bob leaves locally (no remove commit) and is re-invited; his stable signing key makes a plain `add_members` fail `validate_key_uniqueness`, so the fix is a combined remove+add.
#[test]
fn reinvite_with_stable_signing_key_handles_stale_leaf() {
    let conv_id = "01JTEST000000000REINVITE01";

    let alice_db = make_db();
    let bob_db = make_db();

    create_group_with_device(&alice_db, conv_id, "alice", "alice_d1");

    // A stable signer reused across enrollments.
    let bob_signer = {
        let provider = PollisProvider::new(&bob_db);
        let sk = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
        sk.store(provider.storage()).unwrap();
        sk
    };

    let bob_kp_v1 =
        gen_key_package_with_existing_signer(&bob_db, "bob", "bob_d1", &bob_signer);
    let (_add1_commit, welcome1) = add_members_batch(&alice_db, conv_id, &[bob_kp_v1]);
    join_via_welcome(&bob_db, &welcome1);

    let pre_ct = try_mls_encrypt(&alice_db, conv_id, b"first life").unwrap();
    assert_eq!(
        try_mls_decrypt(&bob_db, conv_id, &pre_ct).unwrap().0,
        b"first life"
    );

    // Bob "leaves": his local state is dropped, but Alice keeps his leaf.

    let bob_db_v2 = make_db();
    let bob_signer_v2 = {
        let provider = PollisProvider::new(&bob_db_v2);
        let sk = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
        sk.store(provider.storage()).unwrap();
        sk
    };

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
        let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();
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

    let welcome2_bytes: Vec<u8> = {
        let provider = PollisProvider::new(&alice_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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

    join_via_welcome(&bob_db_v2, &welcome2_bytes);

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

/// Call `reconcile_group_mls_core` with validated raw KP bytes.
fn reconcile(
    actor_db: &rusqlite::Connection,
    conv_id: &str,
    roster_user_ids: &[&str],
    available_kp_bytes: &[(String, String, Vec<u8>)],
    actor_user_id: &str,
    actor_device_id: &str,
) -> (ReconcileOutcome, Option<ReconcileCommitData>) {
    let provider = PollisProvider::new(actor_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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

    let (outcome1, _) = reconcile(
        &alice_db,
        conv_id,
        &["alice"],
        &[],
        "alice",
        "alice_d1",
    );
    assert_eq!(outcome1.removed.len(), 1);

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

/// 10. End-to-end: reconcile removes a stale leaf; the removed member cannot decrypt.
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

    let data = commit_data.unwrap();
    apply_commit(&carol_db, conv_id, &data.commit_bytes);

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

    assert!(
        try_mls_decrypt(&bob_db, conv_id, &alice_ct).is_none(),
        "removed member must not decrypt after reconcile"
    );
}

/// The shipped provider's PQ suite is post-quantum in BOTH halves:
/// - the joiner's `hpke_init_key` is 1216 bytes (ML-KEM-768 + X25519);
/// - the leaf signature key is 1312 bytes (ML-DSA-44, #668).
#[test]
fn pq_suite_reachable_and_genuinely_post_quantum() {
    let alice_db = make_db();
    let bob_db = make_db();

    {
        let provider = PollisProvider::new(&alice_db);
        assert!(
            provider.crypto().supported_ciphersuites().contains(&CS_PQ),
            "the shipped provider must advertise the PQ suite"
        );
    }

    let (bob_kp_bytes, bob_hpke_init_len, bob_sig_pub_len) = {
        let provider = PollisProvider::new(&bob_db);
        let sig = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
        sig.store(provider.storage()).unwrap();
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig.to_public_vec().into(),
            CS_PQ.signature_algorithm(),
        )
        .unwrap();
        let cwk = CredentialWithKey {
            credential: make_credential("bob", "bob_dev"),
            signature_key: sig_pub.into(),
        };
        let bundle = KeyPackage::builder()
            .build(CS_PQ, &provider, &sig, cwk)
            .unwrap();
        let init_len = bundle.key_package().hpke_init_key().as_slice().len();
        let sig_pub_len = bundle
            .key_package()
            .leaf_node()
            .signature_key()
            .as_slice()
            .len();
        (
            bundle.key_package().tls_serialize_detached().unwrap(),
            init_len,
            sig_pub_len,
        )
    };

    // Hybrid proof: ML-KEM-768 (1184) + X25519 (32) = 1216.
    assert_eq!(
        bob_hpke_init_len, 1216,
        "hybrid hpke_init_key must be ML-KEM-768 (1184) + X25519 (32) = 1216 bytes"
    );

    // PQ-signature proof (#668): ML-DSA-44 is 1312 bytes, Ed25519 32.
    assert_eq!(
        bob_sig_pub_len, 1312,
        "PQ leaf signature key must be an ML-DSA-44 public key (1312 bytes)"
    );

    let alice_sig = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
    let (welcome_bytes, mut alice_group) = {
        let provider = PollisProvider::new(&alice_db);
        alice_sig.store(provider.storage()).unwrap();
        let sig_pub = OpenMlsSignaturePublicKey::new(
            alice_sig.to_public_vec().into(),
            CS_PQ.signature_algorithm(),
        )
        .unwrap();
        let cwk = CredentialWithKey {
            credential: make_credential("alice", "alice_dev"),
            signature_key: sig_pub.into(),
        };
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CS_PQ)
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

    // Driven through openmls directly; `suite_seam_round_trip` covers the production seams.
    {
        let provider = PollisProvider::new(&bob_db);
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
        assert_eq!(bob_group.ciphersuite(), CS_PQ);

        let ct = {
            let provider = PollisProvider::new(&alice_db);
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

/// T4. `validate_key_package` must not panic on a post-quantum KeyPackage.
///
/// Generic over `OpenMlsCrypto`, so an unimplemented suite is an `Err`, not a panic.
///
/// Checks the returned `KeyPackageRef` and the credential-mismatch `Err`.
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
fn validate_key_package_handles_any_suite() {
    let legacy_db = make_db();
    validate_key_package_round_trip(&PollisProvider::new(&legacy_db), CS_LEGACY);

    let pq_db = make_db();
    validate_key_package_round_trip(&PollisProvider::new(&pq_db), CS_PQ);
}

/// **One backend, and it stays RustCrypto.**
///
/// Type-level: reintroducing a backend (#454 RUSTSEC-2026-0211 routing) has to face this test.
#[test]
fn mls_backend_is_rustcrypto() {
    fn backend_of<P: OpenMlsProvider>(_: &P) -> &'static str {
        std::any::type_name::<P::CryptoProvider>()
    }

    let db = make_db();
    let provider = PollisProvider::new(&db);
    assert!(
        backend_of(&provider).contains("openmls_rust_crypto"),
        "PollisProvider must stay RustCrypto-backed; got {}",
        backend_of(&provider)
    );

    // Asserted because `CS_PQ` is a provisional code point.
    let advertised = provider.crypto().supported_ciphersuites();
    assert!(
        advertised.contains(&CS_PQ),
        "RustCrypto must advertise the Pollis suite; got {advertised:?}"
    );

    // ChaCha20-Poly1305, not AES-GCM: a return to AES-GCM re-opens RUSTSEC-2026-0211.
    assert_eq!(
        CS_PQ.aead_algorithm(),
        AeadType::ChaCha20Poly1305,
        "the shipped suite must stay ChaCha20-Poly1305"
    );
}

/// The signature scheme is derived from the suite, not a constant.
///
/// It is read off a *stored group*, which stays on its old suite until migrated, so both answers are pinned.
#[test]
fn signature_scheme_per_suite() {
    assert_eq!(signature_scheme(CS_LEGACY), SignatureScheme::ED25519);
    assert_eq!(signature_scheme(CS_PQ), SignatureScheme::MLDSA44);
}

/// A full two-party flow through the PRODUCTION suite seams at an arbitrary suite.
fn suite_seam_round_trip<CA, CB>(
    alice: &MlsProvider<'_, CA>,
    bob: &MlsProvider<'_, CB>,
    suite: Ciphersuite,
    conversation_id: &str,
) where
    CA: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
    CB: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(alice, conversation_id, 0, "alice", "alice_dev", suite)
        .unwrap_or_else(|e| panic!("create group on {suite:?}: {e}"));

    let (_bob_ref, bob_kp_bytes) = build_key_package_in_suite(bob, "bob", "bob_dev", suite)
        .unwrap_or_else(|e| panic!("build key package on {suite:?}: {e}"));

    let (mut alice_group, alice_sig) =
        load_group_with_signer(alice, conversation_id, 0).expect("reload alice's group");
    assert_eq!(
        alice_group.ciphersuite(),
        suite,
        "the group must be created in the suite the caller asked for"
    );

    // Generic over the backend so the hybrid case is not a panic.
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

/// A suite that is not `CS_PQ` still round-trips through the production seams.
///
/// Proves the parametrisation is real: a `CS_PQ` renumber needs these seams on the old suite.
#[test]
fn a_non_current_suite_round_trips_through_the_production_seams() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisProvider::new(&alice_db);
    let bob = PollisProvider::new(&bob_db);

    suite_seam_round_trip(&alice, &bob, CS_LEGACY, "01JTESTSEAMLEGACY000000000");
    assert_eq!(u16::from(CS_LEGACY), 0x0001);
}

/// The seam is REAL, not cosmetic: passing `CS_PQ` produces a genuinely
/// post-quantum group through the same production functions.
/// The code point is asserted literally: the DS stores it as a wire label (`CIPHERSUITE_PQ`).
#[test]
fn pq_suite_round_trips_through_the_production_seams() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisProvider::new(&alice_db);
    let bob = PollisProvider::new(&bob_db);

    suite_seam_round_trip(&alice, &bob, CS_PQ, "01JTESTSEAMPQ00000000000000");
    assert_eq!(u16::from(CS_PQ), 0x0052);
}

/// The suite argument selects a different key exchange: 32-byte X25519 vs 1216-byte X-Wing init key.
#[test]
fn the_suite_parameter_selects_a_different_key_exchange() {
    let legacy_db = make_db();
    let pq_db = make_db();

    let legacy = PollisProvider::new(&legacy_db);
    let (_, legacy_kp) =
        build_key_package_in_suite(&legacy, "bob", "bob_dev", CS_LEGACY).unwrap();
    let hybrid = PollisProvider::new(&pq_db);
    let (_, hybrid_kp) =
        build_key_package_in_suite(&hybrid, "bob", "bob_dev", CS_PQ).unwrap();

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
        init_key_len(&legacy_kp, legacy.crypto()),
        32,
        "the legacy init key is a bare X25519 public key"
    );
    assert_eq!(
        init_key_len(&hybrid_kp, hybrid.crypto()),
        1216,
        "the hybrid init key must be ML-KEM-768 (1184) + X25519 (32)"
    );
}

/// A group's ciphersuite is readable from STORAGE ALONE (`MlsGroup::load` needs only a `StorageProvider`), which `migrate` relies on.
#[test]
fn a_groups_suite_is_readable_from_storage_without_a_crypto_backend() {
    for (suite, conv) in [
        (CS_LEGACY, "01JTESTSUITEREADLEGACY0000"),
        (CS_PQ, "01JTESTSUITEREADPQ00000000"),
    ] {
        let db = make_db();
        create_mls_group_in_suite(&PollisProvider::new(&db), conv, 0, "alice", "alice_dev", suite)
            .unwrap();

        assert_eq!(
            super::provider::stored_group_ciphersuite(&db, conv),
            Some(suite),
            "the stored group's suite must be readable through MlsStore alone"
        );
    }
}

/// Pins `welcome_ciphersuite`'s RFC 9420 §12.4.3.1 parse against real Welcomes on both suites.
#[test]
fn welcome_suite_is_readable_on_both_suites() {
    for (suite, conv) in [
        (CS_LEGACY, "01JTESTWELCOMESUITELEGACY0"),
        (CS_PQ, "01JTESTWELCOMESUITEPQ00000"),
    ] {
        let alice_db = make_db();
        let bob_db = make_db();
        let welcome_bytes = welcome_for_suite(
            &PollisProvider::new(&alice_db),
            &PollisProvider::new(&bob_db),
            suite,
            conv,
        );

        let mut reader: &[u8] = &welcome_bytes;
        let welcome = match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
            MlsMessageBodyIn::Welcome(w) => w,
            _ => panic!("expected a Welcome"),
        };
        assert_eq!(
            super::provider::welcome_ciphersuite(&welcome),
            Some(suite),
            "a joiner must be able to route a Welcome to the right provider"
        );
    }
}

/// Build a real Welcome by adding bob to a group alice creates, both on `suite`.
fn welcome_for_suite<CA, CB>(
    alice: &MlsProvider<'_, CA>,
    bob: &MlsProvider<'_, CB>,
    suite: Ciphersuite,
    conversation_id: &str,
) -> Vec<u8>
where
    CA: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
    CB: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(alice, conversation_id, 0, "alice", "alice_dev", suite).unwrap();
    let (_, bob_kp_bytes) = build_key_package_in_suite(bob, "bob", "bob_dev", suite).unwrap();
    let (mut alice_group, alice_sig) = load_group_with_signer(alice, conversation_id, 0).unwrap();

    let mut reader: &[u8] = &bob_kp_bytes;
    let kp = KeyPackageIn::tls_deserialize(&mut reader)
        .unwrap()
        .validate(alice.crypto(), ProtocolVersion::Mls10)
        .unwrap();
    let (_commit, welcome, _) = alice_group.add_members(alice, &alice_sig, &[kp]).unwrap();
    alice_group.merge_pending_commit(alice).unwrap();
    welcome.tls_serialize_detached().unwrap()
}

/// #454's acceptance criterion, encoded as an invalid state that cannot be
/// created: **a post-quantum group can never contain a member whose KeyPackage
/// is not post-quantum.**
///
/// Asserted here: openmls itself refuses the add (reconcile and `stage_reconcile_commit` also check).
///
/// The DS is untrusted, so a hostile one offering a weaker suite is still the attack; `CS_LEGACY` stands in for it.
#[test]
fn a_group_cannot_admit_a_key_package_from_another_suite() {
    let alice_db = make_db();
    let bob_db = make_db();

    let alice = PollisProvider::new(&alice_db);
    create_mls_group_in_suite(&alice, "01JTESTNODOWNGRADE00000000", 0, "alice", "alice_dev", CS_PQ)
        .unwrap();
    let (mut alice_group, alice_sig) =
        load_group_with_signer(&alice, "01JTESTNODOWNGRADE00000000", 0).unwrap();
    assert_eq!(alice_group.ciphersuite(), CS_PQ);

    let bob = PollisProvider::new(&bob_db);
    let (_, bob_kp_bytes) =
        build_key_package_in_suite(&bob, "bob", "bob_dev", CS_LEGACY).unwrap();
    let mut reader: &[u8] = &bob_kp_bytes;
    let bob_kp = KeyPackageIn::tls_deserialize(&mut reader)
        .unwrap()
        .validate(alice.crypto(), ProtocolVersion::Mls10)
        .expect("the package is well-formed; it is the SUITE that must reject it");
    assert_eq!(bob_kp.ciphersuite(), CS_LEGACY);

    let err = alice_group
        .add_members(&alice, &alice_sig, &[bob_kp])
        .expect_err("adding an off-suite leaf to a PQ group must be impossible");
    crate::diag!("[test] PQ group rejected the off-suite KeyPackage: {err}");

    // A rejected add must leave nothing staged.
    alice_group.clear_pending_commit(alice.storage()).unwrap();
    assert_eq!(alice_group.ciphersuite(), CS_PQ, "the group stays post-quantum");
}

/// The claim #666 rests on, measured rather than asserted: **commit size grows
/// with the number of members only while their leaves are unmerged.**
///
/// An unmerged leaf costs one HPKE ciphertext per commit until it commits itself.
///
/// Unmerged, doubling the group roughly doubles the commit; merged, it adds a couple of ciphertexts.
///
/// A ratio, not a byte ceiling, run on both suites; ceilings live in [`pq_payloads_stay_under_their_ceilings`].
#[test]
fn self_update_turns_linear_commit_growth_into_logarithmic() {
    for suite in [CS_LEGACY, CS_PQ] {
        let (small_unmerged, small_merged) = commit_growth_probe(suite, 8, "01JT666GROWTHSMALL00000000");
        let (large_unmerged, large_merged) = commit_growth_probe(suite, 16, "01JT666GROWTHLARGE00000000");

        let unmerged_delta = large_unmerged as i64 - small_unmerged as i64;
        let merged_delta = large_merged as i64 - small_merged as i64;
        crate::diag!(
            "[test] {suite:?}: 8→16 members costs +{unmerged_delta} B unmerged, +{merged_delta} B merged \
             (8: {small_unmerged}→{small_merged} B, 16: {large_unmerged}→{large_merged} B)"
        );

        assert!(
            large_merged < large_unmerged,
            "{suite:?}: self-updates must shrink the commit ({large_merged} !< {large_unmerged})"
        );

        // Unmerged, 8→16 adds eight ciphertexts; merged, two.
        assert!(
            merged_delta * 3 < unmerged_delta,
            "{suite:?}: doubling the group must cost far less once every leaf is merged \
             (+{merged_delta} B merged vs +{unmerged_delta} B unmerged)"
        );
    }
}

/// Size of a commit alice issues into a `members`-strong group, with leaves unmerged then merged.
fn commit_growth_probe(suite: Ciphersuite, members: usize, conversation_id: &str) -> (usize, usize) {
    let dbs: Vec<rusqlite::Connection> = (0..members).map(|_| make_db()).collect();
    let providers: Vec<_> = dbs.iter().map(PollisProvider::new).collect();
    measure_commit_growth(&providers, suite, conversation_id)
}

fn measure_commit_growth<C>(
    providers: &[MlsProvider<'_, C>],
    suite: Ciphersuite,
    conversation_id: &str,
) -> (usize, usize)
where
    C: OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let alice = &providers[0];
    create_mls_group_in_suite(alice, conversation_id, 0, "m0", "m0_dev", suite).unwrap();

    let mut kps = Vec::new();
    for (i, p) in providers.iter().enumerate().skip(1) {
        let (_, bytes) =
            build_key_package_in_suite(p, &format!("m{i}"), &format!("m{i}_dev"), suite).unwrap();
        let mut reader: &[u8] = &bytes;
        kps.push(
            KeyPackageIn::tls_deserialize(&mut reader)
                .unwrap()
                .validate(alice.crypto(), ProtocolVersion::Mls10)
                .unwrap(),
        );
    }
    let welcome_bytes = {
        let (mut group, signer) = load_group_with_signer(alice, conversation_id, 0).unwrap();
        let (_commit, welcome, _) = group.add_members(alice, &signer, &kps).unwrap();
        group.merge_pending_commit(alice).unwrap();
        welcome.tls_serialize_detached().unwrap()
    };
    for p in providers.iter().skip(1) {
        join_welcome_in_suite(p, &welcome_bytes);
    }

    let unmerged = probe_self_update_size(alice, conversation_id);

    // Every member rotates its own leaf (the post-join self-update).
    for i in 1..providers.len() {
        let (_, commit_bytes, _) = stage_self_update(&providers[i], conversation_id, 0)
            .unwrap()
            .expect("a joined member can always self-update");
        for (j, p) in providers.iter().enumerate() {
            if i == j {
                // The committer merges its own staged commit explicitly (#1079).
                let (mut group, _) = load_group_with_signer(p, conversation_id, 0).unwrap();
                group.merge_pending_commit(p).unwrap();
            } else {
                apply_commit_in_suite(p, conversation_id, &commit_bytes);
            }
        }
    }

    let merged = probe_self_update_size(alice, conversation_id);
    (unmerged, merged)
}

/// Stage a self-update, note how many bytes it is, and roll it back so the
/// measurement itself does not move the tree.
fn probe_self_update_size<C>(provider: &MlsProvider<'_, C>, conversation_id: &str) -> usize
where
    C: OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let (_, commit_bytes, _) = stage_self_update(provider, conversation_id, 0)
        .unwrap()
        .expect("group exists");
    // A raw load with no merge: the measurement must not advance the epoch.
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .unwrap()
        .expect("group exists");
    group.clear_pending_commit(provider.storage()).unwrap();
    commit_bytes.len()
}

fn join_welcome_in_suite<C>(provider: &MlsProvider<'_, C>, welcome_bytes: &[u8])
where
    C: OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let mut reader: &[u8] = welcome_bytes;
    let welcome = match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected Welcome"),
    };
    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();
    StagedWelcome::new_from_welcome(provider, &join_config, welcome, None)
        .unwrap()
        .into_group(provider)
        .unwrap();
}

fn apply_commit_in_suite<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    commit_bytes: &[u8],
) where
    C: OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .unwrap()
        .expect("group must exist");
    let mut reader: &[u8] = commit_bytes;
    let protocol_msg = MlsMessageIn::tls_deserialize(&mut reader)
        .unwrap()
        .try_into_protocol_message()
        .unwrap();
    let processed = group.process_message(provider, protocol_msg).unwrap();
    if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
        group.merge_staged_commit(provider, *staged).unwrap();
    }
}

/// A self-update must rotate the leaf key; pins `force_self_update(true)`.
#[test]
fn a_self_update_replaces_our_own_leaf_key() {
    let db = make_db();
    let provider = PollisProvider::new(&db);
    let conv = "01JT666LEAFROTATES00000000";
    create_mls_group_in_suite(&provider, conv, 0, "alice", "alice_dev", CS_PQ).unwrap();

    let leaf_key_of = |p: &PollisProvider| {
        let (group, _) = load_group_with_signer(p, conv, 0).unwrap();
        group.own_leaf_node().unwrap().encryption_key().clone()
    };

    let before = leaf_key_of(&provider);
    stage_self_update(&provider, conv, 0).unwrap().expect("group exists");
    {
        let (mut group, _) = load_group_with_signer(&provider, conv, 0).unwrap();
        group.merge_pending_commit(&provider).unwrap();
    }
    let after = leaf_key_of(&provider);

    assert_ne!(
        before, after,
        "a self-update that leaves the leaf key in place heals nothing"
    );
}

/// Jitter must be stable per conversation, or a restarting device drifts past its deadline.
#[test]
fn self_update_jitter_is_stable_and_bounded() {
    let a = super::self_update::jitter_for("01JT666JITTERCONVA00000000");
    let b = super::self_update::jitter_for("01JT666JITTERCONVB00000000");

    assert_eq!(
        a,
        super::self_update::jitter_for("01JT666JITTERCONVA00000000"),
        "the same conversation must always get the same offset"
    );
    assert_ne!(a, b, "different conversations must not rotate in lockstep");
    for j in [a, b] {
        assert!(
            j >= chrono::Duration::zero() && j < super::self_update::SELF_UPDATE_JITTER,
            "jitter must stay inside its window, got {j}"
        );
    }
}

/// #454 P3 item 4: absolute byte ceilings for the post-quantum suite.
///
/// Pins the constants; [`self_update_turns_linear_commit_growth_into_logarithmic`] pins the shape.
///
/// Measured after #668: KeyPackage 8,670 B, Welcome 15,241 B, merged 8-member commit 14,839 B; ceilings ~1.4× above.
#[test]
fn pq_payloads_stay_under_their_ceilings() {
    let kp_db = make_db();
    let (_, hybrid_kp) =
        build_key_package_in_suite(&PollisProvider::new(&kp_db), "alice", "alice_dev", CS_PQ)
            .unwrap();
    let legacy_db = make_db();
    let (_, legacy_kp) =
        build_key_package_in_suite(&PollisProvider::new(&legacy_db), "alice", "alice_dev", CS_LEGACY)
            .unwrap();
    crate::diag!(
        "[test] KeyPackage: classical {} B, post-quantum {} B",
        legacy_kp.len(),
        hybrid_kp.len()
    );
    assert!(hybrid_kp.len() < 12_288, "hybrid KeyPackage: {} B", hybrid_kp.len());

    let (wa, wb) = (make_db(), make_db());
    let welcome = welcome_for_suite(
        &PollisProvider::new(&wa),
        &PollisProvider::new(&wb),
        CS_PQ,
        "01JT454CEILINGWELCOME00000",
    );
    crate::diag!("[test] hybrid Welcome (2 members): {} B", welcome.len());
    assert!(welcome.len() < 21_504, "hybrid Welcome: {} B", welcome.len());

    // Measured with every leaf merged, as the fleet runs (#666).
    let (unmerged, merged) = commit_growth_probe(CS_PQ, 8, "01JT454CEILINGCOMMIT000000");
    crate::diag!("[test] hybrid commit at 8 members: {unmerged} B unmerged, {merged} B merged");
    assert!(merged < 20_480, "hybrid commit at 8 members: {merged} B");
}

// #680: `apply_one_commit` classifies openmls errors by enum, and no former `Stop` site wedges a group.

use super::group_state::{
    apply_one_commit, build_external_commit, export_group_info_blob,
    CommitApply, LineageProvenance, RecoverReason,
};
use openmls::prelude::group_info::VerifiableGroupInfo;

/// The cross-signing material a device has when it can load none: an empty,
/// unpinned directory, which verdicts every signer `Unverifiable`.
///
/// With `roster: None` it ADOPTS, so tests about something other than provenance do not trip that gate.
fn no_identities() -> super::device::IdentityDirectory {
    super::device::IdentityDirectory::new(Vec::new())
}

/// Layer 1, the pure classifier: an unrecognised error recovers rather than wedging (#680).
mod commit_recovery_classification {
    use super::super::group_state::{classify_process_message_error, RecoverReason};
    use openmls::prelude::*;

    fn classify(e: ProcessMessageError<()>) -> RecoverReason {
        classify_process_message_error(&e)
    }

    #[test]
    fn use_after_eviction_maps_to_evicted() {
        assert_eq!(
            classify(ProcessMessageError::GroupStateError(
                MlsGroupStateError::UseAfterEviction
            )),
            RecoverReason::Evicted
        );
    }

    #[test]
    fn own_commit_mismatch_maps_to_forked_tree() {
        // Our pending state diverged from what landed: rebuild, do NOT merge it.
        assert_eq!(
            classify(ProcessMessageError::InvalidCommit(
                StageCommitError::OwnCommitMismatch
            )),
            RecoverReason::ForkedTree
        );
    }

    #[test]
    fn other_stage_commit_errors_map_to_forked_tree() {
        for e in [
            StageCommitError::EpochMismatch,
            StageCommitError::ConfirmationTagMismatch,
            StageCommitError::WrongPlaintextContentType,
        ] {
            assert_eq!(
                classify(ProcessMessageError::InvalidCommit(e)),
                RecoverReason::ForkedTree
            );
        }
    }

    #[test]
    fn validation_errors_map_to_forked_tree() {
        for e in [
            ValidationError::WrongEpoch,
            ValidationError::WrongGroupId,
            ValidationError::UnknownMember,
        ] {
            assert_eq!(
                classify(ProcessMessageError::ValidationError(e)),
                RecoverReason::ForkedTree
            );
        }
    }

    #[test]
    fn wire_format_and_external_sender_errors_are_unclassified() {
        for e in [
            ProcessMessageError::IncompatibleWireFormat,
            ProcessMessageError::UnauthorizedExternalApplicationMessage,
            ProcessMessageError::UnauthorizedExternalCommitMessage,
            ProcessMessageError::UnsupportedProposalType,
        ] {
            assert_eq!(classify(e), RecoverReason::Unclassified);
        }
    }

    #[test]
    fn unrecognised_error_recovers_conservatively_never_wedges() {
        // THE #680 guarantee: any unrecognised error must recover, never wedge.
        assert_eq!(
            classify(ProcessMessageError::StorageError(())),
            RecoverReason::Unclassified
        );
    }
}

/// Load a member's stored MLS group (generation 0), or `None` if it was deleted.
fn load_local_group(db: &rusqlite::Connection, conv: &str) -> Option<MlsGroup> {
    let provider = PollisProvider::new(db);
    let group_id = GroupId::from_slice(conv.as_bytes());
    MlsGroup::load(provider.storage(), &group_id).unwrap()
}

/// This member's current epoch.
fn member_epoch(db: &rusqlite::Connection, conv: &str) -> u64 {
    load_local_group(db, conv).expect("group must exist").epoch().as_u64()
}

/// This member's epoch authenticator: equal at the same epoch iff group state matches.
fn member_auth(db: &rusqlite::Connection, conv: &str) -> Vec<u8> {
    load_local_group(db, conv)
        .expect("group must exist")
        .epoch_authenticator()
        .as_slice()
        .to_vec()
}

/// Stage a member-add commit without merging it. Returns `(commit_bytes, welcome_bytes)`.
fn stage_add_without_merge(
    adder_db: &rusqlite::Connection,
    conv: &str,
    kp_bytes: &[u8],
) -> (Vec<u8>, Vec<u8>) {
    let provider = PollisProvider::new(adder_db);
    let (mut group, signer) = load_group_with_signer(&provider, conv, 0).unwrap();
    let mut reader: &[u8] = kp_bytes;
    let kp_in = KeyPackageIn::tls_deserialize(&mut reader).unwrap();
    let kp = kp_in.validate(provider.crypto(), ProtocolVersion::Mls10).unwrap();
    let (commit_msg, welcome_msg, _) = group.add_members(&provider, &signer, &[kp]).unwrap();
    (
        commit_msg.tls_serialize_detached().unwrap(),
        welcome_msg.tls_serialize_detached().unwrap(),
    )
}

/// Self-update and merge; returns the commit bytes.
fn self_update_and_merge(db: &rusqlite::Connection, conv: &str) -> Vec<u8> {
    let provider = PollisProvider::new(db);
    let (_epoch_before, commit_bytes, _gi) =
        stage_self_update(&provider, conv, 0).unwrap().expect("self-update produced a commit");
    let group_id = GroupId::from_slice(conv.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id).unwrap().unwrap();
    group.merge_pending_commit(&provider).unwrap();
    commit_bytes
}

/// Drive `apply_one_commit` (generation 0) for `member` against `commit_bytes`.
fn apply_one(db: &rusqlite::Connection, conv: &str, epoch: i64, commit_bytes: &[u8]) -> CommitApply {
    let provider = PollisProvider::new(db);
    apply_one_commit(&provider, conv, 0, epoch, commit_bytes)
}

/// Two-member group at epoch 1, plus a valid third-party commit alice made at epoch 1 for bob to apply.
fn two_members_plus_valid_commit(
    conv: &str,
    alice_db: &rusqlite::Connection,
    bob_db: &rusqlite::Connection,
    carol_db: &rusqlite::Connection,
) -> (Vec<u8>, Vec<u8>) {
    create_group(alice_db, conv, "alice");
    let bob_kp = gen_key_package(bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(alice_db, conv, &bob_kp);
    join_via_welcome(bob_db, &welcome1);
    assert_eq!(member_epoch(alice_db, conv), 1);
    assert_eq!(member_epoch(bob_db, conv), 1);

    let carol_kp = gen_key_package(carol_db, "carol");
    let (commit2, welcome2) = add_member_to_group(alice_db, conv, &carol_kp);
    assert_eq!(member_epoch(alice_db, conv), 2);
    (commit2, welcome2)
}

/// Site 1 (`MlsGroup::load` → `Ok(None)`): no local group must recover, not wedge.
#[test]
fn stop_site_group_missing_recovers() {
    let conv = "01JT680MISSINGGROUP0000000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    let empty_db = make_db();
    let outcome = apply_one(&empty_db, conv, 1, &commit2);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::GroupMissing)),
        "a missing local group must recover via external-join, not wedge"
    );
}

/// Site 2 (`MlsMessageIn::tls_deserialize` fails): corrupt / truncated commit
/// bytes must recover, not wedge, deleting the group so the member can rejoin.
#[test]
fn stop_site_corrupt_commit_bytes_recovers_and_can_advance() {
    let conv = "01JT680CORRUPTBYTES000000A";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    let truncated = &commit2[..commit2.len() / 2];
    let outcome = apply_one(&bob_db, conv, 1, truncated);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::MalformedCommit)),
        "truncated commit bytes must recover, got {outcome:?}",
    );
    assert!(
        load_local_group(&bob_db, conv).is_none(),
        "recovery must delete the local group so the rejoin path rebuilds it",
    );

    // …and recovery is real: rejoin, then apply alice's NEXT commit (#680).
    assert_rejoin_then_next_commit_works(&alice_db, &bob_db, conv);
}

/// Site 3 (`try_into_protocol_message` fails): bytes that deserialize as an MLS
/// message but are NOT a protocol message (here, a Welcome) must recover, not wedge.
#[test]
fn stop_site_non_protocol_message_recovers() {
    let conv = "01JT680NONPROTOMESSAGE0000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (_commit2, welcome2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    // A Welcome deserializes as `MlsMessageIn` but is not a `ProtocolMessage`.
    let outcome = apply_one(&bob_db, conv, 1, &welcome2);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::NotAProtocolMessage)),
        "a non-protocol message must recover, got {outcome:?}",
    );
    assert!(load_local_group(&bob_db, conv).is_none(), "group must be deleted for rejoin");
}

/// A stale commit re-fed after it applied is `WrongEpoch`: recover, never wedge.
#[test]
fn already_merged_epoch_commit_recovers() {
    let conv = "01JT680ALREADYMERGED00000A";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    assert!(matches!(apply_one(&bob_db, conv, 1, &commit2), CommitApply::Applied { .. }));
    assert_eq!(member_epoch(&bob_db, conv), 2);

    // Re-feeding the stale epoch-1 commit at epoch 2 must recover, not wedge.
    let outcome = apply_one(&bob_db, conv, 1, &commit2);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::ForkedTree)),
        "a commit for an already-merged epoch must recover, got {outcome:?}",
    );
}

/// Two members commit concurrently at epoch 1 and diverge; bob is then fed alice's next commit, which must recover.
///
/// openmls rejects the diverged tree while staging (`ForkedTree`), before `merge_staged_commit`.
#[test]
fn diverged_tree_commit_recovers() {
    let conv = "01JT680DIVERGEDTREE00000AB";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (_commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);
    let conv = "01JT680DIVERGEDTREE00000CD";
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);

    // Concurrent epoch-1 commits diverge onto distinct epoch-2 trees.
    let _alice_fork = self_update_and_merge(&alice_db, conv);
    let _bob_fork = self_update_and_merge(&bob_db, conv);
    assert_eq!(member_epoch(&alice_db, conv), 2);
    assert_eq!(member_epoch(&bob_db, conv), 2);
    assert_ne!(
        member_auth(&alice_db, conv),
        member_auth(&bob_db, conv),
        "the two members must have genuinely diverged trees"
    );

    let alice_next = self_update_and_merge(&alice_db, conv);
    let outcome = apply_one(&bob_db, conv, 2, &alice_next);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::ForkedTree)),
        "a commit against a diverged local tree must recover, got {outcome:?}",
    );
    assert!(load_local_group(&bob_db, conv).is_none(), "group must be deleted for rejoin");
}

/// #411/#680: openmls surfaces a committer's OWN replayed commit as `OwnPrivateMessage` content; `apply_one_commit` must merge the pending commit and converge.
#[test]
fn own_pending_commit_is_adopted_and_converges() {
    let conv = "01JT680OWNPENDINGCOMMIT000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);
    assert_eq!(member_epoch(&alice_db, conv), 1);

    // Alice stages but does not merge: the #411 lost-response state.
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (own_commit, welcome2) = stage_add_without_merge(&alice_db, conv, &carol_kp);
    assert_eq!(member_epoch(&alice_db, conv), 1, "alice still at epoch 1 (unmerged)");
    assert!(
        load_local_group(&alice_db, conv).unwrap().pending_commit().is_some(),
        "alice holds a pending commit",
    );

    // Replaying her own commit must adopt it.
    let outcome = apply_one(&alice_db, conv, 1, &own_commit);
    assert!(
        matches!(outcome, CommitApply::Applied { .. }),
        "own commit replay must be adopted (Applied), got {outcome:?}",
    );
    assert_eq!(member_epoch(&alice_db, conv), 2, "alice must advance to the real merged epoch");
    assert!(
        load_local_group(&alice_db, conv).unwrap().pending_commit().is_none(),
        "the pending commit must be merged, not left dangling",
    );

    // Bob applies it and carol joins: all three converge.
    apply_commit(&bob_db, conv, &own_commit);
    join_via_welcome(&carol_db, &welcome2);
    assert_eq!(member_epoch(&alice_db, conv), member_epoch(&bob_db, conv));
    assert_eq!(member_epoch(&alice_db, conv), member_epoch(&carol_db, conv));
    let a = member_auth(&alice_db, conv);
    assert_eq!(a, member_auth(&bob_db, conv), "alice and bob trees must match");
    assert_eq!(a, member_auth(&carol_db, conv), "alice and carol trees must match");

    let ct = try_mls_encrypt(&alice_db, conv, b"post-recovery").expect("encrypt");
    let (bob_pt, _) = try_mls_decrypt(&bob_db, conv, &ct).expect("bob decrypts");
    let (carol_pt, _) = try_mls_decrypt(&carol_db, conv, &ct).expect("carol decrypts");
    assert_eq!(bob_pt, b"post-recovery");
    assert_eq!(carol_pt, b"post-recovery");
}

/// Prove recovery: external-join `owner`'s GroupInfo, then apply its NEXT commit and decrypt.
fn assert_rejoin_then_next_commit_works(
    owner_db: &rusqlite::Connection,
    joiner_db: &rusqlite::Connection,
    conv: &str,
) {
    let gi_bytes = {
        let provider = PollisProvider::new(owner_db);
        export_group_info_blob(&provider, conv, 0).unwrap().expect("owner exports GroupInfo").1
    };
    let vgi: VerifiableGroupInfo = {
        let mut reader: &[u8] = &gi_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).unwrap();
        match msg_in.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            _ => panic!("expected GroupInfo"),
        }
    };

    let identities = no_identities();
    let ext_commit = {
        let provider = PollisProvider::new(joiner_db);
        let (commit, _gi) = build_external_commit(
            &provider,
            conv,
            0,
            "bob",
            &test_device_id("bob"),
            CS_PQ,
            vgi,
            &LineageProvenance {
                identities: &identities,
                roster: None,
            },
        )
        .expect("external join builds");
        commit
    };
    assert!(load_local_group(joiner_db, conv).is_some(), "rejoin must rebuild the local group");

    apply_commit(owner_db, conv, &ext_commit);

    let next = self_update_and_merge(owner_db, conv);
    apply_commit(joiner_db, conv, &next);
    let ct = try_mls_encrypt(owner_db, conv, b"after recovery").expect("owner encrypts");
    let (pt, _) = try_mls_decrypt(joiner_db, conv, &ct).expect("rejoined member decrypts");
    assert_eq!(pt, b"after recovery", "the rejoined group must apply the next commit");
}

/// Build a two-member group and hand back both members' DBs.
fn alice_and_bob(conv_id: &str) -> (rusqlite::Connection, rusqlite::Connection) {
    let alice_db = make_db();
    let bob_db = make_db();
    create_group(&alice_db, conv_id, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_commit, welcome) = add_member_to_group(&alice_db, conv_id, &bob_kp);
    join_via_welcome(&bob_db, &welcome);
    (alice_db, bob_db)
}

/// #875 regression guard: a run of envelopes sealed at one epoch must cost ONE
/// group load, not one per message.
///
/// A shape guard: before #875 each envelope reloaded the group; the one UPDATE per message persists the ratchet (forward secrecy) and must stay.
#[test]
fn a_run_of_envelopes_loads_the_group_once() {
    let conv_id = "01JTEST0000000000000000BAT";
    let (alice_db, bob_db) = alice_and_bob(conv_id);

    const N: usize = 10;
    let cts: Vec<Vec<u8>> = (0..N)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("m{i}").as_bytes()).unwrap())
        .collect();

    // Widen the sender-ratchet window (#1526) outside the measurement.
    drop(MlsDecryptor::open(&bob_db, conv_id).expect("group is stored"));

    let before = crate::signal::mls_storage::counters::snapshot();
    {
        let mut decryptor = MlsDecryptor::open(&bob_db, conv_id).expect("group is stored");
        for (i, ct) in cts.iter().enumerate() {
            let (pt, sender) = decryptor.decrypt(ct).expect("decrypt");
            assert_eq!(pt, format!("m{i}").as_bytes(), "batched decrypt must return the same plaintext");
            assert_eq!(sender, "alice");
        }
    }
    let (reads, writes) = crate::signal::mls_storage::counters::since(before);

    // One amortised load, so reads stay far below the old 12 per message.
    assert!(
        reads < 2 * N as u64,
        "expected roughly one group load for the whole run, got {reads} reads for {N} messages \
         — the per-envelope group reload is back"
    );
    // Exactly one secret-tree persist per message.
    assert_eq!(writes, N as u64, "one message_secrets write per decrypt, no more and no fewer");
}

/// The same run through [`try_mls_decrypt`], one decryptor per call.
#[test]
fn single_shot_decrypt_still_works_and_costs_a_full_load() {
    let conv_id = "01JTEST0000000000000000SIN";
    let (alice_db, bob_db) = alice_and_bob(conv_id);

    let ct = try_mls_encrypt(&alice_db, conv_id, b"solo").unwrap();
    let before = crate::signal::mls_storage::counters::snapshot();
    let (pt, sender) = try_mls_decrypt(&bob_db, conv_id, &ct).expect("decrypt");
    let (reads, _writes) = crate::signal::mls_storage::counters::since(before);
    assert_eq!(pt, b"solo");
    assert_eq!(sender, "alice");
    // The cost of ONE load — the figure the batched test amortises away.
    assert!(reads >= 8, "a cold decrypt loads the whole group; got {reads} reads");
}

/// Prints the before/after figures behind #875 rather than asserting them, so
/// the numbers in `.codesight/wiki/mls.md` can be re-derived on any machine:
/// `cargo test -p pollis-core --no-default-features --features test-harness
/// --lib measure_decrypt -- --nocapture`.
///
/// Statement counts are exact; timings are a debug, in-memory build.
#[test]
fn measure_decrypt_statement_count() {
    let conv_id = "01JTEST0000000000000000MEA";
    let (alice_db, bob_db) = alice_and_bob(conv_id);

    const N: usize = 50;
    let cts: Vec<Vec<u8>> = (0..N)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("m{i}").as_bytes()).unwrap())
        .collect();

    let before = crate::signal::mls_storage::counters::snapshot();
    let t0 = std::time::Instant::now();
    {
        let mut d = MlsDecryptor::open(&bob_db, conv_id).unwrap();
        for ct in &cts {
            d.decrypt(ct).expect("decrypt");
        }
    }
    let batched_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let (reads, writes) = crate::signal::mls_storage::counters::since(before);
    println!("BATCHED  : reads={reads} writes={writes} => per-message reads={:.1} time={batched_ms:.2}ms", reads as f64 / N as f64);

    let cts2: Vec<Vec<u8>> = (0..N)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("n{i}").as_bytes()).unwrap())
        .collect();
    let before = crate::signal::mls_storage::counters::snapshot();
    let t1 = std::time::Instant::now();
    for ct in &cts2 {
        try_mls_decrypt(&bob_db, conv_id, ct).expect("decrypt");
    }
    let percall_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let (reads2, writes2) = crate::signal::mls_storage::counters::since(before);
    println!("PER-CALL : reads={reads2} writes={writes2} => per-message reads={:.1} time={percall_ms:.2}ms", reads2 as f64 / N as f64);
}

// #1161 C2 / H5 / H4: the commit log is attacker-writable, since the DS does not parse commit bytes.

/// C2: a non-commit row must be damage, never a free advance.
///
/// Pre-fix it returned `Applied` while the epoch stood still, so later envelopes were lost.
#[test]
fn an_application_message_in_the_commit_log_is_damage_not_a_free_advance() {
    let conv = "01JT1161APPMSGINCOMMITSLOT";
    let (alice_db, bob_db) = alice_and_bob(conv);
    assert_eq!(member_epoch(&bob_db, conv), 1, "precondition: bob is at epoch 1");

    let envelope = try_mls_encrypt(&alice_db, conv, b"not a commit").expect("alice seals");

    let outcome = apply_one(&bob_db, conv, 1, &envelope);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::NotACommit)),
        "a non-commit row must recover, not advance the replay counter, got {outcome:?}",
    );
}

/// H5: one of our OWN envelopes, replayed into a commit slot, must not be
/// mistaken for our own commit.
///
/// openmls returns `OwnPrivateMessage` for any `PrivateMessage` from our leaf index, so pre-fix this merged a staged commit the log never accepted (#1079).
#[test]
fn our_own_envelope_replayed_into_a_commit_slot_never_merges_a_staged_commit() {
    let conv = "01JT1161OWNENVELOPEREPLAY0";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);

    let own_envelope = try_mls_encrypt(&alice_db, conv, b"lunch?").expect("alice seals");

    // Alice stages a commit and does not adopt it (#411).
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (_own_commit, _welcome2) = stage_add_without_merge(&alice_db, conv, &carol_kp);
    assert_eq!(member_epoch(&alice_db, conv), 1, "precondition: alice unmerged at epoch 1");

    let outcome = apply_one(&alice_db, conv, 1, &own_envelope);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::NotACommit)),
        "our own envelope in a commit slot must not read as our own commit, got {outcome:?}",
    );
}

/// C2's second half: `Applied` reports the epoch the group actually reached.
#[test]
fn applied_reports_the_epoch_the_group_actually_reached() {
    let conv = "01JT1161APPLIEDEPOCHREADS0";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    match apply_one(&bob_db, conv, 1, &commit2) {
        CommitApply::Applied { epoch_after, .. } => {
            assert_eq!(epoch_after, 2, "the commit at epoch 1 lands the group at epoch 2");
            assert_eq!(
                epoch_after,
                member_epoch(&bob_db, conv),
                "the reported epoch must be read back off the merged group"
            );
        }
        other => panic!("a valid commit must apply, got {other:?}"),
    }
}

/// H4: a leaf grafted by an EXTERNAL JOIN is reported for cross-signing exactly
/// like one an Add proposal introduced.
///
/// An external commit carries no Add, so pre-fix the leaf was never verdicted.
#[test]
fn an_externally_joined_leaf_is_reported_for_cross_signing() {
    let conv = "01JT1161EXTERNALJOINLEAF00";
    let (alice_db, bob_db) = alice_and_bob(conv);
    let carol_db = make_db();

    let gi_bytes = {
        let provider = PollisProvider::new(&alice_db);
        export_group_info_blob(&provider, conv, 0).unwrap().expect("GroupInfo").1
    };
    let vgi: VerifiableGroupInfo = {
        let mut reader: &[u8] = &gi_bytes;
        match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            _ => panic!("expected GroupInfo"),
        }
    };
    let identities = no_identities();
    let ext_commit = {
        let provider = PollisProvider::new(&carol_db);
        build_external_commit(
            &provider,
            conv,
            0,
            "carol",
            &test_device_id("carol"),
            CS_PQ,
            vgi,
            &LineageProvenance {
                identities: &identities,
                roster: None,
            },
        )
        .expect("external join builds")
        .0
    };

    match apply_one(&bob_db, conv, 1, &ext_commit) {
        CommitApply::Applied { adds, .. } => {
            assert_eq!(
                adds.len(),
                1,
                "only the joined leaf is new — a commit's ordinary UpdatePath rotates \
                 encryption keys, not credentials or signature keys; got {adds:?}"
            );
            assert_eq!(adds[0].user_id, "carol", "the externally-joined leaf must be verdicted");
            assert_eq!(adds[0].device_id, test_device_id("carol"));
        }
        other => panic!("the external commit must apply, got {other:?}"),
    }
}

/// #1161 M7: two leaves claiming ONE `(user, device)` must not be
/// unrepresentable in reconcile's model while being representable in the tree.
///
/// openmls rejects duplicate keys but not duplicate CREDENTIALS, so reconcile must track every leaf index.
#[test]
fn a_duplicate_credential_leaf_is_evicted_rather_than_shadowed() {
    let conv = "01JT1161DUPLICATELEAF00000";
    let (alice_db, bob_db, rogue_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");

    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, _w1) = add_member_to_group(&alice_db, conv, &bob_kp);

    // A second leaf with the same credential and its own key.
    let rogue_kp = gen_key_package(&rogue_db, "bob");
    let (_c2, _w2) = add_member_to_group(&alice_db, conv, &rogue_kp);

    let claims: Vec<(String, String)> = load_local_group(&alice_db, conv)
        .unwrap()
        .members()
        .map(|m| {
            (
                parse_credential_user_id(&m.credential),
                parse_credential_device_id(&m.credential).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        claims.iter().filter(|(u, _)| u == "bob").count(),
        2,
        "precondition: the tree holds two leaves claiming one device — {claims:?}"
    );

    let (outcome, data) = reconcile(
        &alice_db,
        conv,
        &["alice", "bob"],
        &[],
        "alice",
        &test_device_id("alice"),
    );
    assert!(data.is_some(), "the duplicate must produce a removal commit, not a no-op");
    assert!(
        outcome.removed.contains(&("bob".to_string(), test_device_id("bob"))),
        "the duplicate leaf must be named in the removal set, got {:?}",
        outcome.removed
    );

    let after: Vec<(String, String)> = load_local_group(&alice_db, conv)
        .unwrap()
        .members()
        .map(|m| {
            (
                parse_credential_user_id(&m.credential),
                parse_credential_device_id(&m.credential).unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        after.iter().filter(|(u, _)| u == "bob").count(),
        1,
        "exactly one leaf may claim bob's device after the reconcile — {after:?}"
    );
    assert!(
        after.iter().any(|(u, _)| u == "alice"),
        "the committer's own leaf survives — {after:?}"
    );
}

/// The other half of M7: removing that user takes BOTH leaves.
#[test]
fn removing_a_user_takes_every_leaf_claiming_their_device() {
    let conv = "01JT1161DUPLICATEREMOVE000";
    let (alice_db, bob_db, rogue_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, _w1) = add_member_to_group(&alice_db, conv, &bob_kp);
    let rogue_kp = gen_key_package(&rogue_db, "bob");
    let (_c2, _w2) = add_member_to_group(&alice_db, conv, &rogue_kp);

    let (_outcome, data) = reconcile(
        &alice_db,
        conv,
        &["alice"],
        &[],
        "alice",
        &test_device_id("alice"),
    );
    assert!(data.is_some(), "removing bob must produce a commit");

    let after: Vec<String> = load_local_group(&alice_db, conv)
        .unwrap()
        .members()
        .map(|m| parse_credential_user_id(&m.credential))
        .collect();
    assert_eq!(after, vec!["alice".to_string()], "no leaf of bob's may survive — {after:?}");
}

/// #1161 M6: an external join must refuse a GroupInfo for a different group.
///
/// The GroupInfo comes from the DS, so a substituted one could destroy a group and join another under its name.
#[test]
fn an_external_join_refuses_a_group_info_for_another_group() {
    let target = "01JT1161EXTJOINPINTARGET00";
    let elsewhere = "01JT1161EXTJOINPINELSEWHR0";

    let (alice_db, _bob_db) = alice_and_bob(elsewhere);
    let gi_bytes = {
        let provider = PollisProvider::new(&alice_db);
        export_group_info_blob(&provider, elsewhere, 0).unwrap().expect("GroupInfo").1
    };
    let vgi: VerifiableGroupInfo = {
        let mut reader: &[u8] = &gi_bytes;
        match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            _ => panic!("expected GroupInfo"),
        }
    };

    // Carol holds the group she meant to rejoin and is handed the wrong GroupInfo.
    let carol_db = make_db();
    create_group(&carol_db, target, "carol");
    let epoch_before = member_epoch(&carol_db, target);

    let provider = PollisProvider::new(&carol_db);
    let identities = no_identities();
    let err = build_external_commit(
        &provider,
        target,
        0,
        "carol",
        &test_device_id("carol"),
        CS_PQ,
        vgi,
        &LineageProvenance {
            identities: &identities,
            roster: None,
        },
    )
    .expect_err("a GroupInfo for another group must be refused");
    assert!(
        err.to_string().contains("different group"),
        "the refusal must name the mismatch, got {err}"
    );

    // The refusal leaves her local group untouched.
    assert_eq!(
        member_epoch(&carol_db, target),
        epoch_before,
        "a refused external join must not have deleted or replaced the local group"
    );
}

// #1161 M6 residual: a GroupId is not a capability, so a hostile DS could build its own successor lineage; these tests gate adoption.

use super::device::{IdentityDirectory, LeafVerdict};

/// The MLS signature public key of `user_id`'s leaf in the group `db` holds for
/// `conv` — the key the roster's cross-signing material has to certify.
fn leaf_signature_key(db: &rusqlite::Connection, conv: &str, user_id: &str) -> Vec<u8> {
    load_local_group(db, conv)
        .expect("group must exist")
        .members()
        .find(|m| parse_credential_user_id(&m.credential) == user_id)
        .expect("member must be in the tree")
        .signature_key
        .to_vec()
}

/// A pinned directory in which `(user_id, device_id)` is certified — by a real
/// account key, over a real v2 device cert — as holding `pq_pub`.
///
/// Pass the honest leaf's key; any OTHER key verdicts `Uncertified`.
fn directory_certifying(user_id: &str, device_id: &str, pq_pub: &[u8]) -> IdentityDirectory {
    use crate::commands::account_identity::{device_cert_signed_payload, AccountSigningKey};
    use base64::Engine as _;
    use ml_dsa::{Keypair as _, Signer as _};
    use pollis_api::reads::{AddedIdentity, DeviceCertRow};

    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let account = AccountSigningKey::from_seed(&[42u8; 32].into());
    let ed_pub = vec![7u8; 32];
    let payload = device_cert_signed_payload(device_id, &ed_pub, pq_pub, 1, 1_700_000_000).unwrap();
    let cert = account.sign(&payload).encode().to_vec();
    IdentityDirectory::new(vec![AddedIdentity {
        user_id: user_id.to_string(),
        account_id_pub: Some(b64(&account.verifying_key().encode())),
        devices: vec![DeviceCertRow {
            device_id: device_id.to_string(),
            device_cert: Some(b64(&cert)),
            cert_issued_at: Some("1700000000".to_string()),
            cert_identity_version: Some(1),
            mls_signature_pub: Some(b64(&ed_pub)),
            revoked_at: None,
            mls_signature_pub_pq: Some(b64(pq_pub)),
        }],
    }])
    .into_pinned()
}

/// A pinned directory that KNOWS `user_id` but has none of their cert columns —
/// the ordinary mid-publish state, which verdicts `Unverifiable`.
fn directory_without_certs(user_id: &str, device_id: &str) -> IdentityDirectory {
    use pollis_api::reads::{AddedIdentity, DeviceCertRow};

    IdentityDirectory::new(vec![AddedIdentity {
        user_id: user_id.to_string(),
        account_id_pub: None,
        devices: vec![DeviceCertRow {
            device_id: device_id.to_string(),
            device_cert: None,
            cert_issued_at: None,
            cert_identity_version: None,
            mls_signature_pub: None,
            revoked_at: None,
            mls_signature_pub_pq: None,
        }],
    }])
    .into_pinned()
}

/// Read the GroupInfo `db` would publish for `(conv, generation)`.
fn published_group_info(
    db: &rusqlite::Connection,
    conv: &str,
    generation: i64,
) -> VerifiableGroupInfo {
    let bytes = {
        let provider = PollisProvider::new(db);
        export_group_info_blob(&provider, conv, generation)
            .unwrap()
            .expect("GroupInfo")
            .1
    };
    let mut reader: &[u8] = &bytes;
    match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::GroupInfo(gi) => gi,
        _ => panic!("expected GroupInfo"),
    }
}

/// The attack: a DS-built group under the successor's name, signed by a leaf whose key the claimed member never certified.
///
/// Every #1166 gate passes; only the cross-signing chain, rooted in a key the DS lacks, does not.
#[test]
fn an_external_join_refuses_a_lineage_built_by_a_forged_signer() {
    let conv = "01JT1167FORGEDLINEAGE00000";
    let alice_device = test_device_id("alice");

    // The honest conversation, and the key the roster certifies for alice.
    let alice_db = make_db();
    create_group(&alice_db, conv, "alice");
    let honest_key = leaf_signature_key(&alice_db, conv, "alice");
    let identities = directory_certifying("alice", &alice_device, &honest_key);
    assert_eq!(
        identities.leaf_verdict("alice", &alice_device, &honest_key, signature_scheme(CS_PQ)),
        LeafVerdict::Certified,
        "precondition: the honest leaf is the one the roster certifies"
    );

    // The DS stands up its OWN group under the successor's name, signing with a
    // leaf that claims alice's credential and its own key.
    let ds_db = make_db();
    {
        let provider = PollisProvider::new(&ds_db);
        create_mls_group_in_suite(&provider, conv, 1, "alice", &alice_device, CS_PQ).unwrap();
    }
    let forged = published_group_info(&ds_db, conv, 1);
    assert_eq!(
        forged.group_id(),
        &super::generation::mls_group_id(conv, 1),
        "precondition: the forgery passes the #1166 GroupId pin — that is the gap"
    );

    let carol_db = make_db();
    let roster: std::collections::HashSet<String> =
        ["alice".to_string(), "carol".to_string()].into_iter().collect();
    let provider = PollisProvider::new(&carol_db);
    let err = build_external_commit(
        &provider,
        conv,
        1,
        "carol",
        &test_device_id("carol"),
        CS_PQ,
        forged,
        &LineageProvenance {
            identities: &identities,
            roster: Some(&roster),
        },
    )
    .expect_err("a lineage whose GroupInfo is signed by an uncertified leaf must be refused");
    assert!(
        err.to_string().contains("not a certified device of a current member"),
        "the refusal must name the provenance failure, got {err}"
    );

    // Nothing was adopted, so the predecessor survives.
    assert!(
        MlsGroup::load(
            PollisProvider::new(&carol_db).storage(),
            &super::generation::mls_group_id(conv, 1),
        )
        .unwrap()
        .is_none(),
        "a refused lineage must leave no local group behind"
    );
}

/// The second shape of the same attack: the DS does not bother claiming a
/// member's identity and signs with a leaf of its own invention.
///
/// `Unverifiable` alone must NOT refuse; the roster does, since the signer is not a member.
#[test]
fn an_external_join_refuses_a_lineage_signed_from_outside_the_roster() {
    let conv = "01JT1167OFFROSTERSIGNER000";

    let ds_db = make_db();
    {
        let provider = PollisProvider::new(&ds_db);
        create_mls_group_in_suite(&provider, conv, 1, "mallory", "mallory_dev", CS_PQ).unwrap();
    }
    let forged = published_group_info(&ds_db, conv, 1);

    let carol_db = make_db();
    let identities = no_identities();
    let roster: std::collections::HashSet<String> =
        ["alice".to_string(), "carol".to_string()].into_iter().collect();
    let provider = PollisProvider::new(&carol_db);
    let err = build_external_commit(
        &provider,
        conv,
        1,
        "carol",
        &test_device_id("carol"),
        CS_PQ,
        forged,
        &LineageProvenance {
            identities: &identities,
            roster: Some(&roster),
        },
    )
    .expect_err("a lineage signed by a non-member must be refused");
    assert!(
        err.to_string().contains("mallory"),
        "the refusal must name the signer it rejected, got {err}"
    );
}

/// The failure bias, end to end: a LEGITIMATE migration whose signer cannot be
/// verified still goes through.
///
/// Missing certs are a race, not evidence; only a positive `Uncertified` acts.
#[test]
fn a_legitimate_migration_survives_an_unverifiable_signer() {
    let conv = "01JT1167UNVERIFIABLESIGNER";
    let alice_device = test_device_id("alice");

    let alice_db = make_db();
    {
        let provider = PollisProvider::new(&alice_db);
        create_mls_group_in_suite(&provider, conv, 1, "alice", &alice_device, CS_PQ).unwrap();
    }
    let group_info = published_group_info(&alice_db, conv, 1);

    let carol_db = make_db();
    let identities = directory_without_certs("alice", &alice_device);
    let honest_key = leaf_signature_key(&alice_db, &format!("{conv}#g1"), "alice");
    assert!(
        matches!(
            identities.leaf_verdict("alice", &alice_device, &honest_key, signature_scheme(CS_PQ)),
            LeafVerdict::Unverifiable(_)
        ),
        "precondition: this is the unverifiable case, not the uncertified one"
    );
    let roster: std::collections::HashSet<String> =
        ["alice".to_string(), "carol".to_string()].into_iter().collect();
    let provider = PollisProvider::new(&carol_db);
    build_external_commit(
        &provider,
        conv,
        1,
        "carol",
        &test_device_id("carol"),
        CS_PQ,
        group_info,
        &LineageProvenance {
            identities: &identities,
            roster: Some(&roster),
        },
    )
    .expect("an unverifiable — as opposed to uncertified — signer must not block a migration");
}

/// …and the same join still works when NEITHER read landed: no roster, no
/// cross-signing material. A device whose DS reads failed must recover, not
/// wedge.
#[test]
fn a_migration_survives_a_device_that_could_read_neither_roster_nor_certs() {
    let conv = "01JT1167NOTHINGREADABLE000";
    let alice_db = make_db();
    {
        let provider = PollisProvider::new(&alice_db);
        create_mls_group_in_suite(&provider, conv, 1, "alice", &test_device_id("alice"), CS_PQ)
            .unwrap();
    }
    let group_info = published_group_info(&alice_db, conv, 1);

    let carol_db = make_db();
    let identities = no_identities();
    let provider = PollisProvider::new(&carol_db);
    build_external_commit(
        &provider,
        conv,
        1,
        "carol",
        &test_device_id("carol"),
        CS_PQ,
        group_info,
        &LineageProvenance {
            identities: &identities,
            roster: None,
        },
    )
    .expect("a device that could read nothing must still recover");
}

use super::welcomes::join_from_welcome;

/// Extract the inner `Welcome` from serialised Welcome bytes.
fn welcome_from_bytes(welcome_bytes: &[u8]) -> Welcome {
    let mut reader: &[u8] = welcome_bytes;
    match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected Welcome"),
    }
}

/// C1: a Welcome at or below our current epoch must never replace the group we
/// already hold.
///
/// The GroupId inside the blob names which of our groups is deleted, so an attacker could evict a working group.
#[test]
fn a_foreign_welcome_never_replaces_a_group_we_have_carried_past_it() {
    let conv = "01JT1161WELCOMECLOBBER0000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);

    let carol_kp = gen_key_package(&carol_db, "carol");
    let (commit2, _welcome2) = add_member_to_group(&alice_db, conv, &carol_kp);
    apply_commit(&bob_db, conv, &commit2);
    assert_eq!(member_epoch(&bob_db, conv), 2, "precondition: bob is at epoch 2");
    let authenticator_before = member_auth(&bob_db, conv);

    // Mallory welcomes bob into her own group under the SAME GroupId.
    let mallory_db = make_db();
    create_group(&mallory_db, conv, "mallory");
    let bob_kp2 = gen_key_package(&bob_db, "bob");
    let (_evil_commit, evil_welcome) = add_member_to_group(&mallory_db, conv, &bob_kp2);

    let provider = PollisProvider::new(&bob_db);
    let err = join_from_welcome(&provider, welcome_from_bytes(&evil_welcome), None)
        .expect_err("a Welcome at or below our epoch must be refused");
    assert!(
        err.to_string().contains("refusing to replace"),
        "the refusal must say what it protected, got {err}"
    );

    assert_eq!(
        member_epoch(&bob_db, conv),
        2,
        "bob's working group must survive the foreign Welcome"
    );
    assert_eq!(
        member_auth(&bob_db, conv),
        authenticator_before,
        "…and survive it unchanged, not be rebuilt from the blob"
    );
    let members: Vec<String> = load_local_group(&bob_db, conv)
        .unwrap()
        .members()
        .map(|m| parse_credential_user_id(&m.credential))
        .collect();
    assert!(members.contains(&"alice".to_string()), "still the real group — {members:?}");
    assert!(!members.contains(&"mallory".to_string()), "never mallory's group — {members:?}");
}

/// C1: once the row's `conversation_id` is available, a Welcome whose embedded
/// GroupId disagrees with it is refused before anything is touched.
#[test]
fn a_welcome_whose_group_id_disagrees_with_its_row_is_refused() {
    let conv = "01JT1161WELCOMEGROUPIDROW0";
    let (alice_db, bob_db) = (make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);

    let provider = PollisProvider::new(&bob_db);
    let err = join_from_welcome(
        &provider,
        welcome_from_bytes(&welcome1),
        Some("01JT1161WELCOMEOTHERCONV00"),
    )
    .expect_err("a Welcome filed under another conversation must be refused");
    assert!(
        err.to_string().contains("carries a group id for"),
        "the refusal must name the disagreement, got {err}"
    );
    assert!(
        load_local_group(&bob_db, conv).is_none(),
        "a refused Welcome must not have created a group"
    );
}

/// …and passes a Welcome that agrees with its row.
#[test]
fn a_welcome_matching_its_row_still_joins() {
    let conv = "01JT1161WELCOMEROWMATCHES0";
    let (alice_db, bob_db) = (make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);

    let provider = PollisProvider::new(&bob_db);
    let raw = join_from_welcome(&provider, welcome_from_bytes(&welcome1), Some(conv))
        .expect("the honest Welcome must join");
    assert_eq!(raw, conv);
    assert_eq!(member_epoch(&bob_db, conv), 1);
}

/// A ciphertext opened twice reports `SecretReuseError` (the key is already consumed).
#[test]
fn a_replayed_ciphertext_fails_as_secret_reuse() {
    let conv_id = "01JT1520REPLAYEDCIPHERTEXT";
    let (alice_db, bob_db) = alice_and_bob(conv_id);
    let ct = try_mls_encrypt(&alice_db, conv_id, b"once").unwrap();

    let mut decryptor = MlsDecryptor::open(&bob_db, conv_id).expect("group is stored");
    decryptor.decrypt(&ct).expect("the first open succeeds");
    let why = decryptor.decrypt(&ct).expect_err("the same ciphertext must not open twice");
    assert_eq!(why.kind, DecryptFailureKind::SecretReuse, "got {why}");
    assert_eq!(why.sealed_at, why.group_at, "sealed at the epoch the group is at");
    assert!(why.to_string().starts_with("secret reuse ("), "got {why}");
    drop(decryptor);

    // Same reason after a fresh load, so the consumed key was persisted.
    let again = try_mls_decrypt_why(&bob_db, conv_id, &ct).expect_err("still consumed");
    assert_eq!(again.kind, DecryptFailureKind::SecretReuse);
}

/// An envelope from another generation of the same conversation's group is a
/// foreign lineage; one from another group entirely is a group-id mismatch.
/// Both are refused before `process_message`, and told apart.
#[test]
fn a_foreign_lineage_and_a_foreign_group_are_named() {
    let conv_id = "01JT1520FOREIGNLINEAGE0000";
    let alice_db = make_db();
    create_group(&alice_db, conv_id, "alice");
    let ct_gen0 = try_mls_encrypt(&alice_db, conv_id, b"old lineage").unwrap();

    let bob_db = make_db();
    {
        let provider = PollisProvider::new(&bob_db);
        create_mls_group_in_suite(&provider, conv_id, 1, "bob", &test_device_id("bob"), CS_PQ)
            .unwrap();
    }
    // `make_db` holds only `mls_kv`; the generation pointer needs its table.
    bob_db
        .execute_batch(
            "CREATE TABLE mls_generation ( \
                 conversation_id TEXT PRIMARY KEY, \
                 generation      INTEGER NOT NULL, \
                 updated_at      TEXT NOT NULL DEFAULT (datetime('now')) \
             );",
        )
        .unwrap();
    super::generation::set_local_generation(&bob_db, conv_id, 1);
    let why = try_mls_decrypt_why(&bob_db, conv_id, &ct_gen0).expect_err("other lineage");
    assert_eq!(why.kind, DecryptFailureKind::ForeignLineage, "got {why}");
    assert_eq!(why.sealed_at.map(|(g, _)| g), Some(0));
    assert_eq!(why.group_at.map(|(g, _)| g), Some(1));

    // A message for a different conversation's group.
    let other = "01JT1520SOMEOTHERGROUP0000";
    let carol_db = make_db();
    create_group(&carol_db, other, "carol");
    let ct_other = try_mls_encrypt(&carol_db, other, b"elsewhere").unwrap();
    let why = try_mls_decrypt_why(&alice_db, conv_id, &ct_other).expect_err("other group");
    assert_eq!(why.kind, DecryptFailureKind::WrongGroup, "got {why}");
}

/// Non-MLS bytes fail at parse, a tampered body at AEAD; no group is its own reason.
#[test]
fn a_garbled_body_fails_at_parse_and_a_tampered_one_at_aead() {
    let conv_id = "01JT1520GARBLEDBODY0000000";
    let (alice_db, bob_db) = alice_and_bob(conv_id);

    let why = try_mls_decrypt_why(&bob_db, conv_id, b"\xffnot an mls message")
        .expect_err("garbage must not open");
    assert_eq!(why.kind, DecryptFailureKind::Malformed, "got {why}");
    assert_eq!(why.sealed_at, None, "nothing parsed, so no position");

    let mut ct = try_mls_encrypt(&alice_db, conv_id, b"tamper me").unwrap();
    let last = ct.len() - 1;
    ct[last] ^= 0x01;
    let why = try_mls_decrypt_why(&bob_db, conv_id, &ct).expect_err("tampered body");
    assert_eq!(why.kind, DecryptFailureKind::AeadFailure, "got {why}");

    let why = try_mls_decrypt_why(&make_db(), conv_id, &ct).expect_err("no group");
    assert_eq!(why.kind, DecryptFailureKind::NoLocalGroup);
    assert_eq!(why.to_string(), "no local group");
}

/// Bob joins alice's group with openmls' DEFAULT (pre-#1526) window of 5.
fn alice_and_bob_with_default_window(conv_id: &str) -> (rusqlite::Connection, rusqlite::Connection) {
    let alice_db = make_db();
    let bob_db = make_db();
    create_group(&alice_db, conv_id, "alice");
    let bob_kp_bytes = gen_key_package(&bob_db, "bob");
    let welcome_bytes: Vec<u8> = {
        let alice_provider = PollisProvider::new(&alice_db);
        let (mut alice_group, alice_signer) =
            load_group_with_signer(&alice_provider, conv_id, 0).unwrap();
        let mut kp_reader: &[u8] = &bob_kp_bytes;
        let kp = KeyPackageIn::tls_deserialize(&mut kp_reader)
            .unwrap()
            .validate(alice_provider.crypto(), ProtocolVersion::Mls10)
            .unwrap();
        let (_, welcome_msg, _) =
            alice_group.add_members(&alice_provider, &alice_signer, &[kp]).unwrap();
        alice_group.merge_pending_commit(&alice_provider).unwrap();
        welcome_msg.tls_serialize_detached().unwrap()
    };
    let bob_provider = PollisProvider::new(&bob_db);
    let mut reader: &[u8] = &welcome_bytes;
    let welcome = match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected Welcome"),
    };
    StagedWelcome::new_from_welcome(&bob_provider, &MlsGroupJoinConfig::default(), welcome, None)
        .unwrap()
        .into_group(&bob_provider)
        .unwrap();
    (alice_db, bob_db)
}

/// Bob's stored window, read without the Pollis loader's upgrade.
fn stored_window(db: &rusqlite::Connection, conv_id: &str) -> u32 {
    let provider = PollisProvider::new(db);
    MlsGroup::load(provider.storage(), &GroupId::from_slice(conv_id.as_bytes()))
        .unwrap()
        .unwrap()
        .configuration()
        .sender_ratchet_configuration()
        .out_of_order_tolerance()
}

/// Control: under the default window, a message 50 generations behind is lost.
#[test]
fn the_default_window_loses_a_message_fifty_generations_behind() {
    let conv_id = "01JTEST0000000000000WIN05";
    let (alice_db, bob_db) = alice_and_bob_with_default_window(conv_id);
    let cts: Vec<Vec<u8>> = (0..51)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("m{i}").as_bytes()).unwrap())
        .collect();

    let provider = PollisProvider::new(&bob_db);
    let mut group =
        MlsGroup::load(provider.storage(), &GroupId::from_slice(conv_id.as_bytes())).unwrap().unwrap();
    let open = |group: &mut MlsGroup, ct: &[u8]| {
        let mut r: &[u8] = ct;
        let msg = MlsMessageIn::tls_deserialize(&mut r).unwrap().try_into_protocol_message().unwrap();
        group.process_message(&provider, msg)
    };
    open(&mut group, &cts[50]).expect("the newest opens");
    let err = open(&mut group, &cts[0]).expect_err("50 behind under a window of 5");
    assert_eq!(
        super::group_state::classify_decrypt_error(&err),
        DecryptFailureKind::TooDistantInThePast
    );
}

/// Through the Pollis decrypt path the stored window is widened, so all open.
#[test]
fn a_message_fifty_generations_behind_still_opens() {
    let conv_id = "01JTEST0000000000000WIN99";
    let (alice_db, bob_db) = alice_and_bob_with_default_window(conv_id);
    let cts: Vec<Vec<u8>> = (0..51)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("m{i}").as_bytes()).unwrap())
        .collect();

    let (newest, _) = try_mls_decrypt_why(&bob_db, conv_id, &cts[50]).expect("newest");
    assert_eq!(newest, b"m50");
    for (i, ct) in cts.iter().enumerate().take(50) {
        let (pt, sender) = try_mls_decrypt_why(&bob_db, conv_id, ct)
            .unwrap_or_else(|why| panic!("m{i}, {} behind, did not open: {why}", 50 - i));
        assert_eq!(pt, format!("m{i}").as_bytes());
        assert_eq!(sender, "alice");
    }
    // Still one use per key: a replay is refused.
    let why = try_mls_decrypt_why(&bob_db, conv_id, &cts[10]).expect_err("reused");
    assert_eq!(why.kind, DecryptFailureKind::SecretReuse);
}

/// A default-window group is upgraded and written on first Pollis load; the rest of the config survives.
#[test]
fn a_group_stored_with_the_default_window_is_upgraded_on_load() {
    let conv_id = "01JTEST0000000000000WINUP";
    let (_alice_db, bob_db) = alice_and_bob_with_default_window(conv_id);
    assert_eq!(stored_window(&bob_db, conv_id), 5, "joined with openmls' default");

    let loaded = super::provider::load_stored_group(&bob_db, conv_id).expect("group");
    assert_eq!(
        loaded.configuration().sender_ratchet_configuration(),
        &super::provider::sender_ratchet_configuration()
    );
    assert_eq!(stored_window(&bob_db, conv_id), super::provider::OUT_OF_ORDER_TOLERANCE);

    let provider = PollisProvider::new(&bob_db);
    let raw =
        MlsGroup::load(provider.storage(), &GroupId::from_slice(conv_id.as_bytes())).unwrap().unwrap();
    let before = MlsGroupJoinConfig::default();
    assert_eq!(raw.configuration().wire_format_policy(), before.wire_format_policy());
    assert_eq!(raw.configuration().padding_size(), before.padding_size());
    assert_eq!(
        raw.configuration().sender_ratchet_configuration().maximum_forward_distance(),
        super::provider::MAXIMUM_FORWARD_DISTANCE
    );

}

/// New groups store the wide window.
#[test]
fn a_created_group_starts_with_the_wide_window() {
    let conv_id = "01JTEST0000000000000WINNEW";
    let db = make_db();
    create_mls_group_in_suite(&PollisProvider::new(&db), conv_id, 0, "carol", "carol_dev", CS_PQ)
        .unwrap();
    let group_id = super::generation::mls_group_id(conv_id, 0);
    let provider = PollisProvider::new(&db);
    let raw = MlsGroup::load(provider.storage(), &group_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        raw.configuration().sender_ratchet_configuration().out_of_order_tolerance(),
        super::provider::OUT_OF_ORDER_TOLERANCE
    );
}

/// Add bob to `conversation_id` (a fresh group of alice's) with `kp_bytes`, and
/// return the Welcome as bob receives it.
fn add_with_key_package<C>(
    alice: &MlsProvider<'_, C>,
    conversation_id: &str,
    kp_bytes: &[u8],
) -> Welcome
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(alice, conversation_id, 0, "alice", "alice_dev", CS_PQ).unwrap();
    let (mut group, sig) = load_group_with_signer(alice, conversation_id, 0).unwrap();
    let mut reader: &[u8] = kp_bytes;
    let kp = KeyPackageIn::tls_deserialize(&mut reader)
        .unwrap()
        .validate(alice.crypto(), ProtocolVersion::Mls10)
        .unwrap();
    let (_commit, welcome, _) = group.add_members(alice, &sig, &[kp]).unwrap();
    group.merge_pending_commit(alice).unwrap();
    let bytes = welcome.tls_serialize_detached().unwrap();
    let mut reader: &[u8] = &bytes;
    match MlsMessageIn::tls_deserialize(&mut reader).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => panic!("expected a Welcome"),
    }
}

/// The DS reuses the last-resort package, so its private key must outlive the first join; a regular package is the control.
#[test]
fn a_last_resort_key_package_admits_the_device_to_more_than_one_group() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisProvider::new(&alice_db);
    let bob = PollisProvider::new(&bob_db);

    let (_, last_resort) =
        super::key_packages::build_key_package(&bob, "bob", "bob_dev", CS_PQ, true).unwrap();
    for conversation_id in [
        "01JLASTRESORTGROUPA0000000",
        "01JLASTRESORTGROUPB0000000",
        "01JLASTRESORTGROUPC0000000",
    ] {
        let welcome = add_with_key_package(&alice, conversation_id, &last_resort);
        let joined = super::welcomes::join_from_welcome(&bob, welcome, Some(conversation_id))
            .unwrap_or_else(|e| {
                panic!("bob must join {conversation_id} with the last-resort package: {e}")
            });
        assert_eq!(joined, conversation_id);
    }

    let (_, regular) = build_key_package_in_suite(&bob, "bob", "bob_dev", CS_PQ).unwrap();
    let first = add_with_key_package(&alice, "01JREGULARGROUPA0000000000", &regular);
    super::welcomes::join_from_welcome(&bob, first, None).expect("first use of a regular package");
    let second = add_with_key_package(&alice, "01JREGULARGROUPB0000000000", &regular);
    assert!(
        super::welcomes::join_from_welcome(&bob, second, None).is_err(),
        "a regular package is one-shot: its private key is gone after the first join"
    );
}
