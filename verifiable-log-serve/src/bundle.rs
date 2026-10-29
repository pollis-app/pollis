//! Wire DTOs for the inputs and outputs of the serve layer.
//!
//! [`Bundle`] mirrors the frozen monitor-bundle contract that
//! `verifiable-log-builder` emits and `verifiable-log`'s `monitor` consumes
//! (see `verifiable-log/README.md`). Pure serde; deserialized without depending on the builder
//! crate.

use serde::{Deserialize, Serialize};
use verifiable_log::{ConsistencyProof, Entry, InclusionProof, Sth, VerifyingKey};

/// The served-bundle **wire format version**: the shape of the discovery
/// manifests and artifact set, checked first so skew is never mistaken for tampering.
///
/// Bumped only on breaking changes to the served shape, not additive ones.
///
/// | version | change |
/// |---------|--------|
/// | 0 (implicit) | the pre-#701 shape: `index.json` carried a `conversations` list, the commit-log tree published precomputed `/verify/group/<id>` reports, and the commit leaf published a raw `conversation_id` / `sender_id`. A manifest with **no** `format_version` field is treated as version 0. |
/// | 2 | **#701**: `conversations` removed from the manifest, the precomputed `/verify/group/<id>` artifacts deleted, and the commit leaf moved to windowed pseudonyms. Lands at the #672 / #699 republish (the leaf is a frozen contract — see `verifiable_log_builder::CommitLeaf`). |
///
/// Version 1 is skipped to line up with the `sth:v2` contexts. Newer served versions are
/// [`crate::error::ServeError::VersionSkew`].
pub const FORMAT_VERSION: u32 = 2;

/// The oldest served `format_version` whose **commit-log leaves** this build can
/// still interpret.
///
/// A floor for leaf-decoding paths: older raw-id leaves would match nothing and report a false
/// PASS. `remote` compares bytes only, so it skips this floor.
/// Raise this whenever the leaf encoding changes incompatibly again.
pub const MIN_LEAF_FORMAT_VERSION: u32 = 2;

/// The signed monitor bundle (input to the layout generator). Field names and
/// shapes match the frozen wire contract; only `public_key` is required.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// ML-DSA-44 log public key, lowercase hex (1312 bytes).
    pub public_key: String,
    /// Keys that no longer sign but must still be accepted until their
    /// `not_after` — the rotation overlap window. Empty outside a rotation.
    #[serde(default)]
    pub retired_keys: Vec<PublicKeyEntry>,
    /// Signed Tree Heads, oldest first.
    #[serde(default)]
    pub sths: Vec<Sth>,
    /// Full ordered log contents.
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Tenants the uniqueness invariant is enforced for on replay.
    #[serde(default)]
    pub enforce_unique: Vec<String>,
    /// Inclusion proofs (one per entry).
    #[serde(default)]
    pub inclusion: Vec<InclusionCheck>,
    /// Consistency proofs between STHs.
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

/// One entry in [`PublicKeyDoc::keys`] — a key the log may currently be signing
/// under, or one recently retired but still inside its overlap window.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicKeyEntry {
    /// [`verifiable_log::key_id_for`] of `public_key`; recomputable, never trusted.
    pub key_id: String,
    /// Signature algorithm, e.g. `"ML-DSA-44"`.
    pub algorithm: String,
    /// The public key, lowercase hex.
    pub public_key: String,
    /// Milliseconds since epoch after which this key must no longer be accepted.
    /// `None` = the active signing key, no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<u64>,
}

/// The standalone `/v1/public_key.json` document.
///
/// Carries a key **list** so a rotation is not a flag day.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyDoc {
    /// The active signing key, lowercase hex — the same value that has always
    /// been served here, kept for verifiers that predate `keys`; must equal the active entry.
    pub public_key: String,
    /// Every key currently acceptable, active first. Empty on documents written
    /// before the key set existed, so consumers fall back to `public_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<PublicKeyEntry>,
}

/// Abbreviate a long opaque id/hash for human-readable output. Shared by the
/// group, account and release reports, which all print ids beside a verdict.
pub(crate) fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Bundle {
    /// The `public_key.json` this bundle publishes: the active signing key first,
    /// then any keys still inside their overlap window.
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
    /// The single place a bundle becomes verifying keys, so the overlap window applies on every
    /// path (#875).
    pub fn key_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        self.public_key_doc().verifying_candidates(now_ms)
    }
}

impl PublicKeyDoc {
    /// Every acceptable key at `now_ms`, newest-usable first: the entries in
    /// `keys` that have not expired, or `public_key` alone when `keys` is absent
    /// (a log published before the key set existed).
    ///
    /// Expiry is enforced here so a retired key stops being accepted on schedule.
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

    /// [`Self::active_keys`] parsed into verifying keys, as `(key_id, key)` pairs
    /// ready for [`Sth::verify_any_with_context`]. Entries whose hex does not
    /// decode are dropped rather than failing the whole set.
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

    /// The entries a **verification-side** [`Bundle`] carries in
    /// [`Bundle::retired_keys`]: every published key other than the active one.
    ///
    /// The active entry is filtered out to keep [`Bundle::public_key_doc`] a round trip; expiry
    /// is applied at verify time.
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

/// The discovery manifest served at `/v1/index.json`. It lists every artifact a
/// client can fetch, so a monitor can walk the whole API from it. Served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. A verifier reads
    /// this first and refuses a newer log; absent means 0.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex.
    pub public_key: String,
    /// Number of entries in the log.
    pub entry_count: u64,
    /// Largest tree size with a published STH, or `null` for an empty log.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/sth/<size>.json` artifact, ascending.
    pub sth_sizes: Vec<u64>,
    /// Every available inclusion proof.
    pub inclusion: Vec<InclusionRef>,
    /// Every available consistency proof.
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose uniqueness invariant a verifier enforces on replay.
    pub enforce_unique: Vec<String>,
}

/// The discovery manifest for the **account-key** tree, served at
/// `/v1/account-keys/index.json`; like [`Manifest`], plus `users`. Served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountManifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. The account
    /// tree carries the same marker so every served `index.json` is version-gated.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex.
    pub public_key: String,
    /// Number of account-key entries in the tree.
    pub entry_count: u64,
    /// Largest tree size with a published account STH, or `null` for an empty tree.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/account-keys/sth/<size>.json` artifact.
    pub sth_sizes: Vec<u64>,
    /// Every available inclusion proof.
    pub inclusion: Vec<InclusionRef>,
    /// Every available consistency proof.
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose invariant a verifier enforces on replay.
    pub enforce_unique: Vec<String>,
    /// Every user id with a precomputed `/verify/account/<user_id>` report,
    /// sorted, so a client can enumerate the per-user endpoints.
    #[serde(default)]
    pub users: Vec<String>,
}

/// The discovery manifest for the **binaries** tree (binary transparency),
/// served at `/v1/binaries/index.json`; like [`Manifest`], plus release `tags`. Served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryManifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. Same marker as
    /// the other two manifests.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex.
    pub public_key: String,
    /// Number of binary-artifact entries in the tree.
    pub entry_count: u64,
    /// Largest tree size with a published binaries STH, or `null` for an empty tree.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/binaries/sth/<size>.json` artifact.
    pub sth_sizes: Vec<u64>,
    /// Every available inclusion proof.
    pub inclusion: Vec<InclusionRef>,
    /// Every available consistency proof.
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose invariant a verifier enforces on replay.
    pub enforce_unique: Vec<String>,
    /// Every release tag with a precomputed `/verify/release/<tag>` report,
    /// sorted, so a client can enumerate the per-release endpoints.
    #[serde(default)]
    pub tags: Vec<String>,
}
