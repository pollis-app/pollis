//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage), the
//! ciphersuite constant, and the credential format used in MLS leaves.

use openmls::prelude::*;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::signal::mls_storage::MlsStore;

/// How far behind the newest generation it has opened for a sender a device
/// may still open one of that sender's messages (openmls'
/// `out_of_order_tolerance`; its default is 5).
///
/// Decryption follows DS arrival order (#1526); this is the backstop for residual reorders.
///
/// Cost: up to this many skipped keys per sender, wiped at the next commit.
pub(crate) const OUT_OF_ORDER_TOLERANCE: u32 = 100;

/// openmls' default, kept: how far AHEAD of the newest opened generation a
/// message may be (a sender who sent that many this device never saw).
pub(crate) const MAXIMUM_FORWARD_DISTANCE: u32 = 1000;

/// The one sender-ratchet configuration, so create, Welcome and external join cannot drift.
pub(crate) fn sender_ratchet_configuration() -> SenderRatchetConfiguration {
    SenderRatchetConfiguration::new(OUT_OF_ORDER_TOLERANCE, MAXIMUM_FORWARD_DISTANCE)
}

/// The join configuration for a Welcome or an external join.
pub(crate) fn join_config() -> MlsGroupJoinConfig {
    MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .sender_ratchet_configuration(sender_ratchet_configuration())
        .build()
}

/// Bring a stored group's sender-ratchet window up to
/// [`sender_ratchet_configuration`], writing only when it differs. Groups
/// created before #1526 carry openmls' default window of 5.
pub(crate) fn upgrade_sender_ratchet<S: openmls_traits::storage::StorageProvider<1>>(
    group: &mut MlsGroup,
    storage: &S,
) {
    if group.configuration().sender_ratchet_configuration() == &sender_ratchet_configuration() {
        return;
    }
    let Ok(mut value) = serde_json::to_value(group.configuration()) else {
        return;
    };
    let Ok(wanted) = serde_json::to_value(sender_ratchet_configuration()) else {
        return;
    };
    let Some(fields) = value.as_object_mut() else {
        return;
    };
    fields.insert("sender_ratchet_configuration".to_string(), wanted);
    let Ok(upgraded) = serde_json::from_value::<MlsGroupJoinConfig>(value) else {
        return;
    };
    if let Err(e) = group.set_configuration(storage, &upgraded) {
        crate::diag!("[mls] could not widen the sender-ratchet window: {e:?}");
    }
}

/// An OpenMLS provider: some crypto backend `C` composed with our SQLite-backed
/// `MlsStore`. Storage is ciphersuite- and backend-agnostic (it stores opaque
/// blobs in `mls_kv`), so only the crypto half varies.
///
/// Composes our own store, not openmls' bundled `Provider`, whose in-memory storage would lose group state on restart.
pub struct MlsProvider<'a, C> {
    crypto: C,
    store: MlsStore<'a>,
}

impl<'a, C> MlsProvider<'a, C> {
    /// Borrow the raw sqlite connection backing `mls_kv`, for Pollis's own rows.
    pub fn raw_conn(&self) -> &rusqlite::Connection {
        self.store.raw_conn()
    }
}

impl<'a, C> OpenMlsProvider for MlsProvider<'a, C>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    // The backend serves as crypto AND rand provider (it holds its own CSPRNG),
    // so both associated types point at the same value.
    type CryptoProvider = C;
    type RandProvider = C;
    type StorageProvider = MlsStore<'a>;

    fn storage(&self) -> &Self::StorageProvider {
        &self.store
    }

    fn crypto(&self) -> &Self::CryptoProvider {
        &self.crypto
    }

    fn rand(&self) -> &Self::RandProvider {
        &self.crypto
    }
}

/// **The provider every path uses.** Backed by
/// `openmls_rust_crypto::RustCrypto`.
///
/// Adding a backend back is a security decision: check every primitive's advisory status.
pub type PollisProvider<'a> = MlsProvider<'a, RustCrypto>;

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            crypto: RustCrypto::default(),
            store: MlsStore::new(conn),
        }
    }
}

/// **The** Pollis ciphersuite: MLS code point `0x0052` — X-Wing (X25519 +
/// ML-KEM-768) + ChaCha20-Poly1305 + SHA-384 + **ML-DSA-44**. Served by
/// [`PollisProvider`]. Post-quantum in both confidentiality and authentication.
///
/// The KEM is a hybrid, so the suite is no weaker than X25519 even if ML-KEM falls.
///
/// The code point is provisional (`draft-ietf-mls-pq-ciphersuites`); changing it is a wire break, handled by `super::migrate`.
pub(crate) const CS_PQ: Ciphersuite =
    Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;

/// The suite this build births groups on and publishes KeyPackages in.
///
/// [`CS_PQ`] in every shipped build; a function so the `test-harness` build can move it and exercise migration.
#[cfg(not(feature = "test-harness"))]
#[inline]
pub(crate) fn current_suite() -> Ciphersuite {
    CS_PQ
}

/// `0` means "no override" — the current suite is [`CS_PQ`]. Any other value is
/// an MLS code point the harness has pinned.
#[cfg(feature = "test-harness")]
static CURRENT_SUITE_OVERRIDE: std::sync::atomic::AtomicU16 =
    std::sync::atomic::AtomicU16::new(0);

#[cfg(feature = "test-harness")]
pub(crate) fn current_suite() -> Ciphersuite {
    match CURRENT_SUITE_OVERRIDE.load(std::sync::atomic::Ordering::Relaxed) {
        0 => CS_PQ,
        code => Ciphersuite::try_from(code).expect("test-harness: unknown ciphersuite override"),
    }
}

/// Pin the suite this process treats as current, or `None` to restore [`CS_PQ`].
///
/// Test-harness only: models a code-point change across a running fleet.
#[cfg(feature = "test-harness")]
pub fn set_current_suite_override(code_point: Option<u16>) {
    CURRENT_SUITE_OVERRIDE.store(
        code_point.unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The signature scheme a suite's leaves sign with.
///
/// A function of the suite: a stored group stays on its old suite until migrated.
pub(crate) fn signature_scheme(suite: Ciphersuite) -> SignatureScheme {
    suite.signature_algorithm()
}

/// Load a group from local storage **without** constructing a provider.
///
/// `MlsGroup::load` needs only storage, not crypto.
///
/// Resolves the conversation's suite generation (#454 P4); use [`load_stored_group_at`] to name a lineage.
pub(crate) fn load_stored_group(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Option<MlsGroup> {
    let generation = crate::commands::mls::generation::local_generation(conn, conversation_id);
    load_stored_group_at(conn, conversation_id, generation)
}

/// [`load_stored_group`] pinned to a generation, for migration and adoption.
pub(crate) fn load_stored_group_at(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<MlsGroup> {
    let store = MlsStore::new(conn);
    let group_id = crate::commands::mls::generation::mls_group_id(conversation_id, generation);
    let mut group = MlsGroup::load(&store, &group_id).ok().flatten()?;
    upgrade_sender_ratchet(&mut group, &store);
    Some(group)
}

/// The storage half on its own, for the storage-only mutations
/// ([`MlsGroup::delete`], `clear_pending_commit`) that pair with
/// [`load_stored_group`].
pub(crate) fn store_only(conn: &rusqlite::Connection) -> MlsStore<'_> {
    MlsStore::new(conn)
}

/// Read the ciphersuite of an inbound `Welcome` **before** any crypto touches it.
///
/// `Welcome::ciphersuite()` is `pub(crate)` in openmls, so read it off the wire.
///
/// Safe: this is the RFC 9420 §12.4.3.1 layout:
///
/// ```text
/// struct {
///     CipherSuite cipher_suite;          // first field, 2 bytes, big-endian
///     EncryptedGroupSecrets secrets<V>;
///     opaque encrypted_group_info<V>;
/// } Welcome;
/// ```
///
/// Returns `None` if unreadable or unimplemented, meaning "cannot join".
pub(crate) fn welcome_ciphersuite(welcome: &Welcome) -> Option<Ciphersuite> {
    let bytes = welcome.tls_serialize_detached().ok()?;
    let code = u16::from_be_bytes([*bytes.first()?, *bytes.get(1)?]);
    Ciphersuite::try_from(code).ok()
}

/// Read a locally-stored group's ciphersuite. `None` when no group is stored for
/// `conversation_id` (not yet joined, or forgotten) — callers decide the default.
pub(crate) fn stored_group_ciphersuite(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Option<Ciphersuite> {
    load_stored_group(conn, conversation_id).map(|g| g.ciphersuite())
}

/// Build an MLS `Credential` encoding both user and device identity.
///
/// Format: `"user_id:device_id"` as UTF-8 bytes inside a `BasicCredential`.
pub fn make_credential(user_id: &str, device_id: &str) -> Credential {
    BasicCredential::new(format!("{user_id}:{device_id}").into_bytes()).into()
}

/// Extract the `user_id` from a credential produced by `make_credential`.
///
/// Handles legacy credentials that contain only `user_id` (no colon).
pub fn parse_credential_user_id(cred: &Credential) -> String {
    let s = String::from_utf8_lossy(cred.serialized_content());
    s.split_once(':').map(|(u, _)| u).unwrap_or(&s).to_string()
}

/// Extract the `device_id` from a credential produced by `make_credential`.
///
/// Returns `None` for legacy credentials that contain only `user_id`.
pub fn parse_credential_device_id(cred: &Credential) -> Option<String> {
    let s = String::from_utf8_lossy(cred.serialized_content());
    s.split_once(':').map(|(_, d)| d.to_string())
}
