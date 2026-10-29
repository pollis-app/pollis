//! The Vault (#107): personal encrypted storage synced across a user's devices.
//!
//! Every blob is AES-256-GCM under a key HKDF'd client-side from the account
//! identity key (`pollis_core::commands::vault`): every device decrypts every
//! entry, the DS none. Pinned flag, body and attachment references are all
//! inside the ciphertext; the server learns only count, size and change times.

use serde::{Deserialize, Serialize};

/// Create one vault entry or rewrite it whole (edits and pins are all
/// re-encrypt-and-save, indistinguishable to the server).
///
/// Upsert by `id`, updating only a row owned by the same user, so a minted
/// duplicate ULID cannot overwrite a stranger's entry. Last-write-wins per entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveVaultMessageBody {
    /// Entry ULID, minted by the writing client and stable across edits.
    pub id: String,
    /// The owner; when signed it must equal the authenticated user.
    pub user_id: String,
    /// base64 12-byte AES-256-GCM nonce, fresh per save.
    pub nonce: String,
    /// base64 AES-256-GCM ciphertext + tag over the padded entry JSON.
    pub blob: String,
    /// Client-supplied creation time; kept from the original row on an edit.
    pub created_at: String,
    /// Content hashes of the attachments this entry references, replacing its
    /// reference set on every save (`vault_attachment_ref`, keeps them alive
    /// against GC). Their keys live inside `blob`.
    #[serde(default)]
    pub attachment_hashes: Vec<String>,
}

/// Hard-delete one vault entry; no tombstone, since nobody else holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeleteVaultMessageBody {
    pub id: String,
    /// When signed it must equal the authenticated user.
    pub user_id: String,
}

/// This account's vault entries, newest first, bound to the authenticated user:
/// undecryptable, but serving them would leak count and change times.
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
    /// base64 12-byte AES-256-GCM nonce.
    pub nonce: String,
    /// base64 AES-256-GCM ciphertext + tag.
    pub blob: String,
    pub created_at: String,
    pub updated_at: String,
}
