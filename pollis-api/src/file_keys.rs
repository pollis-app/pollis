//! The per-conversation attachment key ring (#1382).
//!
//! Each conversation's ring of file keys crosses here only wrapped under an MLS-exporter KEK, like `crate::pins`.

use serde::{Deserialize, Serialize};

/// What a keystate write is doing to the ring. The DS enforces each one's
/// `key_version` rule, which is what makes a key fork unrepresentable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKeystateOp {
    /// A brand-new ring of one key: insert-only, `key_version` must be 1, and
    /// refused (`updated: false`) whenever any row exists.
    Mint,
    /// The same ring wrapped under a newer epoch's KEK: `key_version` must
    /// equal the stored one and `(generation, epoch)` must be strictly greater.
    Rewrap,
    /// The ring with exactly one new key appended (a member was removed):
    /// `key_version` must be the stored one plus one and `(generation, epoch)`
    /// must not be older than the stored pair.
    Rotate,
}

/// Write a conversation's wrapped file key ring.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertFileKeystateBody {
    /// The MLS conversation — a group id or DM channel id.
    pub conversation_id: String,
    /// base64 — AES-256-GCM ciphertext of the ring (`key_version` entries of a
    /// 32-byte key plus the big-endian `(generation, epoch)` it was added at)
    /// under the epoch's exporter-derived KEK.
    pub wrapped_keyring: String,
    /// base64 — the 12-byte AES-256-GCM nonce of the wrap.
    pub nonce: String,
    /// The ring's length after this write.
    pub key_version: i64,
    /// The MLS suite generation this wrap was produced on.
    pub generation: i64,
    /// The MLS epoch whose exporter produced this wrap.
    pub epoch: i64,
    /// The wrapping device, recorded for operators.
    pub device_id: String,
    /// When signed it must equal the authenticated user.
    pub actor_id: Option<String>,
    pub op: FileKeystateOp,
}

/// Whether the write landed. `updated: false` is the race answer: another
/// device's mint, re-wrap or rotation is already stored, and the caller
/// refetches and adopts it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum FileKeystateUpserted {
    Ok { updated: bool },
}

/// Fetch a conversation's current wrapped ring. Membership-gated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileKeystateBody {
    pub conversation_id: String,
    /// No-auth fallback for the reading user; when signed it must equal the
    /// authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileKeystateResponse {
    /// `None` → no ring minted yet for this conversation.
    #[serde(default)]
    pub keystate: Option<StoredFileKeystate>,
}

/// The stored wrap, exactly as the DS holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredFileKeystate {
    /// base64 — AES-256-GCM ciphertext of the ring.
    pub wrapped_keyring: String,
    /// base64 — the 12-byte nonce.
    pub nonce: String,
    pub key_version: i64,
    pub generation: i64,
    pub epoch: i64,
}
