//! MLS provider and credential helpers.
//!
//! Houses `PollisProvider` (the OpenMls provider wiring crypto + storage),
//! the ciphersuite constant, and the credential format used in MLS leaves.

use openmls::prelude::*;
use openmls_rust_crypto::RustCrypto;
use openmls_traits::OpenMlsProvider;

use crate::signal::mls_storage::MlsStore;

// ── Provider ─────────────────────────────────────────────────────────────────

/// Combines `RustCrypto` with our SQLite-backed `MlsStore` to satisfy the
/// `OpenMlsProvider` bound required by all openmls API calls.
pub struct PollisProvider<'a> {
    crypto: RustCrypto,
    store: MlsStore<'a>,
}

impl<'a> PollisProvider<'a> {
    pub fn new(conn: &'a rusqlite::Connection) -> Self {
        Self {
            crypto: RustCrypto::default(),
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
    type CryptoProvider = RustCrypto;
    type RandProvider = RustCrypto;
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
