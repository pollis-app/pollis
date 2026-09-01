//! Wire DTOs for the inputs and outputs of the serve layer.
//!
//! [`Bundle`] mirrors the frozen monitor-bundle contract that
//! `verifiable-log-builder` emits and `verifiable-log`'s `monitor` consumes
//! (see `verifiable-log/README.md`). It is a pure serde shape — no Merkle or
//! proof logic lives here; that all stays in `verifiable_log`. We deserialize
//! the builder's JSON directly rather than taking a dependency on the builder
//! crate (which pulls in libSQL/tokio), keeping the serve layer light and the
//! core dependency-pure.
//!
//! [`PublicKeyDoc`] and [`Manifest`] are the two non-artifact-shaped documents
//! the serve layer itself defines: the standalone public-key file and the
//! discovery manifest (`/v1/index.json`).

use serde::{Deserialize, Serialize};
use verifiable_log::{ConsistencyProof, Entry, InclusionProof, Sth, VerifyingKey};

/// The served-bundle **wire format version**: the shape of the discovery
/// manifests and the artifact set a verifier fetches. It is the number a verifier
/// checks *before* it trusts anything else, so version skew is reported as version
/// skew and never mistaken for tampering (see [`crate::remote::gate_format_version`]
/// and `pollis-verify`).
///
/// Bumped on any **breaking** change to the served shape — a removed/renamed
/// manifest field, a deleted class of artifact, or a change to the leaf encoding
/// that a prior verifier cannot parse. It is *not* bumped for additive changes an
/// older verifier tolerates.
///
/// | version | change |
/// |---------|--------|
/// | 0 (implicit) | the pre-#701 shape: `index.json` carried a `conversations` list, the commit-log tree published precomputed `/verify/group/<id>` reports, and the commit leaf published a raw `conversation_id` / `sender_id`. A manifest with **no** `format_version` field is treated as version 0. |
/// | 2 | **#701**: `conversations` removed from the manifest, the precomputed `/verify/group/<id>` artifacts deleted, and the commit leaf moved to windowed pseudonyms. Lands at the #672 / #699 republish (the leaf is a frozen contract — see `verifiable_log_builder::CommitLeaf`). |
///
/// (Version 1 is skipped so the number lines up with the `sth:v2` STH contexts the
/// same republish introduces — one mental model, "everything went to v2 at the
/// republish", not two.)
///
/// A verifier accepts any served version **≤** this constant (older or equal — it
/// understands the shape) and rejects any version **greater** than it as
/// [`crate::error::ServeError::VersionSkew`] ("this verifier is too old for this
/// log — upgrade"). That is the mechanism that stops the *next* breaking wire
/// change from surfacing as a serde/verification error to an auditor who is merely
/// running a stale binary.
pub const FORMAT_VERSION: u32 = 2;

/// The oldest served `format_version` whose **commit-log leaves** this build can
/// still interpret.
///
/// Distinct from [`FORMAT_VERSION`] because the two gates protect different
/// things, in opposite directions:
///
/// * The [`FORMAT_VERSION`] *ceiling* protects a stale verifier from a newer log:
///   it can still check signatures and proofs, so accepting older logs is right.
/// * This *floor* protects a current verifier from an older log **on the paths
///   that decode leaf contents**. #701 replaced the leaf's raw `conversation_id`
///   and `sender_id` with windowed pseudonyms, so a v2 verifier re-deriving
///   pseudonyms against v1 leaves matches nothing and reports `Found: no` for a
///   conversation that is demonstrably present — an empty result carrying a
///   confident `PASS`, which is precisely the failure the version gate exists to
///   prevent, just in the direction the ceiling does not cover.
///
/// Signature, inclusion and consistency checking is format-agnostic (the
/// commit tenant replays under `UniqueDataInvariant`, which compares bytes and
/// never decodes a leaf), so `remote` deliberately does **not** apply this floor
/// and stays backward-compatible with a log that has not been republished.
///
/// Raise this whenever the leaf encoding changes incompatibly again.
pub const MIN_LEAF_FORMAT_VERSION: u32 = 2;

/// The signed monitor bundle (input to the layout generator). Field names and
/// shapes match the frozen wire contract; every section except `public_key` is
/// optional so a minimal fixture still deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// ML-DSA-44 log public key, lowercase hex (1312 bytes).
    pub public_key: String,
    /// Keys that no longer sign but must still be accepted until their
    /// `not_after` — the rotation overlap window. Empty outside a rotation.
    /// Published alongside the active key in `public_key.json`.
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
    /// [`verifiable_log::key_id_for`] of `public_key`. Derived, so a verifier can
    /// recompute it rather than trusting the label.
    pub key_id: String,
    /// Signature algorithm, e.g. `"ML-DSA-44"`. Present so a future algorithm
    /// migration is a data change rather than another flag day.
    pub algorithm: String,
    /// The public key, lowercase hex.
    pub public_key: String,
    /// Milliseconds since epoch after which this key must no longer be accepted.
    /// `None` = the active signing key, no expiry. A retiring key carries the end
    /// of its overlap window; past it, a client treats heads under it as
    /// unverified rather than trusted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<u64>,
}

/// The standalone `/v1/public_key.json` document.
///
/// Carries a key **list** so a rotation is not a flag day: during a changeover
/// the log serves both the new key and the retiring one (with its `not_after`),
/// a client pinning either can still verify, and the overlap is what lets a
/// release reach users before the old key stops being accepted. Design:
/// `docs/sth-signing-key-custody.md` §5.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyDoc {
    /// The active signing key, lowercase hex — the same value that has always
    /// been served here.
    ///
    /// Retained for compatibility with verifiers built before `keys` existed
    /// (including every shipped desktop build). Those read this field and see a
    /// single key, exactly as before. Drop it only once no such verifier is in
    /// the field; until then it must always equal the one entry in `keys` with no
    /// `not_after`.
    pub public_key: String,
    /// Every key currently acceptable, active first. Empty on documents written
    /// before the key set existed, which is why consumers must fall back to
    /// `public_key`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keys: Vec<PublicKeyEntry>,
}

/// Wall-clock milliseconds, for rotation-overlap expiry.
///
/// A *verifier* is allowed a clock (unlike the builder, whose output must be
/// deterministic). A clock wrong in the past can only keep a retired key trusted
/// slightly too long; one wrong in the future retires it early and degrades to
/// "unverified", never to a false alarm about tampering.
/// Abbreviate a long opaque id/hash for human-readable output. Shared by the
/// group, account and release reports, which all print ids beside a verdict.
pub(crate) fn short(s: &str) -> String {
    if s.len() <= 12 {
        s.to_string()
    } else {
        format!("{}\u{2026}{}", &s[..6], &s[s.len() - 4..])
    }
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl Bundle {
    /// The `public_key.json` this bundle publishes: the active signing key first,
    /// then any keys still inside their overlap window.
    ///
    /// `public_key` stays populated with the active key so verifiers built before
    /// the key set keep working unchanged.
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
    /// This is the single place a verify path turns a bundle into verifying keys,
    /// so the rotation-overlap window is applied identically on every path. Using
    /// `public_key` alone instead — which the group, account and release cores did
    /// before #875 — makes an honest log mid-rotation look tampered with.
    pub fn key_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        self.public_key_doc().verifying_candidates(now_ms)
    }
}

impl PublicKeyDoc {
    /// Every acceptable key at `now_ms`, newest-usable first: the entries in
    /// `keys` that have not expired, or `public_key` alone when `keys` is absent
    /// (a log published before the key set existed).
    ///
    /// Expiry is enforced here so a retired key stops being accepted on schedule
    /// even if a client never updates — the overlap window is a deadline, not a
    /// suggestion.
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
    /// decode are dropped rather than failing the whole set — one malformed
    /// published entry must not disarm the keys that are fine.
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
    /// A served document lists the active key inside `keys` as well as in
    /// `public_key`; a bundle splits the two, so the active entry is filtered out
    /// here to keep [`Bundle::public_key_doc`] a faithful round trip. Expiry is
    /// deliberately NOT applied — that belongs at verification time, against the
    /// clock of whoever is verifying, not to whoever parsed the document.
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

/// The discovery manifest served at `/v1/index.json`. It lists every artifact a
/// client can fetch — STH sizes, the entry count, and the available proofs — so
/// a monitor or explorer can walk the whole API from this one document.
///
/// Unlike the immutable per-size artifacts, the manifest *moves* as the log
/// grows, so it is served short-cache (see the README cache policy).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. A verifier reads
    /// this *first* and refuses a log newer than it understands with a clear
    /// "upgrade your verifier" rather than a parse/verification failure.
    /// `#[serde(default)]` (→ 0) so a pre-#701 manifest with no such field still
    /// deserializes and is treated as the legacy shape.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex.
    pub public_key: String,
    /// Number of entries in the log.
    pub entry_count: u64,
    /// Largest tree size with a published STH (the one `latest.json` points at),
    /// or `null` for an empty log with no STHs.
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
/// `/v1/account-keys/index.json`. It is the account tenant's analogue of
/// [`Manifest`]: same artifact bookkeeping (STH sizes, entry count, proofs), but
/// it enumerates `users` (each with a precomputed `/verify/account/<user_id>`
/// report) instead of conversations. The account tree is a fully separate
/// Merkle tree from the commit log, so it carries its own manifest under its own
/// path — `/v1/index.json` is never touched.
///
/// Like [`Manifest`] it *moves* as the log grows, so it is served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountManifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. The account
    /// tree's shape did not change in #701, but it carries the same version marker
    /// as the commit-log manifest so every served `index.json` is uniformly
    /// version-gated and a future break to *this* tree is caught the same way.
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex. The same key signs both trees;
    /// the account tree's STHs are domain-separated by signing context, not key.
    pub public_key: String,
    /// Number of account-key entries in the tree.
    pub entry_count: u64,
    /// Largest tree size with a published account STH, or `null` for an empty
    /// tree.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/account-keys/sth/<size>.json` artifact.
    pub sth_sizes: Vec<u64>,
    /// Every available inclusion proof.
    pub inclusion: Vec<InclusionRef>,
    /// Every available consistency proof.
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose invariant a verifier enforces on replay (the account-key
    /// tenant).
    pub enforce_unique: Vec<String>,
    /// Every user id with a precomputed `/verify/account/<user_id>` report,
    /// sorted. Lets a client enumerate the per-user endpoints without scraping
    /// every entry.
    #[serde(default)]
    pub users: Vec<String>,
}

/// The discovery manifest for the **binaries** tree (binary transparency),
/// served at `/v1/binaries/index.json`. It is the binaries tenant's analogue of
/// [`Manifest`]/[`AccountManifest`]: same artifact bookkeeping (STH sizes, entry
/// count, proofs), but it enumerates release `tags` (each with a precomputed
/// `/verify/release/<tag>` report) instead of conversations or users. The
/// binaries tree is a fully separate Merkle tree, so it carries its own manifest
/// under its own path — `/v1/index.json` is never touched.
///
/// Like the others it *moves* as the log grows, so it is served short-cache.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BinaryManifest {
    /// Served-bundle wire format version — see [`FORMAT_VERSION`]. Same marker as
    /// the other two manifests so every served `index.json` is uniformly
    /// version-gated (the binaries tree's shape did not change in #701).
    #[serde(default)]
    pub format_version: u32,
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// ML-DSA-44 log public key, lowercase hex. The same key signs all three
    /// trees; the binaries tree's STHs are domain-separated by signing context.
    pub public_key: String,
    /// Number of binary-artifact entries in the tree.
    pub entry_count: u64,
    /// Largest tree size with a published binaries STH, or `null` for an empty
    /// tree.
    pub latest_tree_size: Option<u64>,
    /// Every tree size with a `/v1/binaries/sth/<size>.json` artifact.
    pub sth_sizes: Vec<u64>,
    /// Every available inclusion proof.
    pub inclusion: Vec<InclusionRef>,
    /// Every available consistency proof.
    pub consistency: Vec<ConsistencyRef>,
    /// Tenants whose invariant a verifier enforces on replay (the binaries
    /// tenant).
    pub enforce_unique: Vec<String>,
    /// Every release tag with a precomputed `/verify/release/<tag>` report,
    /// sorted. Lets a client enumerate the per-release endpoints without scraping
    /// every entry.
    #[serde(default)]
    pub tags: Vec<String>,
}
