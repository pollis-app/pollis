//! The Vault (#107): personal encrypted storage, synced across a user's
//! devices as ciphertext the server cannot read.
//!
//! Wire types only; blobs are AES-256-GCM under an account-key-derived key, so the DS decrypts nothing.

use serde::{Deserialize, Serialize};

/// Create one vault entry, or rewrite it whole (edit, pin, unpin — every
/// mutation is a re-encrypt-and-save, so the server cannot tell them apart).
///
/// Upsert by `id`, bound to the authenticated user; last-write-wins per entry.
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
    /// Attachment content hashes; replaces the entry's reference set on each save (keys stay in `blob`).
    #[serde(default)]
    pub attachment_hashes: Vec<String>,
}

/// Hard-delete one vault entry. No tombstone: nobody else holds the entry, so
/// there is nothing to converge with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteVaultMessageBody {
    pub id: String,
    /// When signed it must equal the authenticated user.
    pub user_id: String,
}

/// This account's vault entries, newest first; bound to the authenticated user.
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
