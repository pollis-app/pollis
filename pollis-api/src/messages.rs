//! Message envelopes, edits/deletes, reactions, watermarks, envelope GC and
//! attachment reference rows.
//!
//! Wire types only; see `pollis-delivery` for handling.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SendMessageBody {
    pub id: String,
    pub conversation_id: String,
    /// Unsealed: bound to the authenticated user when signed (the no-auth
    /// fallback only). Sealed (issue #331): a non-identifying sentinel the client
    /// chose (e.g. the string `"sealed"`) — persisted as-is, NOT bound to the
    /// auth user (see `apply_send_message`).
    #[serde(default)]
    pub sender_id: Option<String>,
    /// The `"mls:<hex>"` ciphertext string the client persists — plain text, not
    /// binary, so no base64.
    pub ciphertext: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    /// The delivery-cursor stamp; admitted only as canonical UTC RFC 3339 within the signature window (a future value blacks out the conversation).
    pub sent_at: String,
    /// Sealed sender flag (issue #331, `docs/metadata-minimization-design.md`
    /// §2). `1` → `sender_id` is a blinded sentinel; the true sender lives in the
    /// MLS credential. Absent (old clients / unsealed sends) → `0`.
    #[serde(default)]
    pub sealed: i64,
    /// Who to WAKE with a content-free push (#987, #843).
    ///
    /// * `None` → do not push at all (a suppressed send).
    /// * `Some([])` → every member. The DM and `@all` behaviour.
    /// * `Some([ids])` → only those, INTERSECTED with real membership
    ///   server-side, so this can only ever narrow the audience — never widen it
    ///   past the conversation.
    ///
    /// Since #1431 clients send `Some([])` for every group-channel message; muted recipients are dropped unless mentioned.
    #[serde(default)]
    pub push_to: Option<Vec<String>>,
    /// Members named by `@username` (#1431), pushed even if they muted the channel; absent → the non-empty `push_to`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_mentions: Option<Vec<String>>,
    /// The MLS lineage the ciphertext was sealed at (#1041); with `epoch`, the DS refuses a non-head position. Absent → ungated.
    #[serde(default)]
    pub generation: Option<i64>,
    /// The MLS epoch the ciphertext was sealed at. See `generation`.
    #[serde(default)]
    pub epoch: Option<i64>,
    /// `SHA-256(delete_token)`, base64 — the per-envelope **deletion capability** (#1086); absent → membership-only deletes.
    #[serde(default)]
    pub delete_token_hash: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct EditMessageBody {
    pub envelope_id: String,
    pub conversation_id: String,
    pub target_message_id: String,
    #[serde(default)]
    pub sender_id: Option<String>,
    pub ciphertext: String,
    /// See [`SendMessageBody::sent_at`] — bounded exactly like a send's.
    pub sent_at: String,
    /// See [`SendMessageBody::generation`] — an edit is sealed exactly like a
    /// message and is gated exactly like one.
    #[serde(default)]
    pub generation: Option<i64>,
    /// See [`SendMessageBody::epoch`].
    #[serde(default)]
    pub epoch: Option<i64>,
    /// The TARGET message's deletion capability, base64 (#1086): an edit replaces a pending edit, which is a delete.
    #[serde(default)]
    pub delete_token: Option<String>,
}

/// The `409 Conflict` body for an envelope not at the log head (#1041); catch up, re-seal and post again.
#[derive(Debug, Serialize, Deserialize)]
pub struct EpochBehind {
    pub error: String,
    pub head_generation: i64,
    pub head_epoch: i64,
}

impl EpochBehind {
    pub const ERROR: &'static str = "epoch_behind";
}

#[derive(Serialize, Deserialize)]
pub struct DeleteMessageBody {
    pub message_id: String,
    pub conversation_id: String,
    /// The original author the client resolved (#607); trusted only for branching, never for a permission grant.
    #[serde(default)]
    pub msg_sender_id: Option<String>,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
    /// The deletion-capability preimage (#1086), required by the self-delete branch when the row has a hash.
    #[serde(default)]
    pub delete_token: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ReactionBody {
    pub message_id: String,
    pub emoji: String,
    /// No-auth fallback for the reacting user.
    #[serde(default)]
    pub user_id: Option<String>,
    /// The conversation the reacted-to message belongs to, for the membership check (#1161); required once the envelope is GC'd.
    #[serde(default)]
    pub conversation_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct WatermarkBody {
    /// The actor must be a current member of this conversation (→ 403).
    pub conversation_id: String,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    /// When signed it must equal the SIGNING device (→ 403); the row is keyed
    /// on the verified device, never on this field alone.
    pub device_id: String,
    /// The DS-assigned position handled up to (#1087); the next fetch uses `seq > last_seq`, and it never rewinds.
    #[serde(default)]
    pub last_seq: Option<i64>,
    /// **Legacy** (#1087): the highest envelope `sent_at` handled, admitted only via `check_cursor_stamp`.
    pub last_fetched_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct EnvelopeGcBody {
    pub conversation_id: String,
    /// `true` → DM cleanup query; `false` → group-channel cleanup query.
    pub is_dm: bool,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// `POST /v1/attachments/register` — two variants, so the send path cannot omit the message id (#925). Untagged: wire form unchanged.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttachmentRegisterBody {
    /// Send path: register the object AND a reference (#690); declared FIRST because `untagged` tries variants in order.
    ForMessage {
        content_hash: String,
        r2_key: String,
        message_id: String,
        /// The message's deletion capability (#1086), proving authorship under sealed sender.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delete_token: Option<String>,
    },
    /// Upload-time dedup registration: the object exists, no message carries it
    /// yet, so there is no reference to count. Also what a pre-#690 client
    /// sends.
    ObjectOnly { content_hash: String, r2_key: String },
}

impl AttachmentRegisterBody {
    /// The object hash, whichever operation this is.
    pub fn content_hash(&self) -> &str {
        match self {
            Self::ForMessage { content_hash, .. } | Self::ObjectOnly { content_hash, .. } => {
                content_hash
            }
        }
    }

    /// The R2 key, whichever operation this is.
    pub fn r2_key(&self) -> &str {
        match self {
            Self::ForMessage { r2_key, .. } | Self::ObjectOnly { r2_key, .. } => r2_key,
        }
    }

    /// The message this registration references, if it is the send path.
    pub fn message_id(&self) -> Option<&str> {
        match self {
            Self::ForMessage { message_id, .. } => Some(message_id),
            Self::ObjectOnly { .. } => None,
        }
    }

    /// The deletion capability presented for the referenced message, if any.
    pub fn delete_token(&self) -> Option<&str> {
        match self {
            Self::ForMessage { delete_token, .. } => delete_token.as_deref(),
            Self::ObjectOnly { .. } => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct AttachmentDeleteBody {
    pub content_hash: String,
    /// The message being deleted (#690): release its reference first; absent → release nothing.
    #[serde(default)]
    pub message_id: Option<String>,
}

/// `POST /v1/reactions/add` — add the authenticated user's reaction.
///
/// A transparent newtype over [`ReactionBody`], so each endpoint has its own path type.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddReaction(pub ReactionBody);

/// `POST /v1/reactions/remove` — remove the authenticated user's reaction. See
/// [`AddReaction`] for why this is a newtype.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveReaction(pub ReactionBody);
