//! Pinned messages (#99): the wrapped pin-key state and the pin rows.
//!
//! Pin content is AES-256-GCM under a per-conversation `Kpin`, which crosses only wrapped under an MLS-exporter KEK.

use serde::{Deserialize, Serialize};

/// Upsert a conversation's wrapped `Kpin` — compare-and-set on `epoch`.
///
/// Updates only when `(generation, epoch)` is lexicographically greater; an equal pair loses, so concurrent mints converge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpsertPinKeystateBody {
    /// The MLS conversation — a group id or DM channel id, exactly the key
    /// `mls_group_info` uses.
    pub conversation_id: String,
    /// base64 — AES-256-GCM ciphertext of the 32-byte `Kpin` under the
    /// epoch's exporter-derived KEK.
    pub wrapped_kpin: String,
    /// base64 — the 12-byte AES-256-GCM nonce of the wrap.
    pub nonce: String,
    /// The MLS suite generation this wrap was produced on.
    pub generation: i64,
    /// The MLS epoch whose exporter produced this wrap. CAS pair with
    /// `generation`.
    pub epoch: i64,
    /// The wrapping device, recorded for operators. The DS trusts the signed
    /// headers for authz, not this field.
    pub device_id: String,
    /// When signed it must equal the authenticated user; carried explicitly
    /// for deployments running with auth disabled (dev harness).
    pub actor_id: Option<String>,
    /// `true` → MINT: insert-only, refused if any row exists, so a key fork is unrepresentable. `false` → RE-WRAP.
    pub mint: bool,
}

/// Whether the CAS landed; `updated: false` means a newer wrap is already stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PinKeystateUpserted {
    Ok { updated: bool },
}

/// Pin one message: store its encrypted content snapshot.
///
/// Idempotent per `(conversation_id, message_id)`; the DS validates only the envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinMessageBody {
    /// Pin ULID, minted by the pinning client.
    pub id: String,
    /// The channel or DM the message lives in (pins list per channel).
    pub conversation_id: String,
    /// The pinned message's id. No FK: a pin snapshot deliberately outlives
    /// envelope GC.
    pub message_id: String,
    /// When signed it must equal the authenticated user.
    pub pinned_by: Option<String>,
    /// base64 — AES-256-GCM ciphertext of the padded pin snapshot JSON under
    /// the conversation's `Kpin`.
    pub encrypted_content: String,
    /// base64 — the 12-byte AES-256-GCM nonce, unique per pin.
    pub nonce: String,
}

/// Remove a pin. Any current member may unpin (Slack's model — a pin is
/// conversation furniture, not the pinner's property).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnpinMessageBody {
    pub conversation_id: String,
    pub message_id: String,
    /// When signed it must equal the authenticated user.
    pub actor_id: Option<String>,
}

/// Fetch a conversation's wrapped `Kpin`; membership-gated, since even the blob leaks pin activity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinKeystateBody {
    /// The MLS conversation id (group or DM).
    pub conversation_id: String,
    /// No-auth fallback for the reading user; when signed it must equal the
    /// authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PinKeystateResponse {
    /// `None` → no keystate yet (a pre-#99 conversation); the first pinner heals it.
    #[serde(default)]
    pub keystate: Option<StoredPinKeystate>,
}

/// The stored wrap, exactly as the DS holds it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPinKeystate {
    /// base64 — AES-256-GCM ciphertext of `Kpin`.
    pub wrapped_kpin: String,
    /// base64 — the 12-byte nonce.
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
    /// No-auth fallback for the reading user; when signed it must equal the
    /// authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListPinsResponse {
    pub pins: Vec<StoredPin>,
}

/// One pin row, ciphertext and all — decrypted client-side under `Kpin`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredPin {
    pub id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub pinned_by: String,
    /// base64 — AES-256-GCM ciphertext of the pin snapshot.
    pub encrypted_content: String,
    /// base64 — the 12-byte nonce.
    pub nonce: String,
    pub pinned_at: String,
}
