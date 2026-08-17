//! Authorized-secrets broker (#393): LiveKit tokens/fan-out/roster, the Turso
//! read-only token, and R2 presigns. The DS holds the secrets; the client asks.
//!
//! Wire types only — no handler logic, no DB access. See the matching module in
//! `pollis-delivery` for what the server does with each one.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct LivekitTokenBody {
    /// The LiveKit room to mint a token for.
    pub room: String,
    /// Identity scheme (default `realtime`). The user + device halves are ALWAYS
    /// taken from the verified signer — `kind` only picks the capability class so
    /// a single endpoint serves every on-device scheme:
    ///   - `realtime` → `d-…`  (data-only realtime/inbox)
    ///   - `voice`    → `v-…`  (voice participant)
    ///   - `view`     → `w-…`  (screenshare receive; no data channel)
    ///
    /// #836: the identity itself is an opaque, per-room pseudonym — the user and
    /// device are ENCRYPTED into it under a key derived per logical room, so the
    /// SFU cannot map a participant to an account or recognise the same account
    /// across two rooms. Only the 2-character capability prefix stays legible,
    /// because both ends route on it. Resolve one with
    /// [`LivekitIdentitiesBody`]. See `pollis_delivery::participant_id`.
    #[serde(default)]
    pub kind: Option<String>,
    /// No-auth path only: the user to mint for. IGNORED when auth is enforced
    /// (the user comes from the verified signer there).
    #[serde(default)]
    pub user_id: Option<String>,
    /// No-auth path only: the device id half of the identity. IGNORED when auth
    /// is enforced (the device comes from the signature-verified `X-Pollis-Device`
    /// header there — a client cannot claim another device's identity).
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LivekitSendDataBody {
    /// The room to publish into (`inbox-<user>`, a group id, `call-<ulid>`, …).
    pub room: String,
    /// The JSON control payload — serialized + base64'd server-side into the
    /// Twirp `data` field. The client never touches the admin token or wire form.
    pub payload: serde_json::Value,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LivekitParticipantsBody {
    /// The room whose voice roster to list (a group id).
    pub room: String,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// Resolve opaque LiveKit participant identities back to Pollis users (#836).
///
/// Participant identities are per-room pseudonyms minted by the DS, so a client
/// that learns one from the LiveKit event stream cannot parse it — it asks here.
/// The DS decrypts each one with the room's key and answers with the user (plus
/// the profile fields the roster would have carried anyway).
///
/// The key stays server-side deliberately, exactly as in #828: a per-room key
/// shipped in a release binary could be extracted, and one that resolves every
/// room would hand back the whole graph. Authorization is the same as minting a
/// token for `room`, so this reveals nothing the caller could not already see by
/// joining it.
#[derive(Serialize, Deserialize)]
pub struct LivekitIdentitiesBody {
    /// The LOGICAL room the identities were minted for (a conversation id,
    /// `inbox-<user_id>`, or `call-<ulid>`) — not the LiveKit pseudonym. Identity
    /// keys are room-scoped, so resolving against the wrong room yields nothing.
    pub room: String,
    /// The opaque identities to resolve. Unresolvable entries (internal
    /// participants, forgeries, another room's identities) are simply absent
    /// from the response rather than erroring the batch.
    pub identities: Vec<String>,
    /// No-auth path only: the user to authorize as. IGNORED when auth is
    /// enforced (the user comes from the verified signer there).
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct R2PresignBody {
    /// `"get"` → presign a GET (download); `"put"` → presign a PUT (upload);
    /// `"delete"` → presign a DELETE (attachment cleanup).
    pub operation: String,
    /// The R2 object key (within the bucket), e.g. `media/<hash>/<file>.enc`.
    pub key: String,
    /// Optional content type — accepted for forward-compat; the presigned URL
    /// signs only `host`, so the client sets Content-Type at upload time.
    #[serde(default)]
    pub content_type: Option<String>,
    /// The EXACT byte count the client will PUT. When present, `content-length`
    /// is added to the signed headers, so R2 rejects a body of any other size —
    /// the presign stops being "here is permission to write, of any size".
    ///
    /// REQUIRED for `put` on an `emoji/…` key (#848): those objects are
    /// unencrypted, publicly fetchable, and bounded by
    /// `pollis_delivery::emoji::EMOJI_MAX_BYTES`, and a size the server merely *believes*
    /// is not a bound at all. Optional everywhere else, so every existing
    /// media/avatar presign is byte-identical to before.
    #[serde(default)]
    pub content_length: Option<u64>,
    /// No-auth path only — see `pollis_delivery::broker::resolve_user`. Unused beyond the auth gate
    /// (presign has no per-object authz), kept for shape-symmetry with the other
    /// broker endpoint.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/turso/token` — mint a short-TTL READ-ONLY Turso token (#393).
///
/// Carries no parameters: the token's scope is fixed server-side (read-only, one
/// database) and its bearer is the verified signer. Empty for the same reason as
/// [`crate::emoji::EmojiGcBody`] — so the endpoint is a row in the
/// [`crate::ENDPOINTS`] table rather than a loose `json!({})`.
#[derive(Serialize, Deserialize)]
pub struct TursoTokenBody {}
