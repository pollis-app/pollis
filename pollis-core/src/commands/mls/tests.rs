// ── Tests ─────────────────────────────────────────────────────────────────────

use super::*;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::crypto::OpenMlsCrypto;
use openmls_traits::OpenMlsProvider;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

// Re-export the private items from submodules into this `tests` module
// so the test bodies (which were originally `use super::*` from the
// single-file module) can keep referencing them by short name.
use super::group_state::{create_mls_group_in_suite, load_group_with_signer};
use super::key_packages::build_key_package_in_suite;
use super::provider::{signature_scheme, MlsProvider, PollisProvider, CS_PQ};
use super::self_update::stage_self_update;

/// A suite that is deliberately **not** [`CS_PQ`], used only by the tests.
///
/// #669 retired the classic suite from production, so there is no second suite
/// to ship — but "a stored group whose suite is not the current one" is a state
/// the code must still handle, because `CS_PQ`'s code point is provisional and
/// `draft-ietf-mls-pq-ciphersuites` has renumbered once already. `migrate`, the
/// suite-mismatch refusal in `stage_reconcile_commit`, `stored_group_ciphersuite`
/// and `welcome_ciphersuite` all exist for that day; without a second suite in
/// the test tree they would be untestable, and untestable code rots.
///
/// `0x0001` is the natural choice: it is the suite Pollis actually shipped
/// before #454, so a group on it is a state that genuinely existed, and it is
/// unlike `CS_PQ` in every dimension (KEM, AEAD, KDF, signature scheme) — which
/// makes an accidental "these two happen to agree" pass impossible.
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

    // Alice creates the group.
    create_group(&alice_db, conv_id, "alice");

    // Bob generates a key package.
    let bob_kp_bytes = gen_key_package(&bob_db, "bob");

    // Alice adds Bob: add_members → commit + welcome.
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
            load_group_with_signer(&alice_provider, conv_id, 0).unwrap();
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
        let sk = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
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
        let sk = SignatureKeyPair::new(CS_PQ.signature_algorithm()).unwrap();
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

    // Apply the fix: combined remove+add commit via commit_builder.
    let welcome2_bytes: Vec<u8> = {
        let provider = PollisProvider::new(&alice_db);
        let (mut group, signer) = load_group_with_signer(&provider, conv_id, 0).unwrap();

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
///     the commit, and the removed member cannot decrypt new messages.
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

// ── Post-quantum suite (#454, #668) ──────────────────────────────────────────
//
// #454 shipped two crypto backends: its hybrid suite was X-Wing / `0x004D`,
// which only `openmls_libcrux_crypto` implemented, and the classic AES-GCM
// suite had to stay on RustCrypto because libcrux's AES-GCM tag check is not
// constant-time (RUSTSEC-2026-0211). Three tests pinned that split — a
// cross-provider interop flow proving a staged rollout could not brick a group,
// a pin on libcrux's `supports()` / `supported_ciphersuites()` self-
// contradiction, and the routing invariant itself.
//
// #668 deleted all three along with the second backend. The suite it moves to
// (`0x0052`) keeps X-Wing as its KEM but signs ML-DSA-44, which RustCrypto
// implements and libcrux does not, so there is no second provider to interop
// with, no `supports()` trap to trip, and nothing to route away from. What
// survives is the part that was never about libcrux: that the PQ suite really
// is post-quantum, in the key exchange AND now in the signature, through the
// exact provider Pollis ships.

/// The PQ suite is reachable through the provider Pollis ships, and is
/// genuinely post-quantum in BOTH halves.
///
/// Asserts the suite is advertised, runs a full two-party round-trip on it, and
/// then makes two structural checks that no amount of "the suite constant says
/// so" can fake:
///
///   - the joiner's `hpke_init_key` is 1216 bytes — ML-KEM-768 public key
///     (1184) + X25519 (32). A classical X25519-only init key is 32 bytes, so
///     this is the proof the KEM did not silently fall back.
///   - the leaf signature key is 1312 bytes — an ML-DSA-44 public key. Ed25519
///     is 32, so this is the #668 half: authentication, not just
///     confidentiality, is post-quantum.
#[test]
fn pq_suite_reachable_and_genuinely_post_quantum() {
    let alice_db = make_db();
    let bob_db = make_db();

    // The suite must be advertised by the PQ provider.
    {
        let provider = PollisProvider::new(&alice_db);
        assert!(
            provider.crypto().supported_ciphersuites().contains(&CS_PQ),
            "the shipped provider must advertise the PQ suite"
        );
    }

    // Bob builds a KeyPackage on the CS_PQ suite.
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

    // Structural hybrid proof: ML-KEM-768 pubkey (1184) + X25519 (32) = 1216.
    // A classical X25519-only init key would be 32 bytes; anything but 1216
    // means the key exchange is NOT the hybrid we think it is.
    assert_eq!(
        bob_hpke_init_len, 1216,
        "hybrid hpke_init_key must be ML-KEM-768 (1184) + X25519 (32) = 1216 bytes"
    );

    // Structural PQ-signature proof (#668): an ML-DSA-44 public key is 1312
    // bytes. Ed25519 — what this suite signed with under #454 — is 32.
    assert_eq!(
        bob_sig_pub_len, 1312,
        "PQ leaf signature key must be an ML-DSA-44 public key (1312 bytes)"
    );

    // Alice creates the group on the hybrid suite and adds Bob.
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

    // Bob joins via the hybrid Welcome, then an application message must
    // decrypt end-to-end on the hybrid suite.
    //
    // Driven through openmls directly rather than through `try_mls_encrypt` /
    // `try_mls_decrypt`: those read the group from `mls_kv` under a Pollis group
    // id, and this test builds its group by hand to keep the assertions above
    // structural. `suite_seam_round_trip` covers the production seams.
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
/// `validate_key_package` used to take a concrete `&RustCrypto`. That backend
/// `unimplemented!()`-PANICS on the X-Wing suite, so the moment #454 P2 mints
/// real post-quantum KeyPackages, validating one would have aborted the process
/// rather than returning an error. The signature is now generic over
/// `OpenMlsCrypto`, so a KeyPackage in a suite the passed backend does not
/// implement is a runtime `Err` and not an abort.
///
/// Asserts the returned `KeyPackageRef` equals the one openmls computes, so this
/// proves validation actually happened rather than merely not crashing, and pins
/// the credential-mismatch path as a normal `Err` on either suite.
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

    // The call that would have panicked under RustCrypto on the X-Wing suite.
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
/// #454 needed this as a *routing* invariant: `deny.toml` carried an ignore for
/// RUSTSEC-2026-0211 (libcrux's AES-GCM tag check is not constant-time) whose
/// stated reason was that Pollis never routed AES-GCM through libcrux, and a
/// well-meaning "simplify the two providers into one" would have turned that
/// reason into a lie.
///
/// #668 removed `openmls_libcrux_crypto` from the tree and #669 removed the
/// second suite, so the advisory is gone from `deny.toml` rather than argued
/// away. What is worth pinning now is the weaker but still load-bearing fact
/// underneath: the single provider every path constructs is RustCrypto-backed,
/// so reintroducing a backend is a decision that has to face this test rather
/// than a silent type change.
///
/// Type-level, so it cannot be satisfied by accident at runtime.
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

    // The one backend must advertise the one suite. Asserted rather than
    // assumed because `CS_PQ` is a provisional code point: bumping it to a
    // value the pinned openmls does not implement would otherwise surface as
    // every group operation failing at runtime instead of here.
    let advertised = provider.crypto().supported_ciphersuites();
    assert!(
        advertised.contains(&CS_PQ),
        "RustCrypto must advertise the Pollis suite; got {advertised:?}"
    );

    // The suite Pollis ships is ChaCha20-Poly1305, not AES-GCM. Pinned because
    // it is the reason the AES-GCM advisory that shaped #454's provider routing
    // (RUSTSEC-2026-0211) no longer touches any Pollis traffic: a change back to
    // an AES-GCM suite has to re-open that question, and should fail here first.
    assert_eq!(
        CS_PQ.aead_algorithm(),
        AeadType::ChaCha20Poly1305,
        "the shipped suite must stay ChaCha20-Poly1305"
    );
}

// ── The ciphersuite is a real parameter, not a constant (#454 P1b) ──────────

/// The signature scheme is derived from the suite, not a constant.
///
/// It genuinely was a single constant under #454, when both suites signed
/// Ed25519 and a device presented one signing key everywhere; #668 made it a
/// real function. #669 leaves only one suite in production, which makes this
/// look collapsible again — it is not. `signature_scheme` is read off a *stored
/// group* on every decrypt and commit path, and a group persisted before a code
/// point change is still on its old suite until `migrate` moves it. A leaf
/// signed with the wrong stored key is a device that can no longer open its own
/// groups, so both answers are pinned here.
#[test]
fn signature_scheme_per_suite() {
    assert_eq!(signature_scheme(CS_LEGACY), SignatureScheme::ED25519);
    assert_eq!(signature_scheme(CS_PQ), SignatureScheme::MLDSA44);
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

/// A suite that is not `CS_PQ` still round-trips through the production seams.
///
/// This is what proves the parametrisation is real rather than decorative now
/// that production only ever passes one value. `CS_PQ`'s code point is
/// provisional, so the day it renumbers, every one of these seams —
/// `create_mls_group_in_suite`, `build_key_package_in_suite`,
/// `load_group_with_signer`, `validate_key_package` — has to keep working
/// against groups already stored under the old number. If that ever stops being
/// true, `migrate` has nothing to migrate *from*.
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
///
/// Since #669 this is the suite `init_mls_group` gives every new conversation,
/// with no gate and no alternative.
///
/// The code point is asserted literally because it is the wire contract: the DS
/// stores it as the `mls_key_package.ciphersuite` label (`CIPHERSUITE_PQ`), so a
/// silent change here would strand every published pool. #454 minted it as
/// `0x004D`, #668 moved it to `0x0052`, and the draft may renumber it again —
/// which is a deliberate, `migrate`-shaped change, never a quiet one.
#[test]
fn pq_suite_round_trips_through_the_production_seams() {
    let alice_db = make_db();
    let bob_db = make_db();
    let alice = PollisProvider::new(&alice_db);
    let bob = PollisProvider::new(&bob_db);

    suite_seam_round_trip(&alice, &bob, CS_PQ, "01JTESTSEAMPQ00000000000000");
    assert_eq!(u16::from(CS_PQ), 0x0052);
}

/// The suite argument selects a genuinely different key exchange, which is the
/// entire point of #454. Asserted structurally on the KeyPackage the production
/// builder emits: X25519 alone is a 32-byte HPKE init key, X-Wing is
/// ML-KEM-768's 1184 plus X25519's 32.
///
/// Reads as a tautology and is not: it is the only thing standing between
/// "Pollis is post-quantum" and a `CS_PQ` that silently resolves to a classical
/// KEM. The number 1216 is the claim.
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

    // Each package is validated by the backend that serves its suite.
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

// ── #454 P3: the suite-dispatch seam ─────────────────────────────────────────

/// A group's ciphersuite is readable from STORAGE ALONE, with no crypto backend
/// picked yet — `MlsGroup::load` is generic over `StorageProvider`, not
/// `OpenMlsProvider`, which is what makes `stored_group_ciphersuite` possible.
///
/// #454 needed this to break a circularity in suite→backend dispatch: you would
/// otherwise need a provider to learn which provider to use. #668 collapsed the
/// backends to one and #669 the suites, so nothing dispatches on the answer any
/// more — but `migrate` still has to ask "is this group on the current suite?"
/// before it can answer "must it move?", and it asks by reading storage.
#[test]
fn a_groups_suite_is_readable_from_storage_without_a_crypto_backend() {
    for (suite, conv) in [
        (CS_LEGACY, "01JTESTSUITEREADLEGACY0000"),
        (CS_PQ, "01JTESTSUITEREADPQ00000000"),
    ] {
        let db = make_db();
        create_mls_group_in_suite(&PollisProvider::new(&db), conv, 0, "alice", "alice_dev", suite)
            .unwrap();

        // Read the suite back with no crypto backend at all.
        assert_eq!(
            super::provider::stored_group_ciphersuite(&db, conv),
            Some(suite),
            "the stored group's suite must be readable through MlsStore alone"
        );
    }
}

/// A joiner has no local group, so `apply_welcome` reads the suite off the
/// Welcome itself. `Welcome::ciphersuite()` is `pub(crate)` in openmls, so
/// `welcome_ciphersuite` parses the wire format per RFC 9420 §12.4.3.1
/// (cipher_suite is the first field, 2 bytes big-endian) — this pins that
/// parse against real Welcomes on both suites.
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
/// Pollis blocks this at three depths, and the deepest one is the protocol
/// itself. `reconcile` claims KeyPackages scoped to the group's own suite (so a
/// device with nothing in that suite has nothing to claim and is reported in
/// `ReconcileOutcome::skipped_no_suite_kp`), `stage_reconcile_commit` re-checks
/// the claimed package's suite against the group's because the DS is untrusted,
/// and — asserted here — openmls itself refuses the add.
///
/// #669 retired the classic suite, which removes the *fleet* that could have
/// offered such a package but not the *attack*: the DS is untrusted, and a
/// hostile one handing back a package in a weaker suite is exactly the
/// downgrade this forbids. `CS_LEGACY` stands in for whatever that package
/// claims to be.
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

    // The package the untrusted DS hands back is in a different, weaker suite.
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
    eprintln!("[test] PQ group rejected the off-suite KeyPackage: {err}");

    // The rejected add must leave nothing staged — a failed downgrade attempt
    // cannot wedge the group.
    alice_group.clear_pending_commit(alice.storage()).unwrap();
    assert_eq!(alice_group.ciphersuite(), CS_PQ, "the group stays post-quantum");
}

// ── #666: self-update, unmerged leaves, and commit growth ────────────────────

/// The claim #666 rests on, measured rather than asserted: **commit size grows
/// with the number of members only while their leaves are unmerged.**
///
/// A member added by someone else does not know the secrets of the nodes above
/// it, so every one of those nodes stays blank and each such leaf lands in a
/// copath resolution on its own — one HPKE ciphertext per member in every
/// commit anyone issues. That is a linear cost, and it never decays on its own:
/// only a commit *from that member* populates its direct path and collapses the
/// resolution back to a single node.
///
/// So the test doubles the group and compares the marginal cost of that
/// doubling. Unmerged, doubling the group roughly doubles the commit; merged,
/// it adds a couple of ciphertexts — TreeKEM's logarithm, which Pollis was not
/// getting before the post-join self-update. On the hybrid suite that is 8→16
/// members costing +10.6 KB unmerged versus +2.4 KB merged (13.4→24.0 KB vs
/// 8.7→11.1 KB).
///
/// Deliberately a ratio and not a byte ceiling: the *shape* is the invariant,
/// and it must hold whatever the suite, even though a post-quantum
/// encapsulation is ~35× larger than X25519's. Running it on `CS_LEGACY` as
/// well is what makes that a property of TreeKEM rather than a coincidence of
/// one set of key sizes. Absolute ceilings live in
/// [`pq_payloads_stay_under_their_ceilings`].
#[test]
fn self_update_turns_linear_commit_growth_into_logarithmic() {
    for suite in [CS_LEGACY, CS_PQ] {
        let (small_unmerged, small_merged) = commit_growth_probe(suite, 8, "01JT666GROWTHSMALL00000000");
        let (large_unmerged, large_merged) = commit_growth_probe(suite, 16, "01JT666GROWTHLARGE00000000");

        let unmerged_delta = large_unmerged as i64 - small_unmerged as i64;
        let merged_delta = large_merged as i64 - small_merged as i64;
        eprintln!(
            "[test] {suite:?}: 8→16 members costs +{unmerged_delta} B unmerged, +{merged_delta} B merged \
             (8: {small_unmerged}→{small_merged} B, 16: {large_unmerged}→{large_merged} B)"
        );

        // Merging can only ever help: a member that rotated its own leaf is
        // strictly cheaper to address than one sitting in a blank subtree.
        assert!(
            large_merged < large_unmerged,
            "{suite:?}: self-updates must shrink the commit ({large_merged} !< {large_unmerged})"
        );

        // The real invariant: the marginal cost of doubling the group collapses.
        // Unmerged, 8→16 adds eight ciphertexts; merged, it adds two.
        assert!(
            merged_delta * 3 < unmerged_delta,
            "{suite:?}: doubling the group must cost far less once every leaf is merged \
             (+{merged_delta} B merged vs +{unmerged_delta} B unmerged)"
        );
    }
}

/// Size of a commit alice issues into a `members`-strong group, measured twice:
/// once with every other leaf still unmerged (added by alice, never having
/// committed), and once after every member has rotated its own leaf.
///
/// Both measurements are of the *same* commit under the *same* tree size, so
/// the only variable is whether the leaves are merged.
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

    // One batched add, which is how reconcile actually adds people — and the
    // reason every joiner starts life as an unmerged leaf.
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

    // Measurement 1: every other leaf is unmerged.
    let unmerged = probe_self_update_size(alice, conversation_id);

    // Every member rotates its own leaf — the post-join self-update, applied by
    // the whole group exactly as the live path would.
    for i in 1..providers.len() {
        let (_, commit_bytes, _) = stage_self_update(&providers[i], conversation_id, 0)
            .unwrap()
            .expect("a joined member can always self-update");
        for (j, p) in providers.iter().enumerate() {
            if i == j {
                // Loading merges the pending commit — see `load_group_with_signer`.
                load_group_with_signer(p, conversation_id, 0).unwrap();
            } else {
                apply_commit_in_suite(p, conversation_id, &commit_bytes);
            }
        }
    }

    // Measurement 2: same members, same tree size, every leaf merged.
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
    // Deliberately NOT `load_group_with_signer`, which merges any pending commit
    // as part of loading — that would make the measurement advance the epoch.
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

/// A self-update must actually rotate this device's leaf key. If a future
/// openmls decided an empty commit needs no path, the commit would still land
/// and every caller would still think it had healed — silently giving up
/// post-compromise security. `force_self_update(true)` is what prevents that,
/// and this pins it.
#[test]
fn a_self_update_replaces_our_own_leaf_key() {
    let db = make_db();
    let provider = PollisProvider::new(&db);
    let conv = "01JT666LEAFROTATES00000000";
    create_mls_group_in_suite(&provider, conv, 0, "alice", "alice_dev", CS_PQ).unwrap();

    // openmls keeps the raw key bytes crate-private, so compare the key itself.
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

/// Rotation jitter must be deterministic per conversation. A jitter re-rolled
/// on every check would let a device that restarts often drift past its
/// deadline indefinitely — the rotation would be perpetually "not quite due".
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
/// The growth *shape* is pinned by
/// [`self_update_turns_linear_commit_growth_into_logarithmic`]; this pins the
/// constants. PQ payloads are large by construction — an ML-KEM-768
/// encapsulation is 1,088 B against X25519's 32, and since #668 every leaf also
/// carries a 1,312 B ML-DSA-44 public key and every signature is 2,420 B against
/// Ed25519's 64 — and "large" quietly becoming "unusable" is the failure mode
/// this has to defend against. A KEM swap, a codepoint change, or an accidental
/// return to unmerged leaves should surface here as a failing number, not as a
/// support ticket about a group that stopped working at forty members.
///
/// Measured after #668: KeyPackage 8,670 B (classical: 307 B), Welcome 15,241 B,
/// and a commit into a merged 8-member group 14,839 B. Moving the signature to
/// ML-DSA-44 roughly tripled all three — a KeyPackage carries one leaf key and
/// two signatures, so it alone went 2,659 → 8,670 B. Ceilings sit ~1.4× above
/// each: tight enough that a doubling fails, loose enough to survive openmls
/// padding and version churn. A classical KeyPackage is built alongside purely
/// to print the multiplier, so the price of post-quantum stays visible and
/// honest rather than becoming a number nobody remembers the baseline for.
#[test]
fn pq_payloads_stay_under_their_ceilings() {
    // A KeyPackage is published per device and claimed on every add, so it is
    // the most-transferred post-quantum object there is.
    let kp_db = make_db();
    let (_, hybrid_kp) =
        build_key_package_in_suite(&PollisProvider::new(&kp_db), "alice", "alice_dev", CS_PQ)
            .unwrap();
    let legacy_db = make_db();
    let (_, legacy_kp) =
        build_key_package_in_suite(&PollisProvider::new(&legacy_db), "alice", "alice_dev", CS_LEGACY)
            .unwrap();
    eprintln!(
        "[test] KeyPackage: classical {} B, post-quantum {} B",
        legacy_kp.len(),
        hybrid_kp.len()
    );
    assert!(hybrid_kp.len() < 12_288, "hybrid KeyPackage: {} B", hybrid_kp.len());

    // A Welcome carries the group secrets sealed to the joiner's init key —
    // one hybrid encapsulation, plus the ratchet tree extension.
    let (wa, wb) = (make_db(), make_db());
    let welcome = welcome_for_suite(
        &PollisProvider::new(&wa),
        &PollisProvider::new(&wb),
        CS_PQ,
        "01JT454CEILINGWELCOME00000",
    );
    eprintln!("[test] hybrid Welcome (2 members): {} B", welcome.len());
    assert!(welcome.len() < 21_504, "hybrid Welcome: {} B", welcome.len());

    // The commit is the payload that scales, so it is the one with a real
    // ceiling. Measured in the state the fleet actually runs in — every leaf
    // merged, because every member self-updates on join (#666).
    let (unmerged, merged) = commit_growth_probe(CS_PQ, 8, "01JT454CEILINGCOMMIT000000");
    eprintln!("[test] hybrid commit at 8 members: {unmerged} B unmerged, {merged} B merged");
    assert!(merged < 20_480, "hybrid commit at 8 members: {merged} B");
}

// ─── #680: commit-apply recovery classification + no-wedge Stop sites ─────────
//
// These tests are the regression harness for issue #680. Two properties are
// pinned: (1) `apply_one_commit` classifies openmls failures by their real error
// ENUMS (not `Display` strings that silently drift on an openmls bump), and (2)
// none of the former `Stop` sites wedge a group that exists locally but can never
// advance — every one turns into an explicit `Recover(reason)` and drops the
// local group so the caller's external-rejoin backstop rebuilds it.

use super::group_state::{
    apply_one_commit, build_external_commit, export_group_info_blob,
    CommitApply, RecoverReason,
};
use openmls::prelude::group_info::VerifiableGroupInfo;

/// Layer 1 — the pure classifier. One case per openmls error we classify, plus
/// the load-bearing #680 guarantee: an UNRECOGNISED error recovers conservatively
/// rather than wedging. `S = ()` — the classifier is generic over the provider's
/// storage-error type and never inspects it.
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
        // The case the OLD "created by this client" string used to catch. It is
        // now a typed `StageCommitError`, and it means our pending state diverged
        // from what landed — rebuild, do NOT merge our stale pending.
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
        // THE #680 guarantee. A storage error — and, by the same `_` arm, any
        // future openmls variant this code has never seen — must still RECOVER,
        // never wedge. When openmls reshapes its error surface, this is the arm
        // that keeps a group live instead of silently stuck.
        assert_eq!(
            classify(ProcessMessageError::StorageError(())),
            RecoverReason::Unclassified
        );
    }
}

// ── layer-2 helpers: real MLS group manipulation via `apply_one_commit` ───────

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

/// This member's epoch authenticator — openmls's own "everyone has the same group
/// state" fingerprint. Two members at the same epoch with equal authenticators
/// hold identical group state (tree + transcript); it diverges the instant their
/// trees do, which is exactly the convergence property #680 must preserve.
fn member_auth(db: &rusqlite::Connection, conv: &str) -> Vec<u8> {
    load_local_group(db, conv)
        .expect("group must exist")
        .epoch_authenticator()
        .as_slice()
        .to_vec()
}

/// Stage a member-add commit but do NOT merge it — the group is left holding a
/// PENDING commit, exactly the state a committer is in between building its
/// commit and adopting it. Returns `(commit_bytes, welcome_bytes)`.
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

/// Self-update and merge, advancing this member's own epoch by one. Returns the
/// commit bytes other members would apply to follow.
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

/// Build a two-member group (alice creator + bob via Welcome), both at epoch 1,
/// and return a THIRD-party commit alice produced at epoch 1 (adding carol,
/// merged on alice → alice at epoch 2) that bob, still at epoch 1, can apply.
/// The returned `(commit, welcome)` is a genuine, valid commit — the failure
/// tests corrupt or misuse it; the happy path applies it.
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

/// Site 1 (`MlsGroup::load` → `Ok(None)`): a member with NO local group must
/// recover, not wedge. This is the arm at group_state.rs ~:1632.
#[test]
fn stop_site_group_missing_recovers() {
    let conv = "01JT680MISSINGGROUP0000000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    // A brand-new device with no group for this conversation.
    let empty_db = make_db();
    let outcome = apply_one(&empty_db, conv, 1, &commit2);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::GroupMissing)),
        "a missing local group must recover via external-join, not wedge"
    );
}

/// Site 2 (`MlsMessageIn::tls_deserialize` fails): corrupt / truncated commit
/// bytes must recover, not wedge (group_state.rs ~:1644). Also proves the group
/// is DELETED so the caller's rejoin has a clean slate, and that the member can
/// then rejoin and apply the NEXT commit.
#[test]
fn stop_site_corrupt_commit_bytes_recovers_and_can_advance() {
    let conv = "01JT680CORRUPTBYTES000000A";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    // Truncate the commit so it can't even deserialize.
    let truncated = &commit2[..commit2.len() / 2];
    let outcome = apply_one(&bob_db, conv, 1, truncated);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::MalformedCommit)),
        "truncated commit bytes must recover, got {outcome:?}",
    );
    // The wedge is converted to a recovery signal: the stale local group is gone.
    assert!(
        load_local_group(&bob_db, conv).is_none(),
        "recovery must delete the local group so the rejoin path rebuilds it",
    );

    // …and recovery is REAL: bob external-joins alice's published GroupInfo and
    // then applies alice's NEXT commit, decrypting a fresh message. This is the
    // whole point of #680 — a group that could not advance is now advancing.
    assert_rejoin_then_next_commit_works(&alice_db, &bob_db, conv);
}

/// Site 3 (`try_into_protocol_message` fails): bytes that deserialize as an MLS
/// message but are NOT a protocol message (here, a Welcome) must recover, not
/// wedge (group_state.rs ~:1651).
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

/// The DoD's "a commit for an already-merged epoch": bob applies the commit, then
/// is fed the SAME (now stale) commit again. openmls rejects it (`WrongEpoch`),
/// which classifies as a forked/diverged tree and recovers — it must NOT wedge.
#[test]
fn already_merged_epoch_commit_recovers() {
    let conv = "01JT680ALREADYMERGED00000A";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);

    // Bob applies commit2 normally: epoch 1 → 2.
    assert!(matches!(apply_one(&bob_db, conv, 1, &commit2), CommitApply::Applied));
    assert_eq!(member_epoch(&bob_db, conv), 2);

    // Re-feeding the stale epoch-1 commit at epoch 2 must recover, not wedge.
    let outcome = apply_one(&bob_db, conv, 1, &commit2);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::ForkedTree)),
        "a commit for an already-merged epoch must recover, got {outcome:?}",
    );
}

/// The DoD's "a merge failure against a diverged local tree". Two members commit
/// CONCURRENTLY at epoch 1 and each merges its own, so both sit at "epoch 2" over
/// DIFFERENT trees. Alice then commits again (epoch 2 → 3) and bob is fed that
/// epoch-2 commit: it passes the epoch-gap check (bob is at epoch 2) yet cannot
/// apply against bob's diverged tree. On today's code that wedged; it must now
/// recover.
///
/// Note on the merge site (group_state.rs ~:1660): at the pinned openmls rev a
/// diverged tree is rejected during `process_message` (staging), classified as
/// `ForkedTree`, before `merge_staged_commit` is reached — so this scenario and
/// the `merge_staged_commit` arm share one recovery. The merge arm's own contract
/// (return `Recover(MergeFailed)`, delete the group) is exercised by the code and
/// covered structurally; a storage-level merge failure is not reproducible in an
/// in-memory provider without corrupting storage.
#[test]
fn diverged_tree_commit_recovers() {
    let conv = "01JT680DIVERGEDTREE00000AB";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    let (_commit2, _w2) = two_members_plus_valid_commit(conv, &alice_db, &bob_db, &carol_db);
    // Reset to a clean two-member group so the fork is unambiguous: rebuild.
    let conv = "01JT680DIVERGEDTREE00000CD";
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);

    // Concurrent commits at epoch 1: alice and bob each self-update and merge
    // their OWN, diverging onto distinct epoch-2 trees.
    let _alice_fork = self_update_and_merge(&alice_db, conv);
    let _bob_fork = self_update_and_merge(&bob_db, conv);
    assert_eq!(member_epoch(&alice_db, conv), 2);
    assert_eq!(member_epoch(&bob_db, conv), 2);
    assert_ne!(
        member_auth(&alice_db, conv),
        member_auth(&bob_db, conv),
        "the two members must have genuinely diverged trees"
    );

    // Alice commits again (epoch 2 → 3). Feeding bob this epoch-2 commit passes
    // the gap check but cannot apply against bob's diverged tree.
    let alice_next = self_update_and_merge(&alice_db, conv);
    let outcome = apply_one(&bob_db, conv, 2, &alice_next);
    assert!(
        matches!(outcome, CommitApply::Recover(RecoverReason::ForkedTree)),
        "a commit against a diverged local tree must recover, got {outcome:?}",
    );
    assert!(load_local_group(&bob_db, conv).is_none(), "group must be deleted for rejoin");
}

/// The #411 own-commit replay, and the specific misclassification #680 fixes: at
/// the pinned openmls rev, a committer's OWN commit fanned back to it is surfaced
/// as CONTENT (an `Ok`) — `OwnPrivateMessage` for Pollis's PrivateMessage-framed
/// handshake (or `OwnPendingCommit` if it were PublicMessage-framed) — NOT the OLD
/// "created by this client" error string. The pre-#680 code only matched the
/// string, so the own commit fell through UNMERGED — counted as applied, left a
/// dangling pending commit, and the dangling-clear backstop then discarded the
/// landed commit, stranding the committer at a phantom epoch. This asserts the
/// fix: `apply_one_commit` MERGES the pending commit, the committer advances to
/// the real epoch, and it CONVERGES with the members who applied the same commit —
/// same epoch, same epoch-authenticator.
#[test]
fn own_pending_commit_is_adopted_and_converges() {
    let conv = "01JT680OWNPENDINGCOMMIT000";
    let (alice_db, bob_db, carol_db) = (make_db(), make_db(), make_db());
    create_group(&alice_db, conv, "alice");
    let bob_kp = gen_key_package(&bob_db, "bob");
    let (_c1, welcome1) = add_member_to_group(&alice_db, conv, &bob_kp);
    join_via_welcome(&bob_db, &welcome1);
    assert_eq!(member_epoch(&alice_db, conv), 1);

    // Alice STAGES a commit adding carol but does NOT merge it — she holds a
    // pending commit at epoch 1. This is exactly the #411 state after a lost
    // success-response: the commit landed in the log, but alice never adopted it.
    let carol_kp = gen_key_package(&carol_db, "carol");
    let (own_commit, welcome2) = stage_add_without_merge(&alice_db, conv, &carol_kp);
    assert_eq!(member_epoch(&alice_db, conv), 1, "alice still at epoch 1 (unmerged)");
    assert!(
        load_local_group(&alice_db, conv).unwrap().pending_commit().is_some(),
        "alice holds a pending commit",
    );

    // Alice replays her OWN commit from the log. Pre-#680 this fell through
    // unmerged; now it must be adopted (merge the pending commit) and count as
    // applied.
    let outcome = apply_one(&alice_db, conv, 1, &own_commit);
    assert!(
        matches!(outcome, CommitApply::Applied),
        "own commit replay must be adopted (Applied), got {outcome:?}",
    );
    assert_eq!(member_epoch(&alice_db, conv), 2, "alice must advance to the real merged epoch");
    assert!(
        load_local_group(&alice_db, conv).unwrap().pending_commit().is_none(),
        "the pending commit must be merged, not left dangling",
    );

    // Convergence: bob applies the same commit, carol joins via the Welcome, and
    // all three land at the same epoch over the same tree.
    apply_commit(&bob_db, conv, &own_commit);
    join_via_welcome(&carol_db, &welcome2);
    assert_eq!(member_epoch(&alice_db, conv), member_epoch(&bob_db, conv));
    assert_eq!(member_epoch(&alice_db, conv), member_epoch(&carol_db, conv));
    let a = member_auth(&alice_db, conv);
    assert_eq!(a, member_auth(&bob_db, conv), "alice and bob trees must match");
    assert_eq!(a, member_auth(&carol_db, conv), "alice and carol trees must match");

    // And the converged group works: alice encrypts, both others decrypt.
    let ct = try_mls_encrypt(&alice_db, conv, b"post-recovery").expect("encrypt");
    let (bob_pt, _) = try_mls_decrypt(&bob_db, conv, &ct).expect("bob decrypts");
    let (carol_pt, _) = try_mls_decrypt(&carol_db, conv, &ct).expect("carol decrypts");
    assert_eq!(bob_pt, b"post-recovery");
    assert_eq!(carol_pt, b"post-recovery");
}

/// After a Stop-site failure deleted `joiner`'s local group, prove recovery is
/// real: the joiner external-joins `owner`'s published GroupInfo (the exact MLS
/// primitive the production external-rejoin uses) and then applies `owner`'s NEXT
/// commit, decrypting a fresh message. Panics on any failure.
fn assert_rejoin_then_next_commit_works(
    owner_db: &rusqlite::Connection,
    joiner_db: &rusqlite::Connection,
    conv: &str,
) {
    // Owner publishes a GroupInfo at its current epoch.
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

    // Joiner external-joins from the GroupInfo, producing an external commit.
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
        )
        .expect("external join builds");
        commit
    };
    // The joiner is now a fresh member — its rebuilt group exists again.
    assert!(load_local_group(joiner_db, conv).is_some(), "rejoin must rebuild the local group");

    // Owner applies the joiner's external commit to converge onto the shared head.
    apply_commit(owner_db, conv, &ext_commit);

    // Owner drives the NEXT commit; the rejoined member applies it and decrypts a
    // fresh message — the group is advancing again.
    let next = self_update_and_merge(owner_db, conv);
    apply_commit(joiner_db, conv, &next);
    let ct = try_mls_encrypt(owner_db, conv, b"after recovery").expect("owner encrypts");
    let (pt, _) = try_mls_decrypt(joiner_db, conv, &ct).expect("rejoined member decrypts");
    assert_eq!(pt, b"after recovery", "the rejoined group must apply the next commit");
}

// ── #875: the shape of the decrypt hot path ──────────────────────────────────

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
/// This is a GUARD, not a proof of correctness — `MlsDecryptor` is a pure
/// optimisation and the tests that prove it decrypts the right bytes are the
/// roundtrip tests above. What this pins is the SHAPE: before #875 every
/// envelope re-read the entire group out of `mls_kv` (measured: 12 SELECTs + 1
/// UPDATE per message), so a 200-message catch-up ran 2400 statements. The
/// per-message floor is the one UPDATE openmls itself makes to persist the
/// ratcheted secret tree — that write is forward secrecy and must never be
/// optimised away, so it is asserted to still be there.
#[test]
fn a_run_of_envelopes_loads_the_group_once() {
    let conv_id = "01JTEST0000000000000000BAT";
    let (alice_db, bob_db) = alice_and_bob(conv_id);

    const N: usize = 10;
    let cts: Vec<Vec<u8>> = (0..N)
        .map(|i| try_mls_encrypt(&alice_db, conv_id, format!("m{i}").as_bytes()).unwrap())
        .collect();

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

    // One group load, whatever its internal read count, amortised over N
    // messages — so reads must be far below the old 12-per-message. The bound is
    // deliberately loose (openmls may add or drop a stored field); what it
    // forbids is the per-message reload coming back.
    assert!(
        reads < 2 * N as u64,
        "expected roughly one group load for the whole run, got {reads} reads for {N} messages \
         — the per-envelope group reload is back"
    );
    // Exactly one secret-tree persist per message. Fewer would mean a consumed
    // ratchet key survived a crash; more would mean redundant writes.
    assert_eq!(writes, N as u64, "one message_secrets write per decrypt, no more and no fewer");
}

/// The same run driven one envelope at a time through the public
/// [`try_mls_decrypt`] entry point still works — that path opens a decryptor per
/// call, which is what every non-batched caller does.
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
/// The statement counts are exact and machine-independent; the timings are a
/// DEBUG build against in-memory SQLite, so they understate the real gain — a
/// production build reads through SQLCipher on disk, where a `mls_kv` SELECT
/// costs far more relative to the crypto.
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
