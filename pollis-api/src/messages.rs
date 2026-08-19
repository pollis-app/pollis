//! Message envelopes, edits/deletes, reactions, watermarks, envelope GC and
//! attachment reference rows.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

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
    pub sent_at: String,
    /// Sealed sender flag (issue #331, `docs/metadata-minimization-design.md`
    /// §2). `1` → `sender_id` is a blinded sentinel; the true sender lives in the
    /// MLS credential. Absent (old clients / unsealed sends) → `0`.
    #[serde(default)]
    pub sealed: i64,
    /// Who to WAKE with a content-free push (#987, #843).
    ///
    /// The push fan-out moved server-side with this field: the DS already knows
    /// the conversation and the sender, so all it was missing was the mention
    /// list. Doing it here removes the client's need to read other people's
    /// `push_token` rows — the most identifying row it had any reason to read
    /// about someone else — and takes two dependent round trips off the hot send
    /// path.
    ///
    /// * `None` → do not push at all (a suppressed send).
    /// * `Some([])` → every member. The DM and `@all` behaviour.
    /// * `Some([ids])` → only those, INTERSECTED with real membership
    ///   server-side, so this can only ever narrow the audience — never widen it
    ///   past the conversation.
    ///
    /// The push itself carries `{ conversationId, kind }` and nothing else.
    #[serde(default)]
    pub push_to: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize)]
pub struct EditMessageBody {
    pub envelope_id: String,
    pub conversation_id: String,
    pub target_message_id: String,
    #[serde(default)]
    pub sender_id: Option<String>,
    pub ciphertext: String,
    pub sent_at: String,
}

#[derive(Serialize, Deserialize)]
pub struct DeleteMessageBody {
    pub message_id: String,
    pub conversation_id: String,
    /// The original author the client resolved (from its local cache). Selects
    /// the self-vs-admin branch (Solution A, #607): the DS can no longer re-derive
    /// authorship from the sealed envelope, so it trusts this hint for BRANCHING
    /// only. It is never trusted for a permission grant — the admin branch
    /// re-derives the admin role independently, and the self branch grants nothing
    /// beyond envelope removal that membership doesn't already permit.
    #[serde(default)]
    pub msg_sender_id: Option<String>,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ReactionBody {
    pub message_id: String,
    pub emoji: String,
    /// No-auth fallback for the reacting user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct WatermarkBody {
    pub conversation_id: String,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    pub device_id: String,
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

/// `POST /v1/attachments/register` — the two things this endpoint is asked to do.
///
/// One body served both operations, with `message_id: Option<String>` as the
/// only thing telling them apart (#925). That made the send path's failure mode
/// — forgetting the message id, so the object is registered with no reference
/// and is collectable the moment anything sweeps — a `None` that typechecks.
/// Two variants make it unrepresentable instead: the send path cannot express
/// "reference this message" without naming the message.
///
/// `#[serde(untagged)]`, so the WIRE FORM is unchanged in both directions: a
/// send-path body still serializes as `{content_hash, r2_key, message_id}` and a
/// dedup body as `{content_hash, r2_key}`. A pre-#690 client (object only) still
/// deserializes, as [`ObjectOnly`](AttachmentRegisterBody::ObjectOnly) — which
/// matters, because this is a deployed endpoint.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttachmentRegisterBody {
    /// Send path: register the object AND a `(content_hash, message_id)`
    /// reference, so the shared convergent object is reference-counted (#690).
    ///
    /// Declared FIRST because `untagged` tries variants in order: a body
    /// carrying a `message_id` must match here, not fall through to
    /// [`ObjectOnly`](AttachmentRegisterBody::ObjectOnly) with the field ignored.
    ForMessage {
        content_hash: String,
        r2_key: String,
        message_id: String,
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
}

#[derive(Serialize, Deserialize)]
pub struct AttachmentDeleteBody {
    pub content_hash: String,
    /// The id of the message being deleted (#690). Present → release that
    /// message's `(content_hash, message_id)` reference before the (now
    /// conditional) object collection. Absent (pre-#690 client) → release
    /// nothing; the object is still only collected when NO reference remains, so
    /// an old client can no longer strand a hash a newer client references.
    #[serde(default)]
    pub message_id: Option<String>,
}

/// `POST /v1/reactions/add` — add the authenticated user's reaction.
///
/// A `#[serde(transparent)]` newtype over [`ReactionBody`], not a second copy of
/// its fields: add and remove take byte-identical JSON, but a path is a property
/// of the TYPE here, so one type cannot address two endpoints. The wrapper keeps
/// one field list while still making "which endpoint" a compile-time fact — and
/// stops a caller reaching for the remove path with an add body.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct AddReaction(pub ReactionBody);

/// `POST /v1/reactions/remove` — remove the authenticated user's reaction. See
/// [`AddReaction`] for why this is a newtype.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoveReaction(pub ReactionBody);
