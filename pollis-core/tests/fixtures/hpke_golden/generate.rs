//! Generates pollis-core/tests/fixtures/hpke_golden/ with the crypto provider every
//! Pollis build before #1846 links (upstream openmls_rust_crypto at 34222ef: hpke-rs 0.7).
//!
//! Alice and Carol drive the group; Bob is the joiner whose pre-join storage is dumped, so a test
//! can replay his side with a different provider and compare against what this one computed.
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_memory_storage::MemoryStorage;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use serde_json::json;
use tls_codec::{Deserialize as _, Serialize as _};

const CS: Ciphersuite = Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;
const GROUP_ID: &[u8] = b"pollis-hpke-golden";
const EXPORTER_LABEL: &str = "pollis-hpke-golden";
const EXPORTER_CONTEXT: &[u8] = b"exporter-context";
const EXPORTER_LEN: usize = 32;
const PLAINTEXT: &str = "golden fixture: sealed by hpke-rs 0.7";

struct P<'a> {
    crypto: RustCrypto,
    storage: &'a MemoryStorage,
}
impl OpenMlsProvider for P<'_> {
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
    type StorageProvider = MemoryStorage;
    fn storage(&self) -> &MemoryStorage {
        self.storage
    }
    fn crypto(&self) -> &RustCrypto {
        &self.crypto
    }
    fn rand(&self) -> &RustCrypto {
        &self.crypto
    }
}
fn p(storage: &MemoryStorage) -> P<'_> {
    P { crypto: RustCrypto::default(), storage }
}

fn identity(name: &str) -> (SignatureKeyPair, CredentialWithKey) {
    let sig = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
    let cred = CredentialWithKey {
        credential: BasicCredential::new(name.as_bytes().to_vec()).into(),
        signature_key: sig.to_public_vec().into(),
    };
    (sig, cred)
}

fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(100, 1000))
        .build()
}

fn fingerprint(g: &MlsGroup, s: &MemoryStorage) -> serde_json::Value {
    let ex = g.export_secret(p(s).crypto(), EXPORTER_LABEL, EXPORTER_CONTEXT, EXPORTER_LEN).unwrap();
    json!({
        "epoch": g.epoch().as_u64(),
        "exporter_secret": hex::encode(ex),
        "epoch_authenticator": hex::encode(g.epoch_authenticator().as_slice()),
    })
}

fn process(g: &mut MlsGroup, s: &MemoryStorage, bytes: &[u8]) -> Option<Vec<u8>> {
    let pm: ProtocolMessage = MlsMessageIn::tls_deserialize(&mut &bytes[..])
        .unwrap()
        .try_into_protocol_message()
        .unwrap();
    match g.process_message(&p(s), pm).unwrap().into_content() {
        ProcessedMessageContent::ApplicationMessage(a) => Some(a.into_bytes()),
        ProcessedMessageContent::StagedCommitMessage(sc) => {
            g.merge_staged_commit(&p(s), *sc).unwrap();
            None
        }
        other => panic!("unexpected {other:?}"),
    }
}

fn main() {
    let out = std::env::args().nth(1).expect("usage: hpke-golden-gen <out-dir>");

    // Alice creates the group.
    let alice_s = MemoryStorage::default();
    let (alice_sig, alice_cred) = identity("alice");
    let create = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .use_ratchet_tree_extension(true)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(100, 1000))
        .build();
    let mut alice = MlsGroup::new_with_group_id(
        &p(&alice_s),
        &alice_sig,
        &create,
        GroupId::from_slice(GROUP_ID),
        alice_cred,
    )
    .unwrap();

    // Bob publishes a KeyPackage. His storage now holds exactly its private half (init key and
    // leaf encryption key); that is the joiner state the fixture carries. His signer is not
    // stored: the replay only receives.
    let bob_s = MemoryStorage::default();
    let (bob_sig, bob_cred) = identity("bob");
    let bob_kp = KeyPackage::builder()
        .build(CS, &p(&bob_s), &bob_sig, bob_cred)
        .unwrap()
        .key_package()
        .clone();
    let mut joiner_storage: Vec<(Vec<u8>, Vec<u8>)> =
        bob_s.values.read().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    joiner_storage.sort();

    // 1. Alice adds Bob: the Welcome is HPKE-sealed to Bob's init key.
    let (_c, welcome, _) = alice.add_members(&p(&alice_s), &alice_sig, &[bob_kp]).unwrap();
    alice.merge_pending_commit(&p(&alice_s)).unwrap();
    let welcome = welcome.tls_serialize_detached().unwrap();
    let mut expected = vec![fingerprint(&alice, &alice_s)];

    // 2. Carol joins by external commit from Alice's GroupInfo: HPKE export against the
    //    DeriveKeyPair'd external_pub, plus an UpdatePath sealed to the members.
    let carol_s = MemoryStorage::default();
    let (carol_sig, carol_cred) = identity("carol");
    let gi = alice
        .export_group_info(p(&alice_s).crypto(), &alice_sig, true)
        .unwrap()
        .tls_serialize_detached()
        .unwrap();
    let vgi = match MlsMessageIn::tls_deserialize(&mut &gi[..]).unwrap().extract() {
        MlsMessageBodyIn::GroupInfo(g) => g,
        _ => unreachable!(),
    };
    let cp = p(&carol_s);
    let (mut carol, bundle) = MlsGroup::external_commit_builder()
        .with_config(join_config())
        .build_group(&cp, vgi, carol_cred)
        .unwrap()
        .load_psks(cp.storage())
        .unwrap()
        .build(cp.rand(), cp.crypto(), &carol_sig, |_| true)
        .unwrap()
        .finalize(&cp)
        .unwrap();
    let (c, _, _) = bundle.into_contents();
    let external_commit = c.tls_serialize_detached().unwrap();
    process(&mut alice, &alice_s, &external_commit);
    expected.push(fingerprint(&alice, &alice_s));

    // 3. Alice commits a path: the path secret for Bob is sealed to his leaf key.
    let b = alice.self_update(&p(&alice_s), &alice_sig, LeafNodeParameters::default()).unwrap();
    alice.merge_pending_commit(&p(&alice_s)).unwrap();
    let alice_update = b.commit().tls_serialize_detached().unwrap();
    process(&mut carol, &carol_s, &alice_update);
    expected.push(fingerprint(&alice, &alice_s));

    // 4. Carol commits a path: Bob's copath node is the parent Alice just set, so he opens it
    //    with a key he derived himself (DeriveKeyPair), not one from his KeyPackage.
    let b = carol.self_update(&p(&carol_s), &carol_sig, LeafNodeParameters::default()).unwrap();
    carol.merge_pending_commit(&p(&carol_s)).unwrap();
    let carol_update = b.commit().tls_serialize_detached().unwrap();
    process(&mut alice, &alice_s, &carol_update);
    expected.push(fingerprint(&alice, &alice_s));
    assert_eq!(fingerprint(&carol, &carol_s), expected[3]);

    // 5. Carol sends an application message.
    let app = carol
        .create_message(&p(&carol_s), &carol_sig, PLAINTEXT.as_bytes())
        .unwrap()
        .tls_serialize_detached()
        .unwrap();
    assert_eq!(process(&mut alice, &alice_s, &app).as_deref(), Some(PLAINTEXT.as_bytes()));

    // Self-check: Bob replayed on this same provider from the dumped storage reaches every value.
    let replay_s = MemoryStorage::default();
    replay_s.values.write().unwrap().extend(joiner_storage.iter().cloned());
    let w = match MlsMessageIn::tls_deserialize(&mut &welcome[..]).unwrap().extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => unreachable!(),
    };
    let mut bob = StagedWelcome::new_from_welcome(&p(&replay_s), &join_config(), w, None)
        .unwrap()
        .into_group(&p(&replay_s))
        .unwrap();
    assert_eq!(fingerprint(&bob, &replay_s), expected[0]);
    for (i, c) in [&external_commit, &alice_update, &carol_update].into_iter().enumerate() {
        process(&mut bob, &replay_s, c);
        assert_eq!(fingerprint(&bob, &replay_s), expected[i + 1]);
    }
    assert_eq!(process(&mut bob, &replay_s, &app).as_deref(), Some(PLAINTEXT.as_bytes()));

    // Messages as raw TLS bytes; the joiner's storage rows as (key hex, value file). The values
    // are already openmls's serde_json, so they are written verbatim.
    let dir = std::path::Path::new(&out);
    std::fs::create_dir_all(dir).unwrap();
    let w = |name: &str, bytes: &[u8]| std::fs::write(dir.join(name), bytes).unwrap();
    w("welcome.bin", &welcome);
    w("commit_1_external.bin", &external_commit);
    w("commit_2_alice_update.bin", &alice_update);
    w("commit_3_carol_update.bin", &carol_update);
    w("application.bin", &app);
    let mut rows = Vec::new();
    for (i, (k, v)) in joiner_storage.iter().enumerate() {
        let name = format!("joiner_storage_{i}.json");
        w(&name, v);
        rows.push(json!({ "key": hex::encode(k), "value_file": name }));
    }
    let fixture = json!({
        "generator": "openmls_rust_crypto 0.5.1 (hpke-rs 0.7) at openmls 34222ef632c87df58baaa7614c8a72ac235c5f07",
        "ciphersuite": u16::from(CS),
        "group_id": hex::encode(GROUP_ID),
        "exporter": { "label": EXPORTER_LABEL, "context": hex::encode(EXPORTER_CONTEXT), "length": EXPORTER_LEN },
        "joiner_storage": rows,
        "plaintext": PLAINTEXT,
        "expected": expected,
    });
    w("golden.json", (serde_json::to_string_pretty(&fixture).unwrap() + "\n").as_bytes());
    println!("wrote {out}");
}
