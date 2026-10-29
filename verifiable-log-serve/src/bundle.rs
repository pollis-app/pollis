//! Wire DTOs for the serve layer. [`Bundle`] mirrors the frozen monitor-bundle
//! contract the builder emits; it is deserialized directly so the serve layer
//! needn't depend on the builder crate (libSQL/tokio).

use serde::{Deserialize, Serialize};
use verifiable_log::{ConsistencyProof, Entry, InclusionProof, Sth, VerifyingKey};

/// Served-bundle wire format version, which a verifier checks before trusting
/// anything so skew is reported as skew, never as tampering. Bumped only for
/// breaking changes; a verifier accepts any version up to this and rejects a
/// newer one as [`crate::error::ServeError::VersionSkew`]. Version history:
/// `docs/transparency.md`.
pub const FORMAT_VERSION: u32 = 2;

/// The oldest served `format_version` whose commit-log leaves this build can
/// interpret. A floor, unlike the [`FORMAT_VERSION`] ceiling: a current verifier
/// reading pre-#701 leaves would match nothing and report a confident empty
/// `PASS`. Only leaf-decoding paths apply it; `remote` checks signatures and
/// proofs, which are format-agnostic, so it stays compatible with older logs.
pub const MIN_LEAF_FORMAT_VERSION: u32 = 2;

/// The signed monitor bundle (input to the layout generator). Every section
/// except `public_key` is optional so a minimal fixture deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// ML-DSA-44 log public key, lowercase hex (1312 bytes).
    pub public_key: String,
    /// Keys that no longer sign but stay accepted until their `not_after` (the
    /// rotation overlap window).
    #[serde(default)]
    pub retired_keys: Vec<PublicKeyEntry>,
    /// Signed Tree Heads, oldest first.
    #[serde(default)]
    pub sths: Vec<Sth>,
    #[serde(default)]
    pub entries: Vec<Entry>,
    #[serde(default)]
    pub enforce_unique: Vec<String>,
    #[serde(default)]
    pub inclusion: Vec<InclusionCheck>,
    #[serde(default)]
    pub consistency: Vec<ConsistencyCheck>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InclusionCheck {
    pub entry: Entry,
    pub proof: InclusionProof,
    /// Index into `sths` whose root the proof is checked against.
    pub sth_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyCheck {
    pub old_index: usize,
    pub new_index: usize,
    pub proof: ConsistencyProof,
}

/// One entry in [`PublicKeyDoc::keys`]: an active key, or a retired one still
/// inside its overlap window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKeyEntry {
    /// [`verifiable_log::key_id_for`] of `public_key`; recomputable, not trusted.
    pub key_id: String,
    pub algorithm: String,
    pub public_key: String,
    /// Milliseconds since epoch after which this key is no longer accepted.
    /// `None` = the active key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<u64>,
}

/// The standalone `/v1/public_key.json` document. Carries a key list so a
/// rotation is not a flag day (`docs/sth-signing-key-custody.md` §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyDoc {
    /// The active signing key, lowercase hex. Kept for verifiers that predate
    /// `keys` (every shipped desktop build); must always equal the one entry in
    /// `keys` with no `not_after`.
    pub public_key: String,
    /// Every key currently acceptable, active first. Empty on pre-key-set
    /// documents, so consumers fall back to `public_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<PublicKeyEntry>,
}

/// Abbreviate a long opaque id/hash for human-readable reports.
pub(crate) fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

/// Wall-clock milliseconds, for rotation-overlap expiry. A wrong clock can only
/// keep a retired key slightly too long or retire it early ("unverified"),
/// never cause a false tampering alarm.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Bundle {
    /// The `public_key.json` this bundle publishes: the active key first, then any
    /// keys still inside their overlap window.
    pub fn public_key_doc(&self) -> PublicKeyDoc {
        let mut keys = Vec::with_capacity(1 + self.retired_keys.len());
        keys.push(PublicKeyEntry {
            key_id: verifiable_log::verifying_key_from_hex(&self.public_key)
                .map(|vk| verifiable_log::key_id_for(&vk))
                .unwrap_or_default(),
            algorithm: "ML-DSA-44".to_string(),
            public_key: self.public_key.clone(),
            not_after: None,
        });
        keys.extend(self.retired_keys.iter().cloned());
        PublicKeyDoc {
            public_key: self.public_key.clone(),
            keys,
        }
    }

    /// Every key a head in this bundle may verify under at `now_ms`. The single
    /// place a verify path turns a bundle into keys, so the overlap window applies
    /// identically everywhere; `public_key` alone makes an honest log mid-rotation
    /// look tampered with (#875).
    pub fn key_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        self.public_key_doc().verifying_candidates(now_ms)
    }
}

impl PublicKeyDoc {
    /// Every acceptable key at `now_ms`: unexpired `keys`, or `public_key` alone
    /// for a pre-key-set document. Expiry is enforced here, so the overlap window
    /// is a deadline even for a client that never updates.
    pub fn active_keys(&self, now_ms: u64) -> Vec<PublicKeyEntry> {
        if self.keys.is_empty() {
            return vec![PublicKeyEntry {
                key_id: String::new(),
                algorithm: "ML-DSA-44".to_string(),
                public_key: self.public_key.clone(),
                not_after: None,
            }];
        }
        self.keys
            .iter()
            .filter(|k| k.not_after.is_none_or(|exp| now_ms <= exp))
            .cloned()
            .collect()
    }

    /// [`Self::active_keys`] as verifying keys. Undecodable entries are dropped so
    /// one malformed entry can't disarm the good keys.
    pub fn verifying_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        self.active_keys(now_ms)
            .into_iter()
            .filter_map(|e| {
                verifiable_log::verifying_key_from_hex(&e.public_key)
                    .ok()
                    .map(|vk| (e.key_id, vk))
            })
            .collect()
    }

    /// Every published key other than the active one, for a verification-side
    /// [`Bundle::retired_keys`]. Expiry is deliberately not applied: that belongs
    /// to the verifier's clock at verification time.
    pub fn overlap_keys(&self) -> Vec<PublicKeyEntry> {
        self.keys
            .iter()
            .filter(|k| !k.public_key.eq_ignore_ascii_case(&self.public_key))
            .cloned()
            .collect()
    }
}

/// A `(tree_size, leaf_index)` reference to an inclusion-proof artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct InclusionRef {
    pub tree_size: u64,
    pub leaf_index: u64,
}

/// A `(first, second)` reference to a consistency-proof artifact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ConsistencyRef {
    pub first: u64,
    pub second: u64,
}

/// The discovery manifest at `/v1/index.json`, listing every fetchable artifact.
/// It moves as the log grows, so it is served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// See [`FORMAT_VERSION`]. Defaults to 0 for a pre-#701 manifest.
    #[serde(default)]
    pub format_version: u32,
    pub version: String,
    pub public_key: String,
    pub entry_count: u64,
    /// Largest tree size with a published STH, `null` for an empty log.
    pub latest_tree_size: Option<u64>,
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    pub enforce_unique: Vec<String>,
}

/// The account-key tree's discovery manifest at `/v1/account-keys/index.json`,
/// listing `users` (each with a precomputed `/verify/account/<user_id>` report).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountManifest {
    /// See [`FORMAT_VERSION`].
    #[serde(default)]
    pub format_version: u32,
    pub version: String,
    /// The same key signs every tree; STHs are domain-separated by context.
    pub public_key: String,
    pub entry_count: u64,
    pub latest_tree_size: Option<u64>,
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    pub enforce_unique: Vec<String>,
    /// Every user id with a precomputed report, sorted.
    #[serde(default)]
    pub users: Vec<String>,
}

/// The binaries tree's discovery manifest at `/v1/binaries/index.json`, listing
/// release `tags` (each with a precomputed `/verify/release/<tag>` report).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryManifest {
    /// See [`FORMAT_VERSION`].
    #[serde(default)]
    pub format_version: u32,
    pub version: String,
    pub public_key: String,
    pub entry_count: u64,
    pub latest_tree_size: Option<u64>,
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    pub enforce_unique: Vec<String>,
    /// Every release tag with a precomputed report, sorted.
    #[serde(default)]
    pub tags: Vec<String>,
}
