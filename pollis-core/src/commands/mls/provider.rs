//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage), the
//! ciphersuite constants and the suite-invariant signature scheme, and the
//! credential format used in MLS leaves.
//!
//! Suite and provider are chosen together — `CS_CLASSIC` with `PollisProvider`,
//! `CS_HYBRID` with `PollisPqProvider` — because each crypto backend implements
//! only one of the two. The functions that create suite-bound material take
//! both as arguments so a caller cannot silently mismatch them.

use openmls::prelude::*;
use openmls_libcrux_crypto::CryptoProvider as LibcruxCrypto;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::signal::mls_storage::MlsStore;

// ── Provider ─────────────────────────────────────────────────────────────────

/// An OpenMLS provider: some crypto backend `C` composed with our SQLite-backed
/// `MlsStore`. Storage is ciphersuite- and backend-agnostic (it stores opaque
/// blobs in `mls_kv`), so only the crypto half varies.
///
/// **The crypto backend is chosen by ciphersuite, and that choice is load-bearing
/// for security — see `PollisProvider` and `PollisPqProvider` below.** We compose
/// a backend with our own store rather than using either crate's bundled
/// `Provider` type: those hardcode in-memory storage, which would silently drop
/// all MLS group state on restart.
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
    // Both backends serve as crypto AND rand provider (each holds its own
    // CSPRNG), so both associated types point at the same value.
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

/// **The provider every production path uses.** Backed by
/// `openmls_rust_crypto::RustCrypto`, which serves [`CS_CLASSIC`].
///
/// Why not libcrux for everything, when libcrux serves the classic suite too and
/// would spare us a second backend? Because the classic suite's AEAD is
/// AES-128-GCM, and libcrux's AES-GCM implementation has an **unpatched**
/// non-constant-time authentication-tag check (RUSTSEC-2026-0211,
/// `libcrux-aesgcm <= 0.0.8`; the fix exists only in the renamed `libcrux-aes`
/// 0.0.9, which no released `openmls_libcrux_crypto` depends on). Routing every
/// message decrypt through that would be a real side-channel regression against
/// today's shipping behaviour, traded for a post-quantum capability nothing yet
/// uses. RustCrypto's `aes-gcm` compares tags with `subtle`'s constant-time
/// primitives, so classic traffic stays where it is.
///
/// This routing is what makes the `RUSTSEC-2026-0211` entry in `deny.toml`
/// truthful. It is pinned by `classic_provider_never_routes_to_libcrux` in
/// `commands/mls/tests.rs` — do not "simplify" the two providers into one
/// without re-reading that test and the advisory.
pub type PollisProvider<'a> = MlsProvider<'a, RustCrypto>;

/// The provider for the post-quantum hybrid suite (#454). Backed by
/// `openmls_libcrux_crypto::CryptoProvider`, the only released backend that
/// implements `MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519` — `RustCrypto`
/// `unimplemented!()`-panics on it.
///
/// **Nothing in production constructs this yet.** It exists so the hybrid suite
/// stays reachable and tested while the phased rollout (#454 P2-P4) lands. Its
/// suite is ChaCha20-Poly1305-based, so the AES-GCM advisory above does not
/// apply to it.
pub type PollisPqProvider<'a> = MlsProvider<'a, LibcruxCrypto>;

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            crypto: RustCrypto::default(),
            store: MlsStore::new(conn),
        }
    }
}

impl<'a> PollisPqProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            // `CryptoProvider::new()` is fallible only because it seeds a CSPRNG
            // from the OS RNG. If the OS RNG is unavailable every other crypto
            // path in the app is already fatally broken, so panicking here (vs.
            // rippling a `Result` through every call site) surfaces the same
            // unrecoverable condition without weakening any signature.
            crypto: LibcruxCrypto::new()
                .expect("OS RNG unavailable — no crypto path in the app can work"),
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

/// The post-quantum hybrid suite (#454): MLS code point `0x004D` — X-Wing
/// (X25519 + ML-KEM-768) + ChaCha20-Poly1305 + SHA-256 + Ed25519.
///
/// Served ONLY by [`PollisPqProvider`]; `RustCrypto` `unimplemented!()`-panics
/// on it, so pairing this suite with [`PollisProvider`] is a crash, not a
/// fallback. **Nothing in production selects it yet** — it exists so the suite
/// stays compiled, reachable and tested while #454 P2-P4 land.
///
/// The code point is still a moving target upstream (`draft-ietf-mls-pq-
/// ciphersuites-06` dropped X-Wing in favour of `0x004E`/`0x004F`/`0x0052`), so
/// treat `0x004D` as experimental until the draft settles — see
/// `docs/pq-hybrid-mls-design.md` §7.
/// Unused outside tests until #454 P2 — that is the whole point of this phase:
/// the seam exists and is exercised, but no production path selects it yet.
#[allow(dead_code)]
pub(crate) const CS_HYBRID: Ciphersuite =
    Ciphersuite::MLS_256_XWING_CHACHA20POLY1305_SHA256_Ed25519;

/// The signature scheme, which is deliberately NOT a per-suite parameter.
///
/// Both suites above sign with Ed25519, so a device's stable signing key — the
/// same key that signs DS requests and that `user_device.device_cert` certifies
/// — is shared across them by design. Sites that only need
/// `Ciphersuite::signature_algorithm()` (device signer load/create, group
/// reload, credential construction) therefore take no suite argument; they use
/// this constant, and `signature_scheme_is_suite_invariant` in `tests.rs` pins
/// that both suites really do agree with it.
pub(crate) const SIGNATURE_SCHEME: SignatureScheme = SignatureScheme::ED25519;

// ── Suite dispatch ───────────────────────────────────────────────────────────

/// Load a group from local storage **without** choosing a crypto backend.
///
/// This is what makes suite dispatch possible at all. `MlsGroup::load` is
/// generic over `StorageProvider`, not `OpenMlsProvider`: it deserialises
/// stored blobs and touches no crypto. `MlsStore` is the crypto-independent
/// half of [`MlsProvider`], so it can be constructed alone — which resolves the
/// chicken-and-egg of "you need the group to learn its suite, and the suite to
/// pick the provider".
///
/// Also the right call for every read-only inspection (does the group exist?
/// what epoch is it at?) and for the storage-only mutations `MlsGroup::delete`
/// and `clear_pending_commit`: none of them need a backend, so none of them
/// need to dispatch.
pub(crate) fn load_stored_group(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Option<MlsGroup> {
    let store = MlsStore::new(conn);
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
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
/// Returns `None` if the Welcome fails to serialise or names a code point we do
/// not implement — both of which the caller must treat as "cannot join", never
/// as "fall back to classic": handing a hybrid Welcome to `PollisProvider`
/// panics (`RustCrypto` `unimplemented!()`s on `CS_HYBRID`).
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

/// Run `$body` against the [`MlsProvider`] that serves `$suite`.
///
/// Dispatch has to be a macro rather than a `dyn` object or a closure:
/// `OpenMlsProvider` has associated types (`CryptoProvider`, `RandProvider`,
/// `StorageProvider`), so it is not object-safe, and Rust closures cannot be
/// generic over the backend. The body is therefore monomorphised once per
/// backend. **Keep bodies small** — the established shape is a one-line call to
/// a `fn foo<C>(provider: &MlsProvider<'_, C>, …)` helper, so the duplicated
/// code is a single call and the real work is compiled twice by the normal
/// generic machinery instead of by macro expansion.
macro_rules! with_suite_provider {
    ($conn:expr, $suite:expr, |$p:ident| $body:block) => {{
        let __conn = $conn;
        if $suite == $crate::commands::mls::provider::CS_HYBRID {
            let $p = $crate::commands::mls::provider::PollisPqProvider::new(__conn);
            $body
        } else {
            let $p = $crate::commands::mls::provider::PollisProvider::new(__conn);
            $body
        }
    }};
}

/// Run `$body` against the provider serving the ciphersuite of the group stored
/// locally for `$conversation_id`.
///
/// A conversation with no local group falls back to [`CS_CLASSIC`]. That is the
/// right default for every caller: paths that read existing state find nothing
/// either way, and paths that *create* state (group creation, Welcome, external
/// join) learn their suite from the wire and use [`with_suite_provider`]
/// directly rather than this macro.
macro_rules! with_group_provider {
    ($conn:expr, $conversation_id:expr, |$p:ident| $body:block) => {{
        let __conn = $conn;
        let __suite = $crate::commands::mls::provider::stored_group_ciphersuite(
            __conn,
            $conversation_id,
        )
        .unwrap_or($crate::commands::mls::provider::CS_CLASSIC);
        with_suite_provider!(__conn, __suite, |$p| $body)
    }};
}

pub(crate) use with_group_provider;
pub(crate) use with_suite_provider;

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
