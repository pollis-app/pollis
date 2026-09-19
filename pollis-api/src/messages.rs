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
    /// The envelope's delivery-cursor stamp, as the client wrote it
    /// (`pollis_core::commands::messages::envelope_sent_at`). Stored verbatim
    /// when admitted, and admitted ONLY as a canonical UTC RFC 3339 stamp no
    /// further ahead of the DS clock than the request-signature window
    /// (`pollis_delivery::messages::check_cursor_stamp`, → 400 otherwise): every
    /// recipient adopts it as its fetch cursor and the GC floor reads it, so a
    /// far-future value would black out the conversation for everyone.
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
    /// The MLS lineage the ciphertext was sealed at, as the client read it off
    /// its own envelope (#1041). Together with `epoch` this is the envelope's
    /// position in the commit log; the DS refuses to keep an envelope whose
    /// position is not the log's head at the moment it lands, because with
    /// `max_past_epochs = 0` every member that has already applied the next
    /// commit could never decrypt it. See `EpochBehind`.
    ///
    /// Absent (an older client) → ungated, exactly the pre-#1041 write. A client
    /// that lies here only hurts its own message.
    #[serde(default)]
    pub generation: Option<i64>,
    /// The MLS epoch the ciphertext was sealed at. See `generation`.
    #[serde(default)]
    pub epoch: Option<i64>,
    /// `SHA-256(delete_token)`, base64 — the per-envelope **deletion capability**
    /// (#1086), stored alongside the row and required to remove it later.
    ///
    /// Sealed sender means the DS cannot tell whose envelope this is, so the
    /// self-delete branch trusted the caller's own claim and any member could
    /// remove any envelope in a conversation before slower recipients fetched it
    /// — an attacker-controlled fourth message loss, on top of the three
    /// CLAUDE.md allows. Possession of the preimage replaces the claim: the DS
    /// authorizes nobody, it just checks a hash.
    ///
    /// The token is derived, not stored: `HMAC-SHA256(k, message_id)` under a key
    /// HKDF'd from the account identity key, so **every device of the author**
    /// can recompute it and nobody else can. The hash reveals nothing linkable —
    /// it is a fresh-looking 32 bytes per message.
    ///
    /// Absent (an older client) → the row keeps a NULL hash and deletes fall back
    /// to the membership-only check. That is the rollout: the DS cannot require a
    /// capability for envelopes written before clients produced one.
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
    /// The TARGET message's deletion capability, base64 (#1086).
    ///
    /// An edit replaces any pending edit of the same target, which is a delete —
    /// so without this, one member could clobber another author's unfetched
    /// edit exactly the way they could delete an unfetched message. Required
    /// whenever the target row has a hash; the edit's own new envelope carries
    /// the target's hash too, so a later edit can replace it in turn.
    #[serde(default)]
    pub delete_token: Option<String>,
}

/// The `409 Conflict` body `/v1/messages/send` and `/v1/messages/edit` answer
/// when the envelope's asserted `(generation, epoch)` is not the commit log's
/// head (#1041). Nothing was stored. The client's only correct move is to catch
/// up (apply the commit(s) it is missing), re-seal at the new epoch, and post
/// again — the same convergence a lost commit race demands.
///
/// `error` is the constant [`EpochBehind::ERROR`], so a 409 from a different
/// cause (a lost commit race on `/v1/commits` is also a 409) can never be
/// mistaken for this one.
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
    /// The per-envelope deletion capability, base64 (#1086) — the preimage of
    /// the `delete_token_hash` stored at send time. Required by the self-delete
    /// branch whenever the target row HAS a hash; ignored by the admin branch,
    /// which is a re-derived permission and not a claim of authorship.
    ///
    /// Absent, or wrong, against a row that has a hash → `Forbidden`. Absent
    /// against a row with no hash → the pre-#1086 membership-only path, for
    /// envelopes older clients wrote.
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
    /// The conversation the reacted-to message belongs to — what the DS checks
    /// membership against (#1161).
    ///
    /// Reactions are membership-gated through the message's `message_envelope`
    /// row, and envelope GC collects that row as soon as every member device has
    /// fetched it, so for anything but a very recent message there is nothing
    /// left to resolve the conversation from. The gate used to be SKIPPED in
    /// that case, which is the common case — a non-member who knows a message id
    /// could write a reaction row for it. The client knows the conversation from
    /// its own local copy of the message, so it declares it here and the DS
    /// checks membership without needing the envelope.
    ///
    /// `#[serde(default)]` for shape compatibility only, NOT as a way to opt out
    /// of the check: when the envelope is gone and no conversation is declared,
    /// the DS refuses. An optional-and-ignored field would close nothing — an
    /// attacker composes the body and would simply omit it.
    ///
    /// A declared value is cross-checked against the envelope whenever one still
    /// exists, so it cannot be used to have the membership question asked about
    /// some other conversation.
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
    /// The DS-assigned delivery position this device has handled up to (#1087).
    /// The cursor the next fetch uses: `seq > last_seq`. Monotone — the upsert
    /// takes `MAX(existing, reported)`, so a cursor never rewinds.
    ///
    /// `None` from a client that predates #1087; such a report advances only the
    /// legacy timestamp and is what keeps `last_fetched_at` alive for one
    /// release. See `apply_advance_watermark`.
    #[serde(default)]
    pub last_seq: Option<i64>,
    /// **Legacy**, and no longer the cursor (#1087): the highest envelope
    /// `sent_at` this device has handled.
    /// Admitted only through `check_cursor_stamp` (canonical UTC RFC 3339,
    /// within the signature window of the DS clock, → 400 otherwise) — the
    /// upsert is monotone and never rewinds, so a far-future cursor would be a
    /// permanent blackout for this device and, once every device reported one,
    /// GC of the whole conversation.
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
