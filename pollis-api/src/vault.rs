//! The Vault (#107): personal encrypted storage, synced across a user's
//! devices as ciphertext the server cannot read.
//!
//! Wire types only — no handler logic, no DB access. Every blob here is
//! AES-256-GCM under a key derived client-side from the ACCOUNT identity key
//! (HKDF-SHA256, vault-specific info string — see
//! `pollis_core::commands::vault`), so every device of the account decrypts
//! every entry and the DS decrypts none of them. The pinned flag, the entry
//! body, attachment references — all live INSIDE the ciphertext; the server
//! stores an opaque row per entry and learns only count, approximate size and
//! change times.

use serde::{Deserialize, Serialize};

// ── POST /v1/vault/save ──────────────────────────────────────────────────────

/// Create one vault entry, or rewrite it whole (edit, pin, unpin — every
/// mutation is a re-encrypt-and-save, so the server cannot tell them apart).
///
/// Upsert by `id`, bound to the authenticated user: a save whose `id` already
/// exists updates only when the stored row belongs to the same user, so an
/// attacker cannot overwrite a stranger's entry by minting the same ULID.
/// Last-write-wins per entry — safe because ids are device-minted ULIDs (two
/// devices never mint the same one) and an edit rewrites the whole entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveVaultMessageBody {
    /// Entry ULID, minted by the writing client and stable across edits.
    pub id: String,
    /// The owner; when signed it must equal the authenticated user.
    pub user_id: String,
    /// base64 — the 12-byte AES-256-GCM nonce, fresh per save.
    pub nonce: String,
    /// base64 — AES-256-GCM ciphertext + tag over the padded entry JSON.
    pub blob: String,
    /// Client-supplied creation time; kept from the original row on an edit.
    pub created_at: String,
    /// Content hashes of the attachment objects this entry references —
    /// REPLACES the entry's reference set on every save, so an edit that
    /// drops a file releases it. These keep the shared R2 objects alive
    /// against attachment GC (`vault_attachment_ref`); the decryption keys
    /// for the objects live inside `blob`, never here. The user→hash pairing
    /// this reveals is already conceded by the signed upload presign.
    #[serde(default)]
    pub attachment_hashes: Vec<String>,
}

// ── POST /v1/vault/delete ────────────────────────────────────────────────────

/// Hard-delete one vault entry. No tombstone: nobody else holds the entry, so
/// there is nothing to converge with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteVaultMessageBody {
    pub id: String,
    /// When signed it must equal the authenticated user.
    pub user_id: String,
}

// ── POST /v1/read/vault ──────────────────────────────────────────────────────

/// This account's vault entries, newest first. Device-signed and bound to the
/// authenticated user — the rows are undecryptable without the account key,
/// but serving them to a stranger would still leak how many entries exist and
/// when they change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultMessagesBody {
    /// Must equal the authenticated user; a mismatch is a 403.
    pub user_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultMessagesResponse {
    pub messages: Vec<StoredVaultMessage>,
}

/// One stored entry, exactly as the DS holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredVaultMessage {
    pub id: String,
    /// base64 — the 12-byte AES-256-GCM nonce.
    pub nonce: String,
    /// base64 — AES-256-GCM ciphertext + tag.
    pub blob: String,
    pub created_at: String,
    pub updated_at: String,
}
