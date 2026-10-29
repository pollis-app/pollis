//! The append-only log itself: tenant-tagged entries, the pluggable
//! per-tenant invariant hook, and [`VerifiableLog`] which hosts many tenants
//! in a single Merkle tree.

use serde::{Deserialize, Serialize};

use crate::error::{Error, InvariantViolation, Result};
use crate::hash::{leaf_hash, Hash};
use crate::merkle;
use crate::sth::Sth;

/// Lowercase-hex serde for the opaque entry payload.
mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        hex::decode(&s).map_err(serde::de::Error::custom)
    }
}

/// A single log entry. `tenant` is an opaque, caller-chosen partition id;
/// `data` is the tenant-specific payload, opaque to the core.
/// Leaf bytes are `len(tenant) as u32 BE || tenant || data`, so pairs never collide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub tenant: String,
    #[serde(with = "hex_bytes")]
    pub data: Vec<u8>,
}

impl Entry {
    pub fn new(tenant: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            tenant: tenant.into(),
            data: data.into(),
        }
    }

    /// Canonical byte encoding fed to the leaf hash.
    pub fn encode(&self) -> Vec<u8> {
        let tenant = self.tenant.as_bytes();
        let mut out = Vec::with_capacity(4 + tenant.len() + self.data.len());
        out.extend_from_slice(&(tenant.len() as u32).to_be_bytes());
        out.extend_from_slice(tenant);
        out.extend_from_slice(&self.data);
        out
    }

    /// Leaf hash `SHA-256(0x00 || encode())`.
    pub fn leaf_hash(&self) -> Hash {
        leaf_hash(&self.encode())
    }
}

/// Pluggable per-tenant correctness rule. Consulted on every append for a
/// tenant that has one registered: it sees the entries already committed for
/// that tenant (in order) and the candidate, and rejects the append by
/// returning an [`InvariantViolation`].
pub trait TenantInvariant: Send + Sync {
    fn check(&self, existing: &[&Entry], candidate: &Entry) -> std::result::Result<(), InvariantViolation>;
}

/// Example invariant: reject an append whose `data` duplicates an existing entry.
pub struct UniqueDataInvariant;

impl TenantInvariant for UniqueDataInvariant {
    fn check(&self, existing: &[&Entry], candidate: &Entry) -> std::result::Result<(), InvariantViolation> {
        if existing.iter().any(|e| e.data == candidate.data) {
            return Err(InvariantViolation::new(
                candidate.tenant.clone(),
                format!(
                    "duplicate entry for tenant `{}` (payload already committed)",
                    candidate.tenant
                ),
            ));
        }
        Ok(())
    }
}

/// An append-only, multi-tenant verifiable Merkle log.
#[derive(Default)]
pub struct VerifiableLog {
    entries: Vec<Entry>,
    leaves: Vec<Hash>,
    invariants: std::collections::HashMap<String, Box<dyn TenantInvariant>>,
}

impl VerifiableLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register (or replace) the invariant hook for a tenant. Entries for
    /// tenants without a registered hook are accepted unconditionally.
    pub fn register_invariant(&mut self, tenant: impl Into<String>, invariant: Box<dyn TenantInvariant>) {
        self.invariants.insert(tenant.into(), invariant);
    }

    /// Append an entry, returning its leaf index. Runs the tenant's invariant
    /// (if any) first; on violation the log is unchanged.
    pub fn append(&mut self, entry: Entry) -> Result<usize> {
        if let Some(invariant) = self.invariants.get(&entry.tenant) {
            let existing: Vec<&Entry> = self
                .entries
                .iter()
                .filter(|e| e.tenant == entry.tenant)
                .collect();
            invariant.check(&existing, &entry)?;
        }
        let index = self.entries.len();
        self.leaves.push(entry.leaf_hash());
        self.entries.push(entry);
        Ok(index)
    }

    /// Number of leaves currently in the log.
    pub fn size(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Current Merkle root.
    pub fn root(&self) -> Hash {
        merkle::root_hash(&self.leaves)
    }

    /// Merkle root the log had at `size` leaves (`size <= self.size()`).
    pub fn root_at(&self, size: usize) -> Result<Hash> {
        if size > self.leaves.len() {
            return Err(Error::IndexOutOfRange {
                index: size,
                size: self.leaves.len(),
            });
        }
        Ok(merkle::root_hash(&self.leaves[..size]))
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// Produce a Signed Tree Head over the current tree. `timestamp` is
    /// supplied by the caller — the core never reads the clock.
    pub fn signed_tree_head(&self, signing_key: &crate::sth::SigningKey, timestamp: u64) -> Sth {
        Sth::create(signing_key, self.size() as u64, self.root(), timestamp)
    }

    /// Build an inclusion proof for the leaf at `index`.
    pub fn inclusion_proof(&self, index: usize) -> Result<crate::proof::InclusionProof> {
        if index >= self.leaves.len() {
            return Err(Error::IndexOutOfRange {
                index,
                size: self.leaves.len(),
            });
        }
        let path = merkle::inclusion_path(index, &self.leaves);
        Ok(crate::proof::InclusionProof {
            leaf_index: index as u64,
            tree_size: self.leaves.len() as u64,
            audit_path: path.iter().map(crate::hash::to_hex).collect(),
        })
    }

    /// Build a consistency proof between tree sizes `first` and `second`
    /// (`0 < first <= second <= size`).
    pub fn consistency_proof(&self, first: usize, second: usize) -> Result<crate::proof::ConsistencyProof> {
        if first == 0 || first > second || second > self.leaves.len() {
            return Err(Error::InvalidTreeSizes { first, second });
        }
        let path = merkle::consistency_path(first, &self.leaves[..second]);
        Ok(crate::proof::ConsistencyProof {
            first_size: first as u64,
            second_size: second as u64,
            path: path.iter().map(crate::hash::to_hex).collect(),
        })
    }
}
