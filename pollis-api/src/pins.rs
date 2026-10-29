//! Pinned messages (#99): the wrapped pin-key state and the pin rows.
//!
//! Each MLS conversation has one random 32-byte pin key (`Kpin`); pin content is
//! AES-256-GCM under it. `Kpin` crosses the boundary only wrapped under a KEK
//! derived from the epoch's exporter secret, so the DS holds the key to neither
//! ciphertext (client half: `pollis_core::commands::pinned_messages`).

use serde::{Deserialize, Serialize};

/// Upsert a conversation's wrapped `Kpin`, compare-and-set on
/// `(generation, epoch)`.
///
/// A re-wrap lands only if the pair is lexicographically strictly greater than
/// the stored one (as `mls_group_info`), so a stale wrap cannot roll back to an
/// epoch a removed member can derive, and a successor lineage restarting at
/// epoch 0 is not stranded. An equal pair answers `updated: false`, so of two
/// concurrent mints one wins and the other adopts its key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertPinKeystateBody {
    /// The MLS conversation (group id or DM channel id), as `mls_group_info`.
    pub conversation_id: String,
    /// base64 AES-256-GCM ciphertext of `Kpin` under the epoch's KEK.
    pub wrapped_kpin: String,
    /// base64 12-byte nonce of the wrap.
    pub nonce: String,
    /// The MLS suite generation this wrap was produced on.
    pub generation: i64,
    /// The MLS epoch whose exporter produced this wrap.
    pub epoch: i64,
    /// The wrapping device, for operators; authz uses the signed headers.
    pub device_id: String,
    /// When signed it must equal the authenticated user (explicit for the
    /// no-auth dev harness).
    pub actor_id: Option<String>,
    /// `true` → a mint of a new `Kpin`: insert-only, refused whenever any row
    /// exists, so a key fork is unrepresentable. `false` → a re-wrap under the
    /// monotone CAS.
    pub mint: bool,
}

/// Whether the CAS landed. `updated: false` is not an error: a newer or equal
/// wrap is already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PinKeystateUpserted {
    Ok { updated: bool },
}

/// Pin one message: store its encrypted snapshot. Idempotent per
/// `(conversation_id, message_id)`; the DS validates only the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinMessageBody {
    /// Pin ULID, minted by the client.
    pub id: String,
    /// The channel or DM the message lives in (pins list per channel).
    pub conversation_id: String,
    /// The pinned message's id. No FK: a pin outlives envelope GC.
    pub message_id: String,
    /// When signed it must equal the authenticated user.
    pub pinned_by: Option<String>,
    /// base64 AES-256-GCM ciphertext of the padded snapshot JSON under `Kpin`.
    pub encrypted_content: String,
    /// base64 12-byte nonce, unique per pin.
    pub nonce: String,
}

/// Remove a pin. Any current member may unpin (as in Slack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnpinMessageBody {
    pub conversation_id: String,
    pub message_id: String,
    /// When signed it must equal the authenticated user.
    pub actor_id: Option<String>,
}

/// Fetch a conversation's wrapped `Kpin`. Membership-gated: the blob is useless
/// without the exporter, but would still leak pin activity and epoch movement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinKeystateBody {
    /// The MLS conversation id (group or DM).
    pub conversation_id: String,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinKeystateResponse {
    /// `None` → nobody has minted a key yet (a pre-#99 conversation); the first
    /// pinner or committer heals it.
    #[serde(default)]
    pub keystate: Option<StoredPinKeystate>,
}

/// The stored wrap, exactly as the DS holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPinKeystate {
    /// base64 AES-256-GCM ciphertext of `Kpin`.
    pub wrapped_kpin: String,
    /// base64 12-byte nonce.
    pub nonce: String,
    /// The MLS suite generation whose exporter unwraps it.
    pub generation: i64,
    /// The MLS epoch whose exporter unwraps it.
    pub epoch: i64,
}

/// List one conversation's pins, newest first. Membership-gated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListPinsBody {
    /// The channel or DM id the pins were recorded against.
    pub conversation_id: String,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListPinsResponse {
    pub pins: Vec<StoredPin>,
}

/// One pin row, decrypted client-side under `Kpin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPin {
    pub id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub pinned_by: String,
    /// base64 AES-256-GCM ciphertext of the snapshot.
    pub encrypted_content: String,
    /// base64 12-byte nonce.
    pub nonce: String,
    pub pinned_at: String,
}
