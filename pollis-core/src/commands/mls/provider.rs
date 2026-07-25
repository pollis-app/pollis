//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage),
//! the ciphersuite constant, and the credential format used in MLS leaves.

use openmls::prelude::*;
use openmls_libcrux_crypto::CryptoProvider as LibcruxCrypto;
use openmls_traits::OpenMlsProvider;

use crate::signal::mls_storage::MlsStore;

// ── Provider ─────────────────────────────────────────────────────────────────

/// Combines the libcrux-backed `CryptoProvider` with our SQLite-backed
/// `MlsStore` to satisfy the `OpenMlsProvider` bound required by all openmls
/// API calls.
///
/// We use libcrux (not `openmls_rust_crypto::RustCrypto`) because RustCrypto
/// `unimplemented!()`-panics on the post-quantum hybrid ciphersuite that later
/// phases of #454 depend on, while libcrux implements it and serves the classic
/// suite Pollis ships today equally well (proven behaviour-preserving and
/// wire-compatible with RustCrypto by the cross-provider interop test in
/// `commands/mls/tests.rs`). We deliberately compose libcrux's `CryptoProvider`
/// rather than its bundled `Provider`, which hardcodes an in-memory storage
/// backend that would silently drop all MLS group state on restart.
pub struct PollisProvider<'a> {
    crypto: LibcruxCrypto,
    store: MlsStore<'a>,
}

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            // `CryptoProvider::new()` is fallible only because it seeds a CSPRNG
            // from the OS RNG. If the OS RNG is unavailable every other crypto
            // path in the app is already fatally broken, so panicking here (vs.
            // rippling a `Result` through `new`'s many call sites in a purely
            // behaviour-preserving change) surfaces the same unrecoverable
            // condition without weakening any signature.
            crypto: LibcruxCrypto::new()
                .expect("OS RNG unavailable — no crypto path in the app can work"),
            store: MlsStore::new(conn),
        }
    }

    /// Borrow the raw sqlite connection backing `mls_kv`. Used for custom
    /// rows Pollis writes alongside openmls state (e.g. the stable per-
    /// device signing key reference).
    pub fn raw_conn(&self) -> &rusqlite::Connection {
        self.store.raw_conn()
    }
}

impl<'a> OpenMlsProvider for PollisProvider<'a> {
    // libcrux's `CryptoProvider` serves as BOTH the crypto and rand provider
    // (it holds the reseeding CSPRNG), exactly as `RustCrypto` did for both.
    type CryptoProvider = LibcruxCrypto;
    type RandProvider = LibcruxCrypto;
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
