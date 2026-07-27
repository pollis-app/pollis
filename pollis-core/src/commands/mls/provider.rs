//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage), the
//! ciphersuite constants, and the credential format used in MLS leaves.
//!
//! One backend serves both suites (#668), so there is no suite→provider
//! dispatch: every MLS operation runs through `PollisProvider`. What *is*
//! per-suite is the signature scheme — [`CS_CLASSIC`] signs with Ed25519,
//! [`CS_HYBRID`] with ML-DSA-44 — so anything that loads or creates a signing
//! key takes the scheme from the suite (or from the stored group) rather than
//! from a constant.

use openmls::prelude::*;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::signal::mls_storage::MlsStore;

// ── Provider ─────────────────────────────────────────────────────────────────

/// An OpenMLS provider: some crypto backend `C` composed with our SQLite-backed
/// `MlsStore`. Storage is ciphersuite- and backend-agnostic (it stores opaque
/// blobs in `mls_kv`), so only the crypto half varies.
///
/// Still generic over `C` although [`PollisProvider`] is its only instantiation:
/// `OpenMlsProvider` composes a crypto backend with a storage backend, and
/// keeping that shape means swapping backends stays a one-line change here
/// rather than a signature change everywhere. We compose a backend with our own
/// store rather than using the crate's bundled `Provider` type, which hardcodes
/// in-memory storage and would silently drop all MLS group state on restart.
pub struct MlsProvider<'a, C> {
    crypto: C,
    store: MlsStore<'a>,
}

impl<'a, C> MlsProvider<'a, C> {
    /// Borrow the raw sqlite connection backing `mls_kv`. Used for custom
    /// rows Pollis writes alongside openmls state (e.g. the stable per-
    /// device signing key reference).
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

/// **The provider every path uses**, for both suites. Backed by
/// `openmls_rust_crypto::RustCrypto`.
///
/// #454 needed a second backend: its hybrid suite was X-Wing / `0x004D`, which
/// only `openmls_libcrux_crypto` implemented, and routing the *classic* suite
/// there too was not an option — libcrux's AES-GCM has an unpatched
/// non-constant-time authentication-tag check (RUSTSEC-2026-0211,
/// `libcrux-aesgcm <= 0.0.8`), and the classic suite's AEAD is AES-128-GCM. So
/// the suite→backend routing existed to keep classic traffic away from that tag
/// check.
///
/// #668 dissolved the problem rather than routing around it. The suite it moves
/// to — `0x0052`, see [`CS_HYBRID`] — keeps X-Wing as its KEM but pairs it with
/// ChaCha20-Poly1305 and ML-DSA-44, and RustCrypto implements it while libcrux
/// implements no ML-DSA suite at all. `openmls_libcrux_crypto` is therefore out
/// of the dependency tree entirely, and with it the advisory: there is nothing
/// left to route away from. RustCrypto's `aes-gcm` compares tags with `subtle`'s
/// constant-time primitives.
///
/// Adding a backend back is a security decision, not a plumbing one. Check the
/// advisory status of every primitive it would serve before reintroducing one.
pub type PollisProvider<'a> = MlsProvider<'a, RustCrypto>;

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            crypto: RustCrypto::default(),
            store: MlsStore::new(conn),
        }
    }
}

// ── Ciphersuites ──────────────────────────────────────────────────────────────

/// The suite every Pollis group uses today: MLS code point `0x0001` —
/// X25519 + AES-128-GCM + SHA-256 + Ed25519. Served by [`PollisProvider`].
///
/// Every production call site passes this explicitly rather than relying on a
/// default, so "which suite is this group / key package?" is answerable by
/// reading the call and not by tracing a constant.
pub(crate) const CS_CLASSIC: Ciphersuite =
    Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

/// The post-quantum suite: MLS code point `0x0052` — X-Wing (X25519 +
/// ML-KEM-768) + ChaCha20-Poly1305 + SHA-384 + **ML-DSA-44**. Served by
/// [`PollisProvider`].
///
/// #454 shipped this constant as X-Wing / `0x004D`, which is post-quantum in
/// *confidentiality* only: its leaves still signed with Ed25519, so a quantum
/// adversary could forge group membership even though it could not read the
/// traffic. #668 moves it to `0x0052`, which keeps exactly the same KEM
/// (`HpkeKemType::XWingKemDraft6`, so the hybrid secrecy #454 argued for is
/// unchanged) and replaces the signature with ML-DSA-44.
///
/// The code point is provisional — `draft-ietf-mls-pq-ciphersuites` has already
/// renumbered once (`-06` dropped the standalone X-Wing code point in favour of
/// `0x004E`/`0x004F`/`0x0052`), and it may renumber again. A code point change
/// is a wire-format break for every group already on this suite, so treat it as
/// a migration, not a version bump; see the `[patch.crates-io]` note in the
/// workspace `Cargo.toml` and `docs/pq-hybrid-mls-design.md` §7.
///
/// The name stays `CS_HYBRID` because the property it names is still true and
/// still the point: the KEM is a hybrid of X25519 and ML-KEM-768, so the suite
/// is no weaker than X25519 even if ML-KEM falls.
pub(crate) const CS_HYBRID: Ciphersuite =
    Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;

/// The signature scheme a suite's leaves sign with.
///
/// Under #454 this was a constant, because both suites signed Ed25519 — a
/// device presented one signing key everywhere and the device cert certified
/// exactly that key. #668 ends that: [`CS_CLASSIC`] still signs Ed25519 and
/// [`CS_HYBRID`] signs ML-DSA-44, so a device holds one signing key *per
/// scheme* (see `device::load_or_create_device_signer`).
///
/// A thin wrapper over `Ciphersuite::signature_algorithm()` on purpose — it
/// gives the call sites a name to point at, and `signature_scheme_per_suite` in
/// `tests.rs` pins the two answers so a suite swap cannot silently change which
/// key a leaf is signed with.
pub(crate) fn signature_scheme(suite: Ciphersuite) -> SignatureScheme {
    suite.signature_algorithm()
}

// ── Storage-only access ──────────────────────────────────────────────────────

/// Load a group from local storage **without** constructing a provider.
///
/// `MlsGroup::load` is generic over `StorageProvider`, not `OpenMlsProvider`:
/// it deserialises stored blobs and touches no crypto. `MlsStore` is the
/// crypto-independent half of [`MlsProvider`], so it can be constructed alone.
///
/// The right call for every read-only inspection (does the group exist? what
/// epoch is it at? which suite is it in?) and for the storage-only mutations
/// `MlsGroup::delete` and `clear_pending_commit`: none of them need a backend.
///
/// Resolves the conversation's **suite generation** (#454 P4) before loading, so
/// a device that has migrated to the hybrid suite transparently reads and writes
/// its successor group. Every caller that means "this conversation's live group"
/// gets that for free; the handful that must name a specific lineage — the
/// migrator standing up a successor, a joiner draining a predecessor — call
/// [`load_stored_group_at`] instead.
pub(crate) fn load_stored_group(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Option<MlsGroup> {
    let generation = crate::commands::mls::generation::local_generation(conn, conversation_id);
    load_stored_group_at(conn, conversation_id, generation)
}

/// [`load_stored_group`] pinned to an explicit suite generation, for the two
/// paths that legitimately hold both lineages at once: migration (create the
/// successor while the predecessor still serves traffic) and adoption (drain the
/// predecessor after the successor's Welcome has landed).
pub(crate) fn load_stored_group_at(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    generation: i64,
) -> Option<MlsGroup> {
    let store = MlsStore::new(conn);
    let group_id = crate::commands::mls::generation::mls_group_id(conversation_id, generation);
    MlsGroup::load(&store, &group_id).ok().flatten()
}

/// The storage half on its own, for the storage-only mutations
/// ([`MlsGroup::delete`], `clear_pending_commit`) that pair with
/// [`load_stored_group`].
pub(crate) fn store_only(conn: &rusqlite::Connection) -> MlsStore<'_> {
    MlsStore::new(conn)
}

/// Read the ciphersuite of an inbound `Welcome` **before** any crypto touches it.
///
/// Joining is the one dispatch site with no local group to read a suite from and
/// no crypto-free accessor to ask: `Welcome::ciphersuite()` is `pub(crate)` in
/// openmls, and `ProcessedWelcome::new_from_welcome` — which would expose the
/// suite via `unverified_group_info()` — already needs the provider we are
/// trying to choose. So we read it off the wire.
///
/// This is safe to hardcode because it is the RFC 9420 §12.4.3.1 struct layout,
/// not an openmls implementation detail:
///
/// ```text
/// struct {
///     CipherSuite cipher_suite;          // first field, 2 bytes, big-endian
///     EncryptedGroupSecrets secrets<V>;
///     opaque encrypted_group_info<V>;
/// } Welcome;
/// ```
///
/// Since #668 one backend serves both suites, so this no longer chooses a
/// provider — but it still decides whether a Welcome is joinable at all, and
/// callers still need it to name the scheme the joining leaf signs under.
/// Returns `None` if the Welcome fails to serialise or names a code point we do
/// not implement; both mean "cannot join", never "fall back to classic".
/// `welcome_suite_is_readable_on_both_suites` in `tests.rs` pins this against
/// real Welcomes of each suite.
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

// #454 put three `with_*_provider!` macros here — `with_suite_provider!`,
// `with_group_provider!`, `with_lineage_provider!` — that picked a crypto
// backend from a suite. Dispatch had to be a macro because `OpenMlsProvider`
// has associated types and so is not object-safe, and closures cannot be
// generic over the backend; each one expanded its body once per backend.
//
// #668 deleted them along with the second backend. Two of the three had to read
// the caller's suite before they could dispatch, and reading a suite means
// deserialising the whole stored group — work every call site then repeated
// inside the body. With one backend that read bought nothing, so the call sites
// now construct `PollisProvider::new(conn)` directly.

// ── Credential helpers ───────────────────────────────────────────────────────

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
    let s = String::from_utf8_lossy(cred.serialized_content()).into_owned();
    s.split_once(':').map(|(_, d)| d.to_string())
}
