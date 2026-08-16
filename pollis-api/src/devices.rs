//! Device and key-package lifecycle: publish/claim/replenish key packages,
//! re-sign device certs, register push tokens.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct PublishKeyPackagesBody {
    /// The publishing device. Must be one of the actor's own devices; the rows
    /// are written with `user_id = actor` regardless, so this only labels which
    /// device produced them.
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    /// No-auth fallback for the actor; when signed it must equal the
    /// authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ReplenishKeyPackagesBody {
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ClaimKeyPackageBody {
    /// The user whose published key package is being claimed (the device being
    /// added to the MLS group). NOT the caller — claiming a *peer's* package is
    /// how you add them, so this is intentionally not bound to the signer.
    pub target_user_id: String,
    /// When present, narrow the claim to one of the target user's specific
    /// devices (the live `reconcile` add path always supplies this). When absent,
    /// any of the target user's unclaimed packages is eligible (the user-scoped
    /// `fetch_mls_key_package` path).
    #[serde(default)]
    pub target_device_id: Option<String>,
    /// Narrow the claim to packages published in this MLS ciphersuite. Optional:
    /// absent means [`CIPHERSUITE_PQ`], the only suite in use.
    ///
    /// Suite pools do not contaminate each other. Asking for a suite the target
    /// has no unclaimed package in yields `ClaimOutcome::NoKeyPackage`, the
    /// same ordinary control-flow result as an exhausted pool — never a package
    /// in the wrong suite. Since #669 there is one live pool, but the narrowing
    /// is what keeps a stale package from a retired code point from being handed
    /// out as if it were current, and it is how a future renumber stays a query
    /// rather than a guess.
    #[serde(default)]
    pub ciphersuite: Option<i64>,
}

#[derive(Serialize, Deserialize)]
pub struct ResignDeviceCertsBody {
    pub certs: Vec<ResignedCert>,
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PushTokenBody {
    pub token: String,
    pub platform: String,
    /// RFC3339 timestamp the client stamped. Plain text (never parsed here).
    pub updated_at: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// The MLS code point of `MLS_128_MLKEM768X25519_CHACHA20POLY1305_SHA384_MLDSA44`
/// — the post-quantum suite, and since #669 the only suite Pollis uses. It is
/// also the default for any request that does not name one.
///
/// The DS only ever uses this as a routing/label value; it never parses the
/// opaque `key_package` blob to confirm the suite.
///
/// #454 shipped this as `0x004D` (X-Wing KEM, Ed25519 signatures) alongside the
/// classic `0x0001`; #668 moved it to `0x0052`, which keeps the same X-Wing KEM
/// but signs ML-DSA-44, so the authentication half is post-quantum too; #669
/// retired `0x0001`. These are provisional `draft-ietf-mls-pq-ciphersuites` code
/// points and the draft may renumber them again — this constant and `CS_PQ` in
/// `pollis-core` must move together, because the client tags its published pool
/// with exactly what the DS matches on. Pools published under the old point are
/// simply never claimed and age out.
pub const CIPHERSUITE_PQ: i64 = 0x0052;

/// One published key package: its hex hash-ref and the TLS-serialized
/// `KeyPackage` bytes, base64 (STANDARD) since they are binary.
#[derive(Serialize, Deserialize)]
pub struct KeyPackageEntry {
    pub ref_hash: String,
    /// base64 (STANDARD) of the TLS-serialized `KeyPackage`.
    pub key_package: String,
    /// MLS ciphersuite code point this package was built with. Optional: absent
    /// means [`CIPHERSUITE_PQ`], the only suite in use.
    ///
    /// Client-supplied, so this is a ROUTING HINT and nothing more — the real
    /// suite lives inside the opaque `key_package` blob and MLS rejects a
    /// mismatch at validation. Mistagging can at worst cost a failed add; it
    /// cannot bypass anything.
    #[serde(default)]
    pub ciphersuite: Option<i64>,
}

/// One re-signed cross-signing cert for a device of the actor's account.
#[derive(Serialize, Deserialize)]
pub struct ResignedCert {
    pub device_id: String,
    /// base64 (STANDARD) of the signed cert bytes.
    pub device_cert: String,
    /// Unix seconds, decimal ASCII (stored as TEXT for lossless round-trip).
    pub cert_issued_at: String,
    pub cert_identity_version: i64,
}

impl KeyPackageEntry {
    /// The suite to persist for this package, defaulting to the current one.
    pub fn suite(&self) -> i64 {
        self.ciphersuite.unwrap_or(CIPHERSUITE_PQ)
    }
}
