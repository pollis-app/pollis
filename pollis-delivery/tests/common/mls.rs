//! Real MLS material for the DS tests (#1851): an account with a published
//! ML-DSA-44 identity key, devices whose certs it signed (through the real
//! `apply_publish_device_cert`), and the KeyPackages, GroupInfos and Welcomes
//! those devices build with openmls, exactly as a client would send them.
//!
//! The DS refuses an MLS object it cannot show to be certified, so a test that
//! stores KeyPackages, Welcomes or GroupInfos needs these rather than
//! placeholder bytes.

use base64::Engine as _;
use libsql::Connection;
use ml_dsa::{Keypair as _, MlDsa44, Signer as _, SigningKey};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;
use rand_core::{OsRng, RngCore as _};

use pollis_delivery::devices::KeyPackageEntry;

/// The only suite Pollis runs (`CS_PQ`, 0x0052).
pub const SUITE: Ciphersuite = Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;

pub fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// An account: a `users` row whose `account_id_pub` this key's public half is.
pub struct MlsAccount {
    pub user_id: String,
    key: SigningKey<MlDsa44>,
}

impl MlsAccount {
    /// Create (or re-key) `user_id`'s `users` row with a fresh account key.
    pub async fn new(conn: &Connection, user_id: &str) -> MlsAccount {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        MlsAccount::from_seed(conn, user_id, seed).await
    }

    /// [`Self::new`] with the account key derived from `seed`.
    pub async fn from_seed(conn: &Connection, user_id: &str, seed: [u8; 32]) -> MlsAccount {
        let key = SigningKey::<MlDsa44>::from_seed(&seed.into());
        conn.execute(
            "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?2, ?1)",
            libsql::params![user_id.to_string(), format!("{user_id}@test.local")],
        )
        .await
        .expect("insert user");
        conn.execute(
            "UPDATE users SET account_id_pub = ?1, identity_version = 1 WHERE id = ?2",
            libsql::params![key.verifying_key().encode().to_vec(), user_id.to_string()],
        )
        .await
        .expect("publish account key");
        MlsAccount {
            user_id: user_id.to_string(),
            key,
        }
    }

    /// Register `device_id` and publish a cert this account signed over its
    /// two device keys.
    pub async fn device(&self, conn: &Connection, device_id: &str) -> MlsDevice {
        let mut seed = [0u8; 32];
        OsRng.fill_bytes(&mut seed);
        self.device_from_seed(conn, device_id, seed).await
    }

    /// [`Self::device`] with the device's ML-DSA-44 key derived from `seed`:
    /// a test whose device already signs DS requests with that key keeps its
    /// request credential (`mls_signature_pub_pq`) unchanged.
    pub async fn device_from_seed(&self, conn: &Connection, device_id: &str, seed: [u8; 32]) -> MlsDevice {
        let ed = SignatureKeyPair::new(SignatureScheme::ED25519).expect("Ed25519 device key");
        self.device_with_keys(conn, device_id, seed, ed).await
    }

    /// [`Self::device_from_seed`] with a given Ed25519 key too.
    pub async fn device_with_keys(
        &self,
        conn: &Connection,
        device_id: &str,
        seed: [u8; 32],
        ed: SignatureKeyPair,
    ) -> MlsDevice {
        conn.execute(
            "INSERT OR IGNORE INTO user_device (device_id, user_id) VALUES (?1, ?2)",
            libsql::params![device_id.to_string(), self.user_id.clone()],
        )
        .await
        .expect("register device");
        let pq_pub = SigningKey::<MlDsa44>::from_seed(&seed.into()).verifying_key().encode().to_vec();
        let signer = SignatureKeyPair::from_raw(SignatureScheme::MLDSA44, seed.to_vec(), pq_pub);
        let issued_at = 1_700_000_000u64;
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            ed.public(),
            signer.public(),
            1,
            issued_at,
        )
        .expect("cert payload");
        let cert = self.key.sign(&payload).encode().to_vec();
        let outcome = pollis_delivery::bootstrap::apply_publish_device_cert(
            conn,
            &self.user_id,
            device_id,
            &cert,
            issued_at,
            1,
            ed.public(),
            signer.public(),
        )
        .await
        .expect("publish device cert");
        assert!(
            matches!(outcome, pollis_delivery::bootstrap::PublishCertOutcome::Applied),
            "fixture cert must verify"
        );
        MlsDevice {
            user_id: self.user_id.clone(),
            device_id: device_id.to_string(),
            provider: OpenMlsRustCrypto::default(),
            signer,
            ed,
        }
    }
}

/// One TLS-serialized KeyPackage and its hex KeyPackageRef.
#[derive(Clone)]
pub struct Kp {
    pub ref_hash: String,
    pub bytes: Vec<u8>,
    pub last_resort: bool,
    pub suite: Ciphersuite,
}

impl Kp {
    /// The wire entry a client would publish this package as.
    pub fn entry(&self) -> KeyPackageEntry {
        KeyPackageEntry {
            ref_hash: self.ref_hash.clone(),
            key_package: b64(&self.bytes),
            ciphersuite: Some(i64::from(u16::from(self.suite))),
            last_resort: self.last_resort,
        }
    }
}

/// The outputs of one add commit.
pub struct AddBundle {
    pub commit: Vec<u8>,
    pub welcome: Vec<u8>,
    pub group_info: Vec<u8>,
}

/// A device: its certified ML-DSA-44 MLS key and its own openmls storage.
pub struct MlsDevice {
    pub user_id: String,
    pub device_id: String,
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    /// The certified Ed25519 key, which signs leaves in the retired classic suite.
    ed: SignatureKeyPair,
}

impl MlsDevice {
    fn credential(&self, signer: &SignatureKeyPair) -> CredentialWithKey {
        CredentialWithKey {
            credential: BasicCredential::new(
                format!("{}:{}", self.user_id, self.device_id).into_bytes(),
            )
            .into(),
            signature_key: signer.public().into(),
        }
    }

    fn build_kp(&self, signer: &SignatureKeyPair, last_resort: bool) -> Kp {
        self.build_kp_in(SUITE, signer, last_resort)
    }

    fn build_kp_in(&self, suite: Ciphersuite, signer: &SignatureKeyPair, last_resort: bool) -> Kp {
        let mut builder = KeyPackage::builder();
        if last_resort {
            builder = builder
                .leaf_node_capabilities(
                    Capabilities::builder()
                        .extensions(vec![ExtensionType::LastResort])
                        .build(),
                )
                .mark_as_last_resort();
        }
        let bundle = builder
            .build(suite, &self.provider, signer, self.credential(signer))
            .expect("build key package");
        let kp = bundle.key_package();
        Kp {
            ref_hash: hex::encode(kp.hash_ref(self.provider.crypto()).expect("hash_ref").as_slice()),
            bytes: kp.tls_serialize_detached().expect("serialize key package"),
            last_resort,
            suite,
        }
    }

    /// A KeyPackage in `suite`, on the certified key for that suite's scheme.
    pub fn key_package_in(&self, suite: Ciphersuite, last_resort: bool) -> Kp {
        let signer = if suite.signature_algorithm() == SignatureScheme::ED25519 {
            &self.ed
        } else {
            &self.signer
        };
        self.build_kp_in(suite, signer, last_resort)
    }

    /// A KeyPackage on this device's certified key.
    pub fn key_package(&self) -> Kp {
        self.build_kp(&self.signer, false)
    }

    /// This device's last-resort KeyPackage (#1571).
    pub fn last_resort_key_package(&self) -> Kp {
        self.build_kp(&self.signer, true)
    }

    /// A self-consistent KeyPackage that claims this device's `user:device`
    /// credential but is signed by a key the account never certified.
    pub fn forged_key_package(&self) -> Kp {
        let rogue = SignatureKeyPair::new(SignatureScheme::MLDSA44).expect("rogue key");
        self.build_kp(&rogue, false)
    }

    /// Stand up `conversation_id` with this device as its only member.
    pub fn create_group(&self, conversation_id: &str) -> MlsGroup {
        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(SUITE)
            .use_ratchet_tree_extension(true)
            .build();
        MlsGroup::new_with_group_id(
            &self.provider,
            &self.signer,
            &config,
            GroupId::from_slice(conversation_id.as_bytes()),
            self.credential(&self.signer),
        )
        .expect("create group")
    }

    /// The group's current GroupInfo, as a client publishes it.
    pub fn group_info(&self, group: &MlsGroup) -> Vec<u8> {
        group
            .export_group_info(self.provider.crypto(), &self.signer, true)
            .expect("export group info")
            .tls_serialize_detached()
            .expect("serialize group info")
    }

    /// Commit `Add`s for `kps` and merge it, returning what a client submits.
    pub fn add(&self, group: &mut MlsGroup, kps: &[&Kp]) -> AddBundle {
        let packages: Vec<KeyPackage> = kps
            .iter()
            .map(|kp| {
                KeyPackageIn::tls_deserialize(&mut kp.bytes.as_slice())
                    .expect("parse key package")
                    .validate(self.provider.crypto(), ProtocolVersion::Mls10)
                    .expect("validate key package")
            })
            .collect();
        let bundle = group
            .commit_builder()
            .propose_adds(packages)
            .load_psks(self.provider.storage())
            .expect("load psks")
            .create_group_info(true)
            .build(self.provider.rand(), self.provider.crypto(), &self.signer, |_| true)
            .expect("build commit")
            .stage_commit(&self.provider)
            .expect("stage commit");
        let (commit, welcome, group_info) = bundle.into_messages();
        let out = AddBundle {
            commit: commit.tls_serialize_detached().expect("serialize commit"),
            welcome: welcome
                .expect("an add has a welcome")
                .tls_serialize_detached()
                .expect("serialize welcome"),
            group_info: group_info
                .expect("group info requested")
                .tls_serialize_detached()
                .expect("serialize group info"),
        };
        group.merge_pending_commit(&self.provider).expect("merge");
        out
    }
}

/// Store `kp` directly, as a DS that skipped the publish check (or a row
/// written before #1851) would hold it.
pub async fn plant_key_package(conn: &Connection, device: &MlsDevice, kp: &Kp) {
    conn.execute(
        "INSERT INTO mls_key_package \
         (ref_hash, user_id, key_package, device_id, ciphersuite, last_resort) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![
            kp.ref_hash.clone(),
            device.user_id.clone(),
            kp.bytes.clone(),
            device.device_id.clone(),
            i64::from(u16::from(SUITE)),
            i64::from(kp.last_resort),
        ],
    )
    .await
    .expect("plant key package");
}

/// A genuine Welcome admitting `user_id`'s `device_id`, with everything the
/// DS needs to accept it: the device certified under a (fresh) account key
/// and its KeyPackage stored. Each call returns new bytes. Re-keys the
/// account, so earlier certs for `user_id` stop verifying.
pub async fn welcome_for(conn: &Connection, user_id: &str, device_id: &str) -> Vec<u8> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    welcome_for_device_key(conn, user_id, device_id, seed).await
}

/// [`welcome_for`] a device whose ML-DSA-44 key derives from `seed` (see
/// [`MlsAccount::device_from_seed`]).
pub async fn welcome_for_device_key(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    seed: [u8; 32],
) -> Vec<u8> {
    let account = MlsAccount::new(conn, user_id).await;
    let device = account.device_from_seed(conn, device_id, seed).await;
    let kp = device.key_package();
    plant_key_package(conn, &device, &kp).await;
    let welcomer_id = format!("welcomer-{}", ulid::Ulid::new());
    let welcomer = MlsAccount::new(conn, &welcomer_id)
        .await
        .device(conn, &format!("{welcomer_id}-d"))
        .await;
    let mut group = welcomer.create_group(&format!("welcome-{}", ulid::Ulid::new()));
    welcomer.add(&mut group, &[&kp]).welcome
}

/// A KeyPackage for `user_id`'s `device_id` that the DS will serve, for tests
/// about something else (claim order, budgets, teardown). The account and
/// device keys are fixed per name for the whole test process, so every call,
/// in any test DB, certifies under the same keys and earlier packages for the
/// device stay serveable.
pub async fn serveable_key_package(conn: &Connection, user_id: &str, device_id: &str) -> Vec<u8> {
    shared_device(conn, user_id, device_id).await.key_package().bytes
}

/// The process-wide device behind [`serveable_key_package`], registered in
/// this DB: its packages, in any suite, are serveable here.
pub async fn shared_device(conn: &Connection, user_id: &str, device_id: &str) -> MlsDevice {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static SEEDS: OnceLock<Mutex<HashMap<String, [u8; 32]>>> = OnceLock::new();
    static ED: OnceLock<Mutex<HashMap<String, SignatureKeyPair>>> = OnceLock::new();
    let seed_for = |name: String| -> [u8; 32] {
        *SEEDS
            .get_or_init(Default::default)
            .lock()
            .expect("seed registry")
            .entry(name)
            .or_insert_with(|| {
                let mut seed = [0u8; 32];
                OsRng.fill_bytes(&mut seed);
                seed
            })
    };
    let account_seed = seed_for(format!("account:{user_id}"));
    let device_seed = seed_for(format!("device:{user_id}:{device_id}"));
    let ed = ED
        .get_or_init(Default::default)
        .lock()
        .expect("ed registry")
        .entry(format!("{user_id}:{device_id}"))
        .or_insert_with(|| SignatureKeyPair::new(SignatureScheme::ED25519).expect("Ed25519 key"))
        .clone();
    MlsAccount::from_seed(conn, user_id, account_seed)
        .await
        .device_with_keys(conn, device_id, device_seed, ed)
        .await
}
