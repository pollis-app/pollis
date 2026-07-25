//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage),
//! the ciphersuite constant, and the credential format used in MLS leaves.

use openmls::prelude::*;
use openmls_libcrux_crypto::CryptoProvider as LibcruxCrypto;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;

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
/// `openmls_rust_crypto::RustCrypto`, which serves the classic suite `CS`.
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

// ── Ciphersuite ───────────────────────────────────────────────────────────────

pub(crate) const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

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
