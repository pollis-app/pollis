//! Authorized-secrets broker (#393): LiveKit tokens/fan-out/roster and R2 presigns; the DS holds the secrets.
//!
//! Wire types only; see `pollis-delivery` for handling.

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
    /// The identity is an opaque per-room pseudonym (#836); resolve with [`LivekitIdentitiesBody`].
    #[serde(default)]
    pub kind: Option<String>,
    /// No-auth path only: the user to mint for. IGNORED when auth is enforced
    /// (the user comes from the verified signer there).
    #[serde(default)]
    pub user_id: Option<String>,
    /// No-auth path only: the device half; ignored when auth is enforced.
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
/// The per-room key stays server-side (#828); authz matches minting a token for `room`.
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

/// Hard ceiling on one presigned media PUT.
///
/// 100 MiB plaintext plus per-chunk AEAD tags; a presign without a signed length permits any size.
pub const R2_MEDIA_MAX_BYTES: u64 = 104 * 1024 * 1024;

/// Hard ceiling on one presigned avatar / group-icon PUT. These are decoration
/// rendered in a list; a few megabytes is already generous.
pub const R2_PUBLIC_IMAGE_MAX_BYTES: u64 = 8 * 1024 * 1024;

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
    /// Required for `put` on `emoji/…` keys (#848), which are unencrypted and public.
    #[serde(default)]
    pub content_length: Option<u64>,
    /// No-auth path only — see `pollis_delivery::broker::resolve_user`.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// `POST /v1/livekit/token` — the participant JWT and the SFU to present it to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitTokenResponse {
    pub token: String,
    /// The DS's LiveKit address; EMPTY means self-host (the client falls back to its compiled-in one).
    pub url: String,
}

/// `POST /v1/livekit/send-data` — the fan-out was accepted upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitSendDataResponse {
    pub ok: bool,
}

/// One opaque LiveKit participant identity, resolved back to a Pollis user
/// (#836).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedIdentity {
    pub identity: String,
    pub user_id: String,
    /// Display name, from the DS's DB (the JWT carries no `name` since #836).
    #[serde(default)]
    pub name: String,
    /// `voice` / `view` / `realtime`. Absent on the roster endpoint, which has
    /// already filtered to real voice participants; present when resolving raw
    /// identities off the SDK event stream.
    ///
    /// Skipped when `None` so the roster's bytes are unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

/// `POST /v1/livekit/participants` — the room's voice roster, internal
/// participants and screenshare receivers already filtered out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitParticipantsResponse {
    #[serde(default)]
    pub participants: Vec<ResolvedIdentity>,
}

/// `POST /v1/livekit/identities` — the identities the caller asked about, minus
/// any the DS could not resolve (internal participants, forgeries, another
/// room's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivekitIdentitiesResponse {
    #[serde(default)]
    pub identities: Vec<ResolvedIdentity>,
}

/// `POST /v1/r2/presign` — the presigned URL and how it must be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct R2PresignResponse {
    /// Self-contained: the SigV4 signature is in the query string, so the caller
    /// attaches no auth headers.
    pub url: String,
    /// The HTTP method the signature is bound to. Using any other method against
    /// this URL is a 403 from R2, so it is not decorative.
    pub method: String,
    /// Seconds until the signature expires.
    pub expires_in: u64,
}
