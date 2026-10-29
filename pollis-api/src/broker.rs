//! Authorized-secrets broker (#393): LiveKit tokens, fan-out and roster, and R2
//! presigns. The DS holds the secrets; the client asks. Wire types only; see
//! `pollis-delivery` for behaviour.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub struct LivekitTokenBody {
    /// The LiveKit room to mint a token for.
    pub room: String,
    /// Identity scheme (default `realtime`): `realtime` → `d-…` (data only),
    /// `voice` → `v-…`, `view` → `w-…` (screenshare receive). User and device
    /// always come from the verified signer.
    ///
    /// #836: the identity is an opaque per-room pseudonym (user and device
    /// encrypted under a per-room key), so the SFU cannot link participants to
    /// accounts or across rooms; only the capability prefix is legible. Resolve
    /// with [`LivekitIdentitiesBody`].
    #[serde(default)]
    pub kind: Option<String>,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub user_id: Option<String>,
    /// No-auth path only; with auth on, the device comes from the verified
    /// `X-Pollis-Device` header.
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct LivekitSendDataBody {
    /// The room to publish into (`inbox-<user>`, a group id, `call-<ulid>`, …).
    pub room: String,
    /// The JSON control payload, encoded server-side into the Twirp `data`
    /// field.
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
/// The per-room key stays server-side (as in #828): shipped in a binary it could
/// be extracted. Authorization equals minting a token for `room`, so this
/// reveals nothing joining the room would not.
#[derive(Serialize, Deserialize)]
pub struct LivekitIdentitiesBody {
    /// The logical room the identities were minted for (not the LiveKit
    /// pseudonym); keys are room-scoped.
    pub room: String,
    /// The identities to resolve; unresolvable ones are absent, not an error.
    pub identities: Vec<String>,
    /// No-auth path only; ignored when auth is enforced.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// Hard ceiling on one presigned media PUT: the client's 100 MiB cache cap plus
/// a 16-byte tag per 4 MiB chunk, rounded up. A presign with no signed length
/// would permit an object of any size. Shared so the uploader and the DS agree.
pub const R2_MEDIA_MAX_BYTES: u64 = 104 * 1024 * 1024;

/// Hard ceiling on one presigned avatar / group-icon PUT.
pub const R2_PUBLIC_IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize)]
pub struct R2PresignBody {
    /// `"get"`, `"put"` or `"delete"` (attachment cleanup).
    pub operation: String,
    /// The R2 object key (within the bucket), e.g. `media/<hash>/<file>.enc`.
    pub key: String,
    /// Optional content type, for forward-compat; the URL signs only `host`.
    #[serde(default)]
    pub content_type: Option<String>,
    /// The exact byte count the client will PUT; when present it is signed, so
    /// R2 rejects any other size. Required for `put` on `emoji/…` keys (#848):
    /// those are unencrypted and public, so a believed size is no bound.
    #[serde(default)]
    pub content_length: Option<u64>,
    /// No-auth path only (`pollis_delivery::broker::resolve_user`); presign has
    /// no per-object authz.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/livekit/token`: the participant JWT and the SFU to present it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitTokenResponse {
    pub token: String,
    /// The DS's LiveKit address. Empty is the self-host case (no `LIVEKIT_URL`),
    /// resolved against the client's compiled-in fallback (`resolve_livekit_url`).
    pub url: String,
}

/// `POST /v1/livekit/send-data`: the fan-out was accepted upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitSendDataResponse {
    pub ok: bool,
}

/// One opaque LiveKit participant identity resolved back to a Pollis user
/// (#836); only the DS can recover it under the room's key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedIdentity {
    pub identity: String,
    pub user_id: String,
    /// Display name, from the DS's DB: since #836 the JWT carries no `name`.
    #[serde(default)]
    pub name: String,
    /// `voice` / `view` / `realtime`. Absent on the roster endpoint (already
    /// voice-only), and skipped when `None` so the roster's live wire bytes
    /// stay unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `POST /v1/livekit/participants`: the room's voice roster, internal
/// participants and screenshare receivers filtered out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitParticipantsResponse {
    #[serde(default)]
    pub participants: Vec<ResolvedIdentity>,
}

/// `POST /v1/livekit/identities`: the requested identities the DS could resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitIdentitiesResponse {
    #[serde(default)]
    pub identities: Vec<ResolvedIdentity>,
}

/// `POST /v1/r2/presign`: the presigned URL and how it must be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct R2PresignResponse {
    /// Self-contained: the SigV4 signature is in the query string.
    pub url: String,
    /// The HTTP method the signature is bound to; any other is a 403 from R2.
    pub method: String,
    /// Seconds until the signature expires.
    pub expires_in: u64,
}
