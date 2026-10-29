//! Message envelopes, edits/deletes, reactions, watermarks, envelope GC and
//! attachment reference rows.
//!
//! Wire types only; see the matching `pollis-delivery` module for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct SendMessageBody {
    pub id: String,
    pub conversation_id: String,
    /// Unsealed: bound to the authenticated user when signed. Sealed (#331): a
    /// non-identifying sentinel, persisted as-is (see `apply_send_message`).
    #[serde(default)]
    pub sender_id: Option<String>,
    /// The `"mls:<hex>"` ciphertext string (text, so no base64).
    pub ciphertext: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    /// The envelope's delivery stamp (`envelope_sent_at`), stored verbatim. Only
    /// a canonical UTC RFC 3339 stamp within the signature window is admitted
    /// (`check_cursor_stamp`, else 400): recipients and the GC floor read it, so
    /// a far-future value would black out the conversation.
    pub sent_at: String,
    /// Sealed sender flag (#331): `1` → `sender_id` is a sentinel and the true
    /// sender is in the MLS credential. Absent → `0`.
    #[serde(default)]
    pub sealed: i64,
    /// Who to wake with a content-free push (#987, #843; the DS fans out, so the
    /// client never reads other people's `push_token` rows).
    ///
    /// * `None` → no push.
    /// * `Some([])` → every member.
    /// * `Some([ids])` → only those, intersected with membership server-side.
    ///
    /// Since #1431 clients send `Some([])` for all channel chatter and the DS
    /// drops recipients who muted the channel, unless `push_mentions` names
    /// them. Pre-#1431 clients send `None` for chatter.
    #[serde(default)]
    pub push_to: Option<Vec<String>>,
    /// The members named directly by `@username` (#1431), pushed even if they
    /// muted the channel; they must still be in `push_to` and members. Absent
    /// (pre-#1431) → the non-empty `push_to` list itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_mentions: Option<Vec<String>>,
    /// The MLS lineage the ciphertext was sealed at (#1041). With `epoch`, the
    /// envelope's log position: the DS refuses one that is not the head when it
    /// lands, since with `max_past_epochs = 0` members past that commit could
    /// never decrypt it (see [`EpochBehind`]). Absent → ungated.
    #[serde(default)]
    pub generation: Option<i64>,
    /// The MLS epoch the ciphertext was sealed at. See `generation`.
    #[serde(default)]
    pub epoch: Option<i64>,
    /// `SHA-256(delete_token)`, base64: the per-envelope deletion capability
    /// (#1086). Under sealed sender the DS cannot tell whose envelope this is,
    /// so possession of the preimage, `HMAC-SHA256(k, message_id)` under a key
    /// HKDF'd from the account identity key, is what authorizes a self-delete;
    /// every device of the author can recompute it. Absent (older clients) →
    /// deletes fall back to the membership-only check.
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
    /// See [`SendMessageBody::sent_at`].
    pub sent_at: String,
    /// See [`SendMessageBody::generation`]; edits are gated like messages.
    #[serde(default)]
    pub generation: Option<i64>,
    /// See [`SendMessageBody::epoch`].
    #[serde(default)]
    pub epoch: Option<i64>,
    /// The target message's deletion capability, base64 (#1086). An edit
    /// replaces any pending edit of the target, which is a delete, so it needs
    /// the same capability whenever the target row has a hash.
    #[serde(default)]
    pub delete_token: Option<String>,
}

/// The `409 Conflict` body of send/edit when the envelope's `(generation,
/// epoch)` is not the log head (#1041). Nothing was stored; the client catches
/// up, re-seals and posts again. `error` is [`EpochBehind::ERROR`], so another
/// 409 (a lost commit race) is never mistaken for it.
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
    /// The author the client resolved locally (#607). Selects the self-vs-admin
    /// branch only and never grants a permission: the admin branch re-derives
    /// the role.
    #[serde(default)]
    pub msg_sender_id: Option<String>,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
    /// The deletion capability (#1086), the preimage of `delete_token_hash`.
    /// Required by the self-delete branch when the row has a hash (absent or
    /// wrong → `Forbidden`); ignored by the admin branch. A row with no hash
    /// takes the membership-only path.
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
    /// The conversation the reacted-to message belongs to, which the DS checks
    /// membership against (#1161): envelope GC usually leaves nothing to resolve
    /// it from. Not optional in effect: with the envelope gone and no value, the
    /// DS refuses; with the envelope present, the value is cross-checked.
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
    /// The DS-assigned delivery position handled up to (#1087), the next fetch's
    /// cursor (`seq > last_seq`). Monotone: the upsert keeps the max. `None`
    /// from a pre-#1087 client.
    #[serde(default)]
    pub last_seq: Option<i64>,
    /// Legacy, no longer the cursor (#1087): the highest envelope `sent_at`
    /// handled. Admitted only through `check_cursor_stamp`; the upsert never
    /// rewinds, so a far-future value would black out this device and stall GC.
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

/// `POST /v1/attachments/register`: two operations (#925) as two variants, so
/// the send path cannot register an object without naming its message (which
/// would leave it unreferenced and collectable). `#[serde(untagged)]` keeps the
/// deployed wire form, including pre-#690 object-only bodies.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttachmentRegisterBody {
    /// Send path: register the object and a `(content_hash, message_id)`
    /// reference (#690). Declared first, since `untagged` tries variants in
    /// order and a body with `message_id` must match here.
    ForMessage {
        content_hash: String,
        r2_key: String,
        message_id: String,
        /// The message's deletion capability (#1086), checked against the
        /// envelope's `delete_token_hash` since a sealed `sender_id` names no
        /// one. Absent (older clients) → accepted only for unsealed envelopes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        delete_token: Option<String>,
    },
    /// Upload-time dedup registration: no message carries the object yet. Also
    /// what a pre-#690 client sends.
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
    /// The message being deleted (#690): its reference is released before the
    /// conditional collection. Absent (pre-#690) → release nothing; the object
    /// is still collected only when no reference remains.
    #[serde(default)]
    pub message_id: Option<String>,
}

/// `POST /v1/reactions/add`. A transparent newtype over [`ReactionBody`]: add
/// and remove share a shape, but a path belongs to a type.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddReaction(pub ReactionBody);

/// `POST /v1/reactions/remove`; see [`AddReaction`].
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveReaction(pub ReactionBody);
