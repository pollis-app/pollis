//! The **monitor bundle** wire contract: the single JSON document the builder
//! emits, the serve layer publishes, and the `monitor` CLI verifies.
//!
//! This type lives in the core crate rather than beside either of its users
//! because it is the *joint* between them, and a joint restated in two places is
//! a joint that drifts. It already had: the monitor carried a private stand-in
//! `Bundle` with no `retired_keys` field, so serde silently dropped the rotation
//! overlap set and the monitor reported FAIL for every head an honest log minted
//! under a retiring key — a "tampering" verdict produced by a missing struct
//! field. A verifier that can be wrong in *that* direction is worse than no
//! verifier, so the shape a verifier parses is defined exactly once.
//!
//! `verifiable-log-builder` re-exports these types. `verifiable-log-serve` still
//! carries its own parallel copy (`serve::bundle::Bundle`); pointing it here is
//! the remaining step of the same consolidation.

use serde::{Deserialize, Serialize};

use crate::proof::{ConsistencyProof, InclusionProof};
use crate::sth::{key_id_for, verifying_key_from_hex, Sth, VerifyingKey};
use crate::Entry;

/// The signed monitor bundle. Field names and order are the frozen wire
/// contract (see `verifiable-log/README.md`); every section except `public_key`
/// deserializes from absent, so a minimal fixture still parses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bundle {
    /// ML-DSA-44 log public key, lowercase hex (1312 bytes) — the **active**
    /// signing key.
    pub public_key: String,
    /// Keys that no longer sign but must still be accepted until their
    /// `not_after` (ms since epoch) — the rotation overlap window that keeps a
    /// key change from being a flag day. Empty outside a rotation; serialized
    /// only when non-empty so bundles stay byte-identical to before otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired_keys: Vec<PublicKeyEntry>,
    /// Signed Tree Heads, oldest first.
    #[serde(default)]
    pub sths: Vec<Sth>,
    /// Full ordered log contents. When present, replayed to confirm each STH's
    /// root and to run the tenant invariants.
    #[serde(default)]
    pub entries: Vec<Entry>,
    /// Tenants whose invariant a verifier enforces on replay.
    #[serde(default)]
    pub enforce_unique: Vec<String>,
    /// Inclusion proofs to verify.
    #[serde(default)]
    pub inclusion: Vec<InclusionCheck>,
    /// Consistency proofs to verify (indices reference `sths`).
    #[serde(default)]
    pub consistency: Vec<ConsistencyCheck>,
}

/// One key a head may verify under: the active signer, or one inside its
/// rotation-overlap window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKeyEntry {
    /// [`key_id_for`] of `public_key`. A selection hint, never a trust input —
    /// the signature still has to verify under the key bytes.
    pub key_id: String,
    /// Signature algorithm, e.g. `"ML-DSA-44"`. Present so a future algorithm
    /// migration is a data change rather than another flag day.
    pub algorithm: String,
    /// The public key, lowercase hex.
    pub public_key: String,
    /// Milliseconds since epoch after which this key must no longer be
    /// accepted. `None` = the active signing key, no expiry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InclusionCheck {
    pub entry: Entry,
    pub proof: InclusionProof,
    /// Index into [`Bundle::sths`] whose root the proof is checked against.
    pub sth_index: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsistencyCheck {
    pub old_index: usize,
    pub new_index: usize,
    pub proof: ConsistencyProof,
}

impl Bundle {
    /// Every key a head in this bundle may verify under at `now_ms`, as
    /// `(key_id, key)` pairs ready for [`Sth::verify_any_with_context`]: the
    /// active key first, then each retired key still inside its overlap window.
    ///
    /// Expiry is applied here so a retired key stops being accepted on schedule
    /// even against a bundle nobody has republished — the overlap window is a
    /// deadline, not a suggestion. Entries whose hex does not decode are
    /// dropped rather than failing the whole set: one malformed published entry
    /// must not disarm the keys that are fine.
    pub fn key_candidates(&self, now_ms: u64) -> Vec<(String, VerifyingKey)> {
        let mut out = Vec::with_capacity(1 + self.retired_keys.len());
        if let Ok(active) = verifying_key_from_hex(&self.public_key) {
            out.push((key_id_for(&active), active));
        }
        for entry in &self.retired_keys {
            if entry.not_after.is_some_and(|exp| now_ms > exp) {
                continue;
            }
            if let Ok(vk) = verifying_key_from_hex(&entry.public_key) {
                out.push((entry.key_id.clone(), vk));
            }
        }
        out
    }
}
