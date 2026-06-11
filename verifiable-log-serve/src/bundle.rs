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
use verifiable_log::{ConsistencyProof, Entry, InclusionProof, Sth};

/// The signed monitor bundle (input to the layout generator). Field names and
/// shapes match the frozen wire contract; every section except `public_key` is
/// optional so a minimal fixture still deserializes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// Ed25519 log public key, lowercase hex (32 bytes).
    pub public_key: String,
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

/// The standalone `/v1/public_key.json` document. A one-field object (rather
/// than a bare string) so it round-trips through serde and can grow metadata
/// (key id, algorithm) without breaking the URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublicKeyDoc {
    /// Ed25519 log public key, lowercase hex (32 bytes).
    pub public_key: String,
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
    /// API version segment these artifacts live under (`"v1"`).
    pub version: String,
    /// Ed25519 log public key, lowercase hex.
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
    /// Every conversation id with a precomputed `/verify/group/<id>` report,
    /// sorted. Lets a client enumerate the per-conversation endpoints (and learn
    /// how many conversations the log carries) without scraping every entry.
    /// `#[serde(default)]` so an older `index.json` written before this field
    /// existed still deserializes.
    #[serde(default)]
    pub conversations: Vec<String>,
}
