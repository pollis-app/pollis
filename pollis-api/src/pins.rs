//! Pinned messages (#99): the wrapped pin-key state and the pin rows.
//!
//! Two tables, one key model. Each MLS conversation holds one random 32-byte
//! pin master key (`Kpin`); pin content is AES-256-GCM ciphertext directly
//! under `Kpin` and is never re-encrypted. `Kpin` itself crosses this boundary
//! only WRAPPED — AES-256-GCM under a KEK the members derive from the current
//! MLS epoch's exporter secret — so the DS stores two ciphertexts and holds the
//! key to neither. See `pollis_core::commands::pinned_messages` for the client
//! half and the wrap construction.

use serde::{Deserialize, Serialize};

// ── POST /v1/pins/keystate ───────────────────────────────────────────────────

/// Upsert a conversation's wrapped `Kpin` — compare-and-set on `epoch`.
///
/// Posted at group creation (the initial wrap at epoch 0), at first pin for a
/// conversation older than this feature (the mint-on-demand path), and by any
/// `Kpin`-holding device after an epoch advance (the re-wrap). The DS inserts
/// when no row exists and otherwise updates only when `(generation, epoch)`
/// is LEXICOGRAPHICALLY strictly greater than the stored pair — the same
/// monotone CAS `mls_group_info` uses, so a stale committer's wrap cannot roll
/// the row back to an epoch a removed member can still derive the KEK for,
/// and a suite migration's successor lineage (which restarts at epoch 0) is
/// not stranded by an epoch-only guard. An equal pair is refused with
/// `updated: false`, which is what makes two devices concurrently MINTING
/// different `Kpin`s safe: exactly one insert wins, the loser sees
/// `updated: false`, refetches, and adopts the winner's key before anything
/// is encrypted under its own.
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
    /// `true` → this is a MINT of a brand-new `Kpin`: insert-only, refused
    /// (`updated: false`) whenever ANY row already exists, regardless of
    /// epoch. `false` → a RE-WRAP of the existing key: the monotone CAS
    /// above. The distinction is what makes a key fork unrepresentable — a
    /// device minting at a higher epoch than a racing peer's landed mint
    /// must lose and adopt, never replace a `Kpin` pins may already be
    /// sealed under.
    pub mint: bool,
}

/// Whether the CAS landed. `updated: false` is not an error — it means a newer
/// (or same-epoch) wrap is already stored, which a racing committer treats as
/// "someone else already did my job".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum PinKeystateUpserted {
    Ok { updated: bool },
}

// ── POST /v1/pins/pin ────────────────────────────────────────────────────────

/// Pin one message: store its encrypted content snapshot.
///
/// Idempotent per `(conversation_id, message_id)` — the second pin of the same
/// message is a no-op, whoever got there first keeps the row. The snapshot is
/// sealed under the conversation's `Kpin` client-side; the DS never sees pin
/// plaintext, and validates only the envelope (base64, nonce length, bounded
/// blob).
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

// ── POST /v1/pins/unpin ──────────────────────────────────────────────────────

/// Remove a pin. Any current member may unpin (Slack's model — a pin is
/// conversation furniture, not the pinner's property).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnpinMessageBody {
    pub conversation_id: String,
    pub message_id: String,
    /// When signed it must equal the authenticated user.
    pub actor_id: Option<String>,
}

// ── POST /v1/read/pin-keystate ───────────────────────────────────────────────

/// Fetch a conversation's current wrapped `Kpin`. Membership-gated: the blob
/// is useless without the exporter, but serving it to strangers would still
/// leak that the conversation pins things and how recently its epoch moved.
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
    /// `None` → no keystate yet: a conversation created before #99 shipped
    /// whose key nobody has minted yet. Not an error — the first pinner (or
    /// the next committer) heals it.
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

// ── POST /v1/read/pins ───────────────────────────────────────────────────────

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
