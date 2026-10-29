//! Wire DTOs for the serve layer. [`Bundle`] mirrors the frozen monitor-bundle
//! contract (`verifiable-log/README.md`) as a pure serde shape; proof logic stays
//! in `verifiable_log`.

use serde::{Deserialize, Serialize};
use verifiable_log::{ConsistencyProof, Entry, InclusionProof, Sth, VerifyingKey};

/// The served-bundle wire format version, checked before anything else so skew
/// is reported as skew, never as tampering ([`crate::remote::gate_format_version`]).
/// Bump only on a breaking change to the served shape.
///
/// | version | shape |
/// |---------|-------|
/// | 0 (no field) | pre-#701: manifest lists `conversations`, precomputed `/verify/group/<id>`, raw ids in the commit leaf |
/// | 2 | #701: no `conversations`, no precomputed group reports, windowed pseudonyms in the leaf (1 skipped to match `sth:v2`) |
///
/// A verifier accepts any version ≤ this and rejects a greater one as
/// [`crate::error::ServeError::VersionSkew`].
pub const FORMAT_VERSION: u32 = 2;

/// The oldest served `format_version` whose commit-log leaves this build can
/// interpret. Applied only on paths that decode leaves: re-deriving pseudonyms
/// against pre-#701 leaves would report a confident empty `PASS`. `remote` skips
/// it, since signature/proof checks never decode a leaf.
///
/// Raise this whenever the leaf encoding changes incompatibly again.
pub const MIN_LEAF_FORMAT_VERSION: u32 = 2;

/// The signed monitor bundle (frozen wire contract); every section except
/// `public_key` is optional.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// ML-DSA-44 log public key, lowercase hex (1312 bytes).
    pub public_key: String,
    /// Keys that no longer sign but stay accepted until their `not_after` (the
    /// rotation overlap window). Empty outside a rotation.
    #[serde(default)]
    pub retired_keys: Vec<PublicKeyEntry>,
    /// Signed Tree Heads, oldest first.
    #[serde(default)]
    pub sths: Vec<Sth>,
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Tenants the uniqueness invariant is enforced for on replay.
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

/// A key the log may sign under, or a retired one still inside its overlap window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKeyEntry {
    /// [`verifiable_log::key_id_for`] of `public_key`; a verifier recomputes it.
    pub key_id: String,
    /// Signature algorithm, e.g. `"ML-DSA-44"`.
    pub algorithm: String,
    pub public_key: String,
    /// Epoch ms after which this key is no longer accepted; `None` = the active
    /// key. Past it, heads under the key are unverified rather than trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<u64>,
}

/// The `/v1/public_key.json` document. Carries a key list so rotation has an
/// overlap window (`docs/sth-signing-key-custody.md` §5).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyDoc {
    /// The active signing key, kept for verifiers built before `keys` existed.
    /// Must always equal the one entry in `keys` with no `not_after`.
    pub public_key: String,
    /// Every key currently acceptable, active first. Empty on older documents,
    /// so consumers fall back to `public_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<PublicKeyEntry>,
}

/// Abbreviate a long opaque id/hash for human-readable output.
pub(crate) fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

/// Wall-clock ms for rotation-overlap expiry. A verifier may use a clock (the
/// builder may not); a wrong clock degrades to "unverified", never to a
/// tampering alarm.
pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Bundle {
    /// The `public_key.json` this bundle publishes: the active key first, then
    /// any keys still inside their overlap window.
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

    /// Every key a head in this bundle may verify under at `now_ms`, as
    /// `(key_id, key)` pairs ready for [`Sth::verify_any_with_context`].
    ///
    /// The single place a verify path turns a bundle into keys, so the overlap
    /// window applies identically everywhere; `public_key` alone would make an
    /// honest log mid-rotation look tampered with (#875).
    pub fn key_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        self.public_key_doc().verifying_candidates(now_ms)
    }
}

impl PublicKeyDoc {
    /// Every acceptable key at `now_ms`: unexpired `keys`, or `public_key` alone
    /// when `keys` is absent. Expiry is enforced here so a retired key stops being
    /// accepted on schedule even if a client never updates.
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

    /// [`Self::active_keys`] as `(key_id, key)` pairs. Undecodable entries are
    /// dropped so one malformed entry can't disarm the valid keys.
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

    /// Every published key other than the active one, for
    /// [`Bundle::retired_keys`]. Expiry is deliberately not applied here; it
    /// belongs at verification time, against the verifier's clock.
    pub fn overlap_keys(&self) -> Vec<PublicKeyEntry> {
        self.keys
            .iter()
            .filter(|k| !k.public_key.eq_ignore_ascii_case(&self.public_key))
            .cloned()
            .collect()
    }
}

/// A `(tree_size, leaf_index)` reference to an inclusion-proof artifact, used in
/// the manifest so a monitor can discover and fetch proofs without guessing.
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
    /// See [`FORMAT_VERSION`]; absent (pre-#701) reads as 0.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex.
    pub public_key: String,
    pub entry_count: u64,
    /// The size `latest.json` points at; `null` for an empty log.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/sth/<size>.json` artifact, ascending.
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose uniqueness invariant a verifier enforces on replay.
    pub enforce_unique: Vec<String>,
}

/// The [`Manifest`] of the separate account-key tree, at
/// `/v1/account-keys/index.json`, enumerating `users` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountManifest {
    /// See [`FORMAT_VERSION`]; every served `index.json` is version-gated.
    #[serde(default)]
    pub format_version: u32,
    pub version: String,
    /// Same key as the commit log; STHs are domain-separated by signing context.
    pub public_key: String,
    pub entry_count: u64,
    pub latest_tree_size: Option<u64>,
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    pub enforce_unique: Vec<String>,
    /// Every user id with a precomputed `/verify/account/<user_id>` report, sorted.
    #[serde(default)]
    pub users: Vec<String>,
}

/// The [`Manifest`] of the separate binary-transparency tree, at
/// `/v1/binaries/index.json`, enumerating release `tags` instead.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryManifest {
    #[serde(default)]
    pub format_version: u32,
    pub version: String,
    /// Same key as the other trees; STHs are domain-separated by signing context.
    pub public_key: String,
    pub entry_count: u64,
    pub latest_tree_size: Option<u64>,
    pub sth_sizes: Vec<u64>,
    pub inclusion: Vec<InclusionRef>,
    pub consistency: Vec<ConsistencyRef>,
    pub enforce_unique: Vec<String>,
    /// Every release tag with a precomputed `/verify/release/<tag>` report, sorted.
    #[serde(default)]
    pub tags: Vec<String>,
}
