//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage), the
//! ciphersuite constant, and the credential format used in MLS leaves.
//!
//! There is exactly one suite ([`CS_PQ`]) and one backend, so nothing here
//! dispatches: every MLS operation runs through `PollisProvider` and every leaf
//! signs ML-DSA-44. #454 introduced a second suite so a fleet mid-upgrade could
//! still talk, and #669 retired the first one once that was done; the code that
//! chose between them is gone rather than pinned to a winner.

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

/// **The provider every path uses.** Backed by
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
/// to — `0x0052`, see [`CS_PQ`] — keeps X-Wing as its KEM but pairs it with
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

/// **The** Pollis ciphersuite: MLS code point `0x0052` — X-Wing (X25519 +
/// ML-KEM-768) + ChaCha20-Poly1305 + SHA-384 + **ML-DSA-44**. Served by
/// [`PollisProvider`]. Post-quantum in both confidentiality and authentication.
///
/// It got here in two steps. #454 added it alongside the classic suite
/// (`0x0001`, X25519 + AES-128-GCM + SHA-256 + Ed25519) as `0x004D`, which was
/// post-quantum in *confidentiality* only — its leaves still signed Ed25519, so
/// a quantum adversary could forge group membership even though it could not
/// read the traffic. #668 moved it to `0x0052`, keeping exactly the same KEM
/// (`HpkeKemType::XWingKemDraft6`, so the hybrid secrecy #454 argued for is
/// unchanged) and replacing the signature with ML-DSA-44. #669 then retired the
/// classic suite, which is why this is a constant and not a choice.
///
/// The KEM is a *hybrid* of X25519 and ML-KEM-768, so the suite is no weaker
/// than X25519 even if ML-KEM falls. That is the property worth remembering: PQ
/// here is additive, never a substitution of new maths for old.
///
/// The code point is provisional — `draft-ietf-mls-pq-ciphersuites` has already
/// renumbered once (`-06` dropped the standalone X-Wing code point in favour of
/// `0x004E`/`0x004F`/`0x0052`), and it may renumber again. A code point change
/// is a wire-format break for every group already on this suite, so treat it as
/// a migration, not a version bump: change this constant and every group
/// migrates itself onto it via `super::migrate`, which is suite-agnostic and
/// exists for exactly this. See the `[patch.crates-io]` note in the workspace
/// `Cargo.toml` and `docs/pq-hybrid-mls-design.md` §7.
pub(crate) const CS_PQ: Ciphersuite =
    Ciphersuite::MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44;

/// The suite this build births groups on and publishes KeyPackages in.
///
/// [`CS_PQ`], always, in every shipped build — this is a function and not a
/// `use` of the constant for one reason: `migrate` fires on a group whose stored
/// suite is not the current one, and in production that state arises *only* from
/// changing `CS_PQ` and shipping. A test cannot wait for a draft to renumber, so
/// the `test-harness` build lets the flows suite move the current suite under a
/// running fleet and watch the migration it triggers. Without that seam the
/// entire successor-lineage mechanism — the part of Pollis that has to work
/// perfectly on the one day it ever runs — would ship untested.
///
/// The override is deliberately global rather than per-client: a code point is a
/// property of the *build*, and a world where two clients disagree about the
/// current suite is not a state production can reach.
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
/// Test-harness only. Models a code-point change the way production would
/// experience one: set it before the fleet signs up to get a world running on
/// the old number, then clear it and let each client's next login and sweep
/// carry it across — the same two steps (`ensure_mls_key_package`, then
/// `migrate_to_current_suite_if_due`) a real renumber would take.
#[cfg(feature = "test-harness")]
pub fn set_current_suite_override(code_point: Option<u16>) {
    CURRENT_SUITE_OVERRIDE.store(
        code_point.unwrap_or(0),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The signature scheme a suite's leaves sign with.
///
/// Still a function of the suite rather than the constant `MLDSA44`, because
/// the suite is the authority: it is read off a *stored group* on every
/// decrypt/commit path, and a group persisted before a code-point change is
/// still on its old suite until `super::migrate` moves it. Hard-coding the
/// scheme would silently sign that group's leaves with the wrong key.
///
/// A thin wrapper over `Ciphersuite::signature_algorithm()` on purpose — it
/// gives the call sites a name to point at, and `signature_scheme_of_suite` in
/// `tests.rs` pins the answer so a suite swap cannot silently change which key
/// a leaf is signed with.
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
