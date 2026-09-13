//! Authorized-secrets broker (#393).
//!
//! Two operations still done on-device hold a long-lived secret in the client
//! bundle: minting a LiveKit access token (needs the LiveKit API secret) and
//! reaching R2 (needs the R2 access key + secret). Shipping those secrets in
//! the client is the whole problem — anyone who unpacks the app can extract
//! them. This module moves both server-side: the DS holds the secrets in its
//! env, the (already device-signed) client asks the DS to mint a token / presign
//! a URL, and the secrets never leave the server.
//!
//! Both endpoints reuse the existing device-signature auth ([`crate::auth`] via
//! [`crate::writes::gate`]) — no new auth scheme. The point of server-side
//! minting is precisely that the **identity is derived from the verified
//! signer, not from anything the client sends**: a client cannot mint a LiveKit
//! token as another user.
//!
//! ## Why R2 presign needs no per-object READ authz (but delete is gated)
//!
//! Pollis media is **convergent-encrypted** (see `pollis-core`'s `r2.rs`):
//! the AES-256-GCM key is derived from `SHA-256(plaintext)`, and the
//! `attachment_object` table is a **global content-hash dedup** with no
//! conversation binding at all. A presigned URL therefore only ever exposes
//! **ciphertext** — confidentiality comes from MLS key distribution (only a
//! member who decrypted the message learns the content hash, and only the
//! content hash derives the decryption key), NOT from the R2 ACL. So for `get`
//! and `put` the presign gate exists solely to stop **anonymous internet
//! access** to the bucket; it does not — and cannot meaningfully — enforce read
//! authz on a per-object basis. Requiring an authenticated device is the right
//! and sufficient gate there.
//!
//! `delete` is different, and NOT for confidentiality — for **integrity of a
//! SHARED object** (#690). Because the object is a global dedup, one convergent
//! blob backs every message that carries the file, across conversations and
//! users. Minting a `delete` for it while another message still references it
//! would 404 that attachment for everyone else. So the `delete` presign consults
//! the server-side reference count ([`crate::messages::object_is_referenced`],
//! DERIVED by joining `attachment_ref` declarations to the still-live
//! `message_envelope` rows — #690) and refuses to sign while any reference
//! remains — the same evidence that gates the Turso row's collection in
//! `apply_delete_attachment`. The DS is the chokepoint: a client that has already
//! deleted its own message cannot blow away a blob a second conversation still
//! needs. Because the count is derived, a reference cannot outlive its message:
//! once every referencing envelope is deleted or GC'd the gate opens on its own,
//! with no per-deleter bookkeeping to forget. This is a per-object *integrity*
//! gate, not the per-object *read* authz
//! the paragraph above (still correctly) says the bucket does not need.
//!
//! ## Contract
//!
//! This module's request/response shapes ARE the contract the frontend `bridge`
//! (and mobile, via uniffi) will call once the on-device LiveKit/R2 paths are
//! removed (that client cutover is the follow-up to #393). See
//! `docs/secrets-broker.md`.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use libsql::Connection;
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate_and_parse, is_member, ok_response, Authed, RawRequest};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::broker::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::broker::*;

// ── Config ───────────────────────────────────────────────────────────────────

/// Secrets the broker needs, read from DS env in [`BrokerConfig::from_env`]. All
/// `Option` — a missing secret makes the matching endpoint return 503 (the
/// endpoint still exists and answers, mirroring OTP with no Resend key) rather
/// than failing at startup. Default is all-`None` (no broker configured), so the
/// integration harness and unconfigured deploys keep working.
#[derive(Clone, Default)]
pub struct BrokerConfig {
    /// LiveKit API key — the JWT `iss` claim (env `LIVEKIT_API_KEY`).
    pub livekit_api_key: Option<String>,
    /// LiveKit API secret — the HS256 signing key (env `LIVEKIT_API_SECRET`).
    /// NEVER logged.
    pub livekit_api_secret: Option<String>,
    /// LiveKit ws URL handed back to the client (env `LIVEKIT_URL`).
    pub livekit_url: Option<String>,
    /// R2 S3 endpoint, e.g. `https://<acct>.r2.cloudflarestorage.com` (a trailing
    /// `/<bucket>` path segment is fine — the presigner uses only the host). Read
    /// from `R2_ENDPOINT`, falling back to the established `R2_S3_ENDPOINT`.
    pub r2_endpoint: Option<String>,
    /// R2 region — SigV4 scope; defaults to `auto` (env `R2_REGION`).
    pub r2_region: String,
    /// R2 bucket name (env `R2_BUCKET`).
    pub r2_bucket: Option<String>,
    /// R2 access key id (env `R2_ACCESS_KEY_ID`).
    pub r2_access_key_id: Option<String>,
    /// R2 secret access key — SigV4 signing secret. Read from
    /// `R2_SECRET_ACCESS_KEY`, falling back to the established `R2_SECRET_KEY`.
    /// NEVER logged.
    pub r2_secret_access_key: Option<String>,
}

impl BrokerConfig {
    /// Read every broker secret from the DS environment. Empty strings are
    /// treated as unset. `R2_REGION` defaults to `auto` (Cloudflare R2's region).
    pub fn from_env() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.is_empty());
        Self {
            livekit_api_key: var("LIVEKIT_API_KEY"),
            livekit_api_secret: var("LIVEKIT_API_SECRET"),
            livekit_url: var("LIVEKIT_URL"),
            // Accept the established client env names (`R2_S3_ENDPOINT` /
            // `R2_SECRET_KEY`) so the DS reuses the same Doppler secrets instead
            // of duplicating them under new keys.
            r2_endpoint: var("R2_ENDPOINT").or_else(|| var("R2_S3_ENDPOINT")),
            r2_region: var("R2_REGION").unwrap_or_else(|| "auto".to_string()),
            r2_bucket: var("R2_BUCKET"),
            r2_access_key_id: var("R2_ACCESS_KEY_ID"),
            r2_secret_access_key: var("R2_SECRET_ACCESS_KEY").or_else(|| var("R2_SECRET_KEY")),
        }
    }

    /// All three LiveKit fields present → the token endpoint can sign.
    fn livekit_ready(&self) -> Option<(&str, &str, &str)> {
        Some((
            self.livekit_api_key.as_deref()?,
            self.livekit_api_secret.as_deref()?,
            self.livekit_url.as_deref()?,
        ))
    }

    /// All R2 fields present → the presign endpoint can sign.
    fn r2_ready(&self) -> Option<(&str, &str, &str, &str)> {
        Some((
            self.r2_endpoint.as_deref()?,
            self.r2_bucket.as_deref()?,
            self.r2_access_key_id.as_deref()?,
            self.r2_secret_access_key.as_deref()?,
        ))
    }
}

/// Resolve the user the broker acts as.
///
///   - auth ON  → the verified signer; any client-supplied identity is ignored
///     (the whole point — a signed request can only act as itself).
///   - auth OFF → the body's `user_id` (no signed identity on the no-auth path).
///     Missing/empty → 400. Mirrors [`crate::writes`]' resolvers.
// The `Err` is the axum `Response` handed straight back to the client; boxing it
// would only move those bytes to the heap on a path taken once per rejected
// request.
#[allow(clippy::result_large_err)]
fn resolve_user(authed: &Authed, body_user_id: Option<&str>) -> Result<String, Response> {
    match authed {
        Some(u) => Ok(u.clone()),
        None => match body_user_id {
            Some(b) if !b.is_empty() => Ok(b.to_string()),
            _ => Err(bad_request("user_id required when auth is disabled")),
        },
    }
}

fn not_configured(what: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(serde_json::json!({ "error": format!("{what} broker not configured") })),
    )
        .into_response()
}

// ── 1. POST /v1/livekit/token ──────────────────────────────────────────────

/// LiveKit JWT claims. This is the only minter left: the on-device
/// `livekit_jwt::make_token` it was written to match was removed once #393
/// landed, and clients now ask `ds_livekit_*` for a token. The shape is still
/// frozen by the LiveKit SFU, which is what it has to satisfy.
#[derive(Serialize)]
struct LiveKitClaims {
    iss: String,
    sub: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    name: String,
    video: VideoGrants,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct VideoGrants {
    room: String,
    room_join: bool,
    can_publish: bool,
    can_subscribe: bool,
    can_publish_data: bool,
}

/// POST /v1/livekit/token — mint a LiveKit access token for the authenticated
/// user. Identity is derived SERVER-SIDE from the verified signer (a client
/// cannot mint a token as someone else). Authorizes the room: the user's own
/// inbox room (`inbox-<user_id>`) is always allowed; any other room requires
/// current membership.
///
/// #836: the identity that reaches LiveKit is an opaque per-room pseudonym, and
/// the JWT carries no display name. Both used to be raw account data — the
/// identity was `{user_id}:{device_id}` and `name` was the username — which
/// handed the SFU the membership of every room and a stable handle to cluster
/// rooms by. Deriving the pseudonym here, from the signer, is what keeps
/// "a client cannot mint a token as another user" true while the value itself
/// becomes meaningless to the SFU.
pub async fn livekit_token(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LivekitTokenBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    if parsed.room.trim().is_empty() {
        return Ok(bad_request("room required"));
    }

    // Secrets gate: a clear 503 when the broker isn't configured, just like OTP
    // with no Resend key.
    let (api_key, api_secret, url) = match state.broker.livekit_ready() {
        Some(t) => t,
        None => return Ok(not_configured("livekit")),
    };

    let user_id = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    // Room authz — only on the signed path (mirrors the other handlers, which
    // skip authz when auth is disabled). Allowed to mint a JOIN token for:
    //   - the user's own inbox room (`inbox-<user_id>`),
    //   - any `call-<ulid>` room — the ULID is an unguessable capability handed
    //     out via the callee's inbox; there is no membership row for it, and
    //     voice is MLS/E2EE'd so the room ACL isn't the confidentiality boundary,
    //   - any conversation the user is a current member of (`is_member` covers
    //     groups, DMs, and channels — the latter being the voice-room case).
    if !authorize_room(&state, &authed, &parsed.room, &user_id).await? {
        return Ok(AuthRejection::Forbidden.into_response());
    }

    // Device half: on the signed path it's the header the signature was verified
    // against (gate proved the key registered for THIS device signed the request),
    // so it's trustworthy without re-checking; on the no-auth path take the body.
    let device_id = if authed.is_some() {
        req.headers
            .get("x-pollis-device")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    } else {
        parsed.device_id.clone().unwrap_or_default()
    };
    // `view` is the screenshare-receive variant: no data channel (mirrors
    // pollis-core's old `make_view_token`). The kind also picks the identity's
    // capability prefix — the only structure left in it.
    let kind = crate::participant_id::ParticipantKind::from_wire(parsed.kind.as_deref());
    let can_publish_data = kind != crate::participant_id::ParticipantKind::View;

    // #836: the user and device are encrypted into the identity under a key
    // scoped to the LOGICAL room, so nothing relates this participant to the
    // same user in another room. `device_id` keeps a user's devices distinct
    // (#140) without appearing in the output.
    let identity = crate::participant_id::participant_pseudonym(
        api_secret,
        &parsed.room,
        &user_id,
        &device_id,
        kind,
    );

    // #828: membership was authorized on the LOGICAL room above; what reaches
    // LiveKit is the pseudonym. The room travels inside the JWT grant and
    // `Room::connect` takes no room argument, so the client needs no change and
    // never holds the mapping — which is the point: a mapping shipped in a client
    // binary could be extracted and used to re-link every room to its conversation.
    let wire_room = crate::room_id::room_pseudonym(api_secret, &parsed.room);

    let token = sign_livekit_token(
        api_key,
        api_secret,
        &wire_room,
        &identity,
        can_publish_data,
        crate::util::now_unix(),
    )?;

    Ok(ok_response::<LivekitTokenBody>(LivekitTokenResponse { token, url: url.to_string() }))
}

/// Look up usernames for a batch of resolved user ids, in one query.
///
/// Display names used to ride along in the LiveKit JWT (`name`) and come back
/// off the SFU with each participant; since #836 stopped sending them, this is
/// where the roster and the identity resolver get them instead. Ids are BOUND,
/// never interpolated.
async fn lookup_usernames(
    conn: &Connection,
    user_ids: &[String],
) -> anyhow::Result<std::collections::HashMap<String, String>> {
    let mut out = std::collections::HashMap::new();
    if user_ids.is_empty() {
        return Ok(out);
    }
    let placeholders = std::iter::repeat_n("?", user_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id, username FROM users WHERE id IN ({placeholders})");
    let params: Vec<libsql::Value> = user_ids
        .iter()
        .map(|id| libsql::Value::Text(id.clone()))
        .collect();
    let mut rows = conn.query(&sql, params).await?;
    while let Some(row) = rows.next().await? {
        if let (Ok(id), Ok(name)) = (row.get::<String>(0), row.get::<String>(1)) {
            out.insert(id, name);
        }
    }
    Ok(out)
}

/// Authorize `user_id` to act on `room`, with the rule shared by every LiveKit
/// broker endpoint: your own inbox is always yours; a `call-<ulid>` room is an
/// unguessable capability handed out through an inbox and has no membership row;
/// anything else needs current membership. Only enforced on the signed path,
/// mirroring the other handlers.
async fn authorize_room(
    state: &AppState,
    authed: &Authed,
    room: &str,
    user_id: &str,
) -> Result<bool, AppError> {
    if authed.is_none() {
        return Ok(true);
    }
    if room == format!("inbox-{user_id}") || room.starts_with("call-") {
        return Ok(true);
    }
    let conn = state.db.conn().await?;
    if !is_member(&conn, room, user_id).await? {
        return Ok(false);
    }
    // A block does not delete the DM's membership rows — it resets the
    // blocker's `accepted_at` — so membership alone would keep handing the
    // blocked user a token for the room they were just evicted from, and the
    // eviction would undo itself on their next reconnect. Refusing the token is
    // what makes it stick. One-directional, like the block: the blocker keeps
    // their own access.
    Ok(!dm_peer_blocked(&conn, room, user_id).await?)
}

/// True when another member of the DM `room` has blocked `user_id`.
///
/// Answers `false` for anything that is not a DM — a group room matches no
/// `dm_channel_member` row — so the group and channel paths are unaffected.
async fn dm_peer_blocked(
    conn: &Connection,
    room: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM dm_channel_member peer \
               JOIN user_block b \
                 ON b.blocker_id = peer.user_id AND b.blocked_id = ?2 \
              WHERE peer.dm_channel_id = ?1 AND peer.user_id <> ?2 \
              LIMIT 1",
            libsql::params![room.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Sign an HS256 LiveKit JWT. `can_publish_data` is `false` for the `view`
/// variant. `now` is injected so the claim times are testable. Pure (no I/O) so
/// it's directly unit-testable.
///
/// There is no display-name parameter, deliberately (#836). The `name` claim
/// used to carry the user's Pollis username, which made pseudonymising the
/// identity pointless — the SFU could read the account straight off the
/// participant. Peers resolve display names through
/// [`livekit_identities`] instead, and the claim is sent empty. Removing the
/// argument rather than passing `""` is what stops it coming back.
pub fn sign_livekit_token(
    api_key: &str,
    api_secret: &str,
    room: &str,
    identity: &str,
    can_publish_data: bool,
    now: u64,
) -> anyhow::Result<String> {
    let claims = LiveKitClaims {
        iss: api_key.to_string(),
        sub: identity.to_string(),
        iat: now,
        nbf: now,
        exp: now + LIVEKIT_TOKEN_TTL_SECS,
        name: String::new(),
        video: VideoGrants {
            room: room.to_string(),
            room_join: true,
            can_publish: true,
            can_subscribe: true,
            can_publish_data,
        },
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(api_secret.as_bytes());
    Ok(encode(&header, &claims, &key)?)
}

// ── 1b. LiveKit server (RoomService) API — admin token + Twirp ────────────────
//
// `RoomService/SendData` (fan out a control payload to a room the caller isn't
// joined to) and `RoomService/ListParticipants` (voice roster) each need a
// short-lived **admin** JWT (`roomAdmin`) — signed with the LiveKit API secret.
// On-device that secret was the leak; here the DS signs and makes the Twirp call
// so the client only names a room + payload. Mirrors pollis-core's old
// `livekit/admin_api.rs` (`make_admin_token` / `twirp_base`).

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminGrants {
    room_admin: bool,
    room_list: bool,
    room: String,
}

#[derive(Serialize)]
struct AdminClaims {
    iss: String,
    sub: String,
    iat: u64,
    nbf: u64,
    exp: u64,
    video: AdminGrants,
}

/// Sign a short-lived (`+300s`) HS256 admin JWT scoped to `room`, granting the
/// `RoomService` calls. `now` injected for testability. Pure.
pub fn sign_livekit_admin_token(
    api_key: &str,
    api_secret: &str,
    room: &str,
    now: u64,
) -> anyhow::Result<String> {
    let claims = AdminClaims {
        iss: api_key.to_string(),
        sub: "pollis-ds".to_string(),
        iat: now,
        nbf: now,
        exp: now + 300,
        video: AdminGrants {
            room_admin: true,
            room_list: true,
            room: room.to_string(),
        },
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(api_secret.as_bytes());
    Ok(encode(&header, &claims, &key)?)
}

/// The Twirp server API is a separate HTTPS endpoint from the `wss://` SDK URL.
/// Translate one to the other (`wss`→`https`, `ws`→`http`).
fn twirp_base(livekit_url: &str) -> String {
    if let Some(rest) = livekit_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = livekit_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        livekit_url.to_string()
    }
}

fn bad_gateway(what: impl std::fmt::Display) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(serde_json::json!({ "error": what.to_string() })),
    )
        .into_response()
}

/// Map a failed outbound call to a response (#913).
///
/// A **timeout** is `504 Gateway Timeout`, distinct from the `502` every other
/// upstream failure gets, because the two ask the client for different things: a
/// 502 means the upstream answered and the answer was unusable (retrying is
/// unlikely to help), while a 504 means it never answered in time and the
/// request may well succeed on a retry. Both beat the pre-#913 behaviour, which
/// was to answer nothing at all and hold the handler — and a pooled connection —
/// for as long as the upstream kept the socket open.
fn upstream_error(upstream: crate::util::Upstream, what: &str, e: &reqwest::Error) -> Response {
    if e.is_timeout() {
        tracing::warn!(
            upstream = upstream.name(),
            timeout_secs = upstream.timeout().as_secs(),
            "{what}: upstream timed out"
        );
        return (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "error": format!("{what}: {} timed out", upstream.name()),
                // The client's retry is worth attempting; say so explicitly
                // rather than making it guess from the status alone.
                "retryable": true,
            })),
        )
            .into_response();
    }
    bad_gateway(format!("{what}: {e}"))
}

// ── POST /v1/livekit/send-data ────────────────────────────────────────────────
//
// Authz: an authenticated device is required (`gate`), and the TARGET room must
// be one the signer may reach (this endpoint used to admit any room):
//
//   - the signer's own inbox (`inbox-<signer>`) — always;
//   - another user's inbox (`inbox-<peer>`) — only while the two share a
//     conversation (a DM channel, a group, or a pending group invite from the
//     signer to the peer) AND neither has blocked the other; `call_invite`
//     additionally needs a relationship the peer consented to (an accepted DM or
//     a shared group) so a bare DM request cannot ring a stranger's devices;
//   - a conversation room — only for a current member (`is_member`).
//
// The payload's `type` must be one a CLIENT legitimately originates
// ([`ClientPayloadKind`]); `enrollment_requested` is emitted by the DS itself
// from `bootstrap::enrollment_request` and is refused here, so no client can
// raise the account-takeover approval prompt on another device.
//
// Identity is never taken from the body. Every identity-bearing key a client
// might send (`sender_id`, `caller_id`, `*_username`, …) is STRIPPED, and for
// the private-inbox kinds that legitimately name their actor the DS stamps the
// VERIFIED signer back in (`sender_id` + `sender_username`, resolved from
// `users`), so a recipient's "Incoming call from X" can only ever name the
// account that actually signed the request. Shared-room targets get nothing
// stamped — §5 metadata minimization keeps those broadcasts routing-only.

/// The control-payload types a client may fan out through `/v1/livekit/send-data`.
///
/// An allowlist rather than a denylist: a `type` the client has no business
/// publishing — `enrollment_requested` most of all — is unrepresentable here and
/// is refused before anything is signed. Mirrors the emitters in pollis-core
/// (`commands/livekit/publish.rs`, `livekit_stub.rs`, `livekit_signalling.rs`,
/// and the inline `json!` pings in `dm.rs` / `groups/*` / `messages/send.rs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientPayloadKind {
    NewMessage,
    EditedMessage,
    DeletedMessage,
    MembershipChanged,
    RosterChanged,
    JoinRequestsChanged,
    MemberRoleChanged,
    DmCreated,
    AllMention,
    UserMention,
    DeviceRevoked,
    CallInvite,
    CallCanceled,
}

impl ClientPayloadKind {
    /// Parse the payload's `type` discriminant. `None` for anything a client may
    /// not publish — unknown strings and the DS-originated `enrollment_requested`
    /// alike.
    pub fn from_wire(kind: &str) -> Option<Self> {
        Some(match kind {
            "new_message" => Self::NewMessage,
            "edited_message" => Self::EditedMessage,
            "deleted_message" => Self::DeletedMessage,
            "membership_changed" => Self::MembershipChanged,
            "roster_changed" => Self::RosterChanged,
            "join_requests_changed" => Self::JoinRequestsChanged,
            "member_role_changed" => Self::MemberRoleChanged,
            "dm_created" => Self::DmCreated,
            "all_mention" => Self::AllMention,
            "user_mention" => Self::UserMention,
            "device_revoked" => Self::DeviceRevoked,
            "call_invite" => Self::CallInvite,
            "call_canceled" => Self::CallCanceled,
            _ => return None,
        })
    }

    /// The private-inbox pings whose recipient renders the actor's name (a DM
    /// request, a group invite, an @mention, an incoming call). Only these get
    /// the verified sender stamped in — everything else stays identity-free.
    fn names_its_sender(self) -> bool {
        matches!(
            self,
            Self::DmCreated
                | Self::MembershipChanged
                | Self::AllMention
                | Self::UserMention
                | Self::CallInvite
        )
    }

    /// Whether reaching a PEER's inbox with this kind needs a relationship the
    /// peer has consented to (an accepted DM or a shared group) rather than any
    /// shared conversation. Ringing someone's devices is louder than a badge.
    fn needs_established_relationship(self) -> bool {
        matches!(self, Self::CallInvite)
    }
}

/// Where a send-data request is addressed, classified from the logical room name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendTarget {
    /// The signer's own `inbox-<signer>` room (every device of the same user).
    OwnInbox,
    /// Another user's `inbox-<peer>` room.
    PeerInbox(String),
    /// A conversation room (group / DM channel / channel id).
    Conversation(String),
}

impl SendTarget {
    pub fn classify(room: &str, user_id: &str) -> Self {
        match room.strip_prefix("inbox-") {
            Some(peer) if peer == user_id => Self::OwnInbox,
            Some(peer) => Self::PeerInbox(peer.to_string()),
            None => Self::Conversation(room.to_string()),
        }
    }

    fn is_inbox(&self) -> bool {
        matches!(self, Self::OwnInbox | Self::PeerInbox(_))
    }
}

/// Every payload key that could carry an identity a recipient might render or
/// act on. Stripped from EVERY client payload before fan-out, whatever the type,
/// so a client cannot smuggle an actor under a legacy or unexpected key.
const IDENTITY_KEYS: &[&str] = &[
    "sender_id",
    "sender_username",
    "caller_id",
    "caller_username",
    "inviter_username",
    "user_id",
    "username",
    "display_name",
    "deleted_by",
    "group_name",
];

/// The verified actor, as the DS will stamp it.
pub struct StampedSender<'a> {
    pub user_id: &'a str,
    /// From `users.username`; `None` if the row is missing (degrades the alert
    /// to a generic name, never to a client-chosen one).
    pub username: Option<&'a str>,
    /// `groups.name` for the `group_id` a `membership_changed` invite names.
    pub group_name: Option<&'a str>,
}

/// Rewrite a client payload so nothing in it can misattribute the actor. Pure.
///
/// Strips [`IDENTITY_KEYS`]; then, only for a private-inbox target and a kind
/// that names its sender, stamps the verified signer as `sender_id` /
/// `sender_username` — plus the per-kind legacy keys shipped clients already
/// read (`caller_id` / `caller_username` on `call_invite`, `inviter_username` /
/// `group_name` on `membership_changed`), all with the SAME verified values, so
/// an older renderer sees the true caller rather than nothing at all.
pub fn sanitize_client_payload(
    payload: &serde_json::Value,
    kind: ClientPayloadKind,
    target: &SendTarget,
    sender: &StampedSender<'_>,
) -> serde_json::Value {
    let mut obj = payload.as_object().cloned().unwrap_or_default();
    for key in IDENTITY_KEYS {
        obj.remove(*key);
    }
    if target.is_inbox() && kind.names_its_sender() {
        obj.insert("sender_id".into(), serde_json::Value::from(sender.user_id));
        obj.insert("sender_username".into(), serde_json::Value::from(sender.username));
        match kind {
            ClientPayloadKind::CallInvite => {
                obj.insert("caller_id".into(), serde_json::Value::from(sender.user_id));
                obj.insert(
                    "caller_username".into(),
                    serde_json::Value::from(sender.username.unwrap_or(sender.user_id)),
                );
            }
            ClientPayloadKind::MembershipChanged => {
                obj.insert("inviter_username".into(), serde_json::Value::from(sender.username));
                obj.insert("group_name".into(), serde_json::Value::from(sender.group_name));
            }
            _ => {}
        }
    }
    serde_json::Value::Object(obj)
}

/// Whether `user_id` may push a control payload into `peer`'s inbox: the two
/// must share a conversation and neither may have blocked the other. With
/// `established`, only a DM the peer has accepted or a shared group counts — a
/// pending DM request or a pending invite is not enough.
async fn may_reach_inbox(
    conn: &Connection,
    user_id: &str,
    peer: &str,
    established: bool,
) -> anyhow::Result<bool> {
    if crate::profile::is_blocked_either_way(conn, user_id, peer).await? {
        return Ok(false);
    }
    let sql = if established {
        "SELECT 1 WHERE \
            EXISTS (SELECT 1 FROM dm_channel_member me \
                    JOIN dm_channel_member them ON them.dm_channel_id = me.dm_channel_id \
                    WHERE me.user_id = ?1 AND them.user_id = ?2 \
                      AND them.accepted_at IS NOT NULL) \
         OR EXISTS (SELECT 1 FROM group_member me \
                    JOIN group_member them ON them.group_id = me.group_id \
                    WHERE me.user_id = ?1 AND them.user_id = ?2) \
         LIMIT 1"
    } else {
        "SELECT 1 WHERE \
            EXISTS (SELECT 1 FROM dm_channel_member me \
                    JOIN dm_channel_member them ON them.dm_channel_id = me.dm_channel_id \
                    WHERE me.user_id = ?1 AND them.user_id = ?2) \
         OR EXISTS (SELECT 1 FROM group_member me \
                    JOIN group_member them ON them.group_id = me.group_id \
                    WHERE me.user_id = ?1 AND them.user_id = ?2) \
         OR EXISTS (SELECT 1 FROM group_invite \
                    WHERE inviter_id = ?1 AND invitee_id = ?2 AND status = 'pending') \
         LIMIT 1"
    };
    let mut rows = conn
        .query(sql, libsql::params![user_id.to_string(), peer.to_string()])
        .await?;
    Ok(rows.next().await?.is_some())
}

/// `groups.name` for `group_id`, if the row exists. Bound, never interpolated.
async fn lookup_group_name(conn: &Connection, group_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT name FROM groups WHERE id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<String>(0).ok(),
        None => None,
    })
}

/// POST /v1/livekit/send-data — fan out a control payload to a LiveKit room via
/// server-side `RoomService/SendData`. A 404 (room currently has no
/// participants) is success, mirroring the client's fire-and-forget semantics.
///
/// Refuses (403) a target the signer may not reach and a payload `type` a client
/// may not originate; strips and re-stamps identity before anything is signed.
/// See the module comment above for the full rule.
pub async fn livekit_send_data(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LivekitSendDataBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    if parsed.room.trim().is_empty() {
        return Ok(bad_request("room required"));
    }
    let kind = match parsed
        .payload
        .get("type")
        .and_then(|v| v.as_str())
        .and_then(ClientPayloadKind::from_wire)
    {
        Some(k) => k,
        // Includes `enrollment_requested`: DS-originated, never client-publishable.
        None => return Ok(AuthRejection::Forbidden.into_response()),
    };
    let user_id = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let target = SendTarget::classify(&parsed.room, &user_id);

    let conn = state.db.conn().await?;
    // Room authz — only on the signed path (mirrors `authorize_room`, which the
    // other LiveKit handlers skip when auth is disabled). Checked BEFORE the
    // secrets gate so a refused target is a 403 whether or not LiveKit is wired.
    if authed.is_some() {
        let allowed = match &target {
            SendTarget::OwnInbox => true,
            SendTarget::PeerInbox(peer) => {
                may_reach_inbox(&conn, &user_id, peer, kind.needs_established_relationship())
                    .await?
            }
            SendTarget::Conversation(room) => is_member(&conn, room, &user_id).await?,
        };
        if !allowed {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    // Resolve the verified actor's display data server-side; the client's copy
    // of these strings is discarded along with every other identity key.
    let username = lookup_usernames(&conn, std::slice::from_ref(&user_id))
        .await?
        .remove(&user_id);
    let group_name = match (kind, parsed.payload.get("group_id").and_then(|v| v.as_str())) {
        (ClientPayloadKind::MembershipChanged, Some(gid)) => lookup_group_name(&conn, gid).await?,
        _ => None,
    };
    drop(conn);
    let payload = sanitize_client_payload(
        &parsed.payload,
        kind,
        &target,
        &StampedSender {
            user_id: &user_id,
            username: username.as_deref(),
            group_name: group_name.as_deref(),
        },
    );

    // Preserve the explicit "not configured" response for the client endpoint;
    // the shared sender collapses a missing broker into a plain error string.
    if state.broker.livekit_ready().is_none() {
        return Ok(not_configured("livekit"));
    }
    match room_send_data(&state, &parsed.room, &payload).await {
        Ok(()) => Ok(ok_response::<LivekitSendDataBody>(LivekitSendDataResponse { ok: true })),
        Err(e) => Ok(bad_gateway(e)),
    }
}

/// Fan out a JSON control payload to a LiveKit room via server-side
/// `RoomService/SendData`. A 404 (room currently has no participants) is
/// treated as success, matching the fire-and-forget nudge semantics.
///
/// Shared by the client-facing `/v1/livekit/send-data` endpoint and by
/// **server-side emitters** — notably the enrollment-request inbox
/// notification (`bootstrap::enrollment_request`), which the requesting device
/// CANNOT send itself: it is pre-enrollment, so its `local_db` is closed and it
/// has no MLS signing credential, and a client-side device-signed send-data
/// fails with "not signed in for DS request signing". The DS holds the LiveKit
/// admin secret, so it emits that nudge here. Returns `Err(reason)` on any
/// failure; fire-and-forget callers log and move on.
pub async fn room_send_data(
    state: &AppState,
    room: &str,
    payload: &serde_json::Value,
) -> Result<(), String> {
    let (api_key, api_secret, url) = state
        .broker
        .livekit_ready()
        .ok_or_else(|| "livekit not configured".to_string())?;

    let raw = serde_json::to_vec(payload).unwrap_or_default();
    let data_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(&raw)
    };
    // #828: map here, at the chokepoint, so EVERY caller — including server-side
    // emitters like `bootstrap::enrollment_request` — keeps passing the logical
    // room and none of them can forget. The pseudonym is the only form that ever
    // reaches LiveKit.
    let wire_room = crate::room_id::room_pseudonym(api_secret, room);
    let token = sign_livekit_admin_token(api_key, api_secret, &wire_room, crate::util::now_unix())
        .map_err(|e| format!("sign admin token: {e}"))?;
    let endpoint = format!("{}/twirp/livekit.RoomService/SendData", twirp_base(url));

    let sent = crate::util::http_post(crate::util::Upstream::LiveKit, &endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "room": wire_room, "data": data_b64, "kind": "RELIABLE" }))
        .send()
        .await;

    match sent {
        Ok(r) if r.status().is_success() || r.status() == StatusCode::NOT_FOUND => Ok(()),
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            Err(format!("SendData {status}: {text}"))
        }
        // Callers of this one are fire-and-forget: they log the reason and move
        // on, so a timeout needs no distinct status — what #913 changes here is
        // that the nudge now GIVES UP instead of holding the caller's handler
        // open indefinitely behind a LiveKit that stopped answering.
        Err(e) if e.is_timeout() => Err(format!(
            "SendData: livekit timed out after {}s",
            crate::util::Upstream::LiveKit.timeout().as_secs()
        )),
        Err(e) => Err(format!("SendData: {e}")),
    }
}

// ── 1c. RoomService/RemoveParticipant — evicting who lost access ─────────────
//
// A LiveKit token is checked ONCE, when the participant joins. Nothing about
// losing access afterwards reaches the SFU on its own: a member removed from a
// group, one who left, a blocked user, or a revoked device all keep the realtime
// connection they already hold until they happen to disconnect. Refusing them a
// NEW token (which `authorize_room` already does) does not close a session that
// is already open — only `RoomService/RemoveParticipant` does.
//
// Two halves make an eviction complete, and both live here so no call site can
// implement half of it:
//
//   - the TTL ([`LIVEKIT_TOKEN_TTL_SECS`]) bounds how long a *stale* token stays
//     usable, i.e. how long a client that reconnects can re-enter a room it has
//     since lost. `realtime.rs` re-mints on every reconnect, so shortening it
//     costs a client nothing;
//   - the kick below ends the session that is open RIGHT NOW.
//
// The identity handed to LiveKit is the per-room pseudonym (#836), derived from
// `(room, user, device, kind)` — so evicting a user means evicting every one of
// their devices in every capability, which is what [`participant_identities`]
// enumerates.

/// Realtime/voice participant token lifetime, in seconds.
///
/// Deliberately short (15 min, was 1 h). Membership is checked only when the
/// token is MINTED, so the TTL is exactly the window in which a token issued
/// before a removal can still be redeemed at the SFU. The client re-mints on
/// every reconnect (`pollis-core` `commands/livekit/realtime.rs`), and LiveKit
/// validates the token at join rather than continuously, so a live connection is
/// never dropped by this expiring — shortening it costs an established session
/// nothing and only narrows the re-entry window.
pub const LIVEKIT_TOKEN_TTL_SECS: u64 = 15 * 60;

/// Evict one participant identity from one LiveKit room via server-side
/// `RoomService/RemoveParticipant`.
///
/// A 404 is success: LiveKit answers that way when the room does not exist or
/// the identity is not in it, which for an eviction is the desired end state —
/// and the common case, since most of a user's `(device, kind)` identities are
/// not connected at any given moment.
pub async fn room_remove_participant(
    state: &AppState,
    room: &str,
    identity: &str,
) -> Result<(), String> {
    let (api_key, api_secret, url) = state
        .broker
        .livekit_ready()
        .ok_or_else(|| "livekit not configured".to_string())?;

    // #828: LiveKit is addressed by the room pseudonym here for the same reason
    // `room_send_data` maps at the chokepoint — the logical name never leaves
    // the DS.
    let wire_room = crate::room_id::room_pseudonym(api_secret, room);
    let token = sign_livekit_admin_token(api_key, api_secret, &wire_room, crate::util::now_unix())
        .map_err(|e| format!("sign admin token: {e}"))?;
    let endpoint = format!(
        "{}/twirp/livekit.RoomService/RemoveParticipant",
        twirp_base(url)
    );

    let sent = crate::util::http_post(crate::util::Upstream::LiveKit, &endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "room": wire_room, "identity": identity }))
        .send()
        .await;

    match sent {
        Ok(r) if r.status().is_success() || r.status() == StatusCode::NOT_FOUND => Ok(()),
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            Err(format!("RemoveParticipant {status}: {text}"))
        }
        Err(e) if e.is_timeout() => Err(format!(
            "RemoveParticipant: livekit timed out after {}s",
            crate::util::Upstream::LiveKit.timeout().as_secs()
        )),
        Err(e) => Err(format!("RemoveParticipant: {e}")),
    }
}

/// Every LiveKit identity `user_id` can be holding in `logical_room`.
///
/// One per `(device, kind)` pair, because that is exactly what
/// [`crate::participant_id::participant_pseudonym`] keys on: a user connected
/// from two devices is two participants, and a device in a voice channel is a
/// different participant from the same device's realtime connection. Missing one
/// of them leaves that session live, so this enumeration is the whole
/// correctness of an eviction.
///
/// Takes the devices explicitly rather than reading them, because the two
/// callers want different sets: losing membership evicts EVERY device the user
/// has, while revoking one device must leave that user's other sessions alone.
pub fn participant_identities(
    api_secret: &str,
    logical_room: &str,
    user_id: &str,
    device_ids: &[String],
) -> Vec<String> {
    use crate::participant_id::ParticipantKind;

    let mut out = Vec::with_capacity(device_ids.len() * 3);
    for device in device_ids {
        for kind in [
            ParticipantKind::Realtime,
            ParticipantKind::Voice,
            ParticipantKind::View,
        ] {
            out.push(crate::participant_id::participant_pseudonym(
                api_secret,
                logical_room,
                user_id,
                device,
                kind,
            ));
        }
    }
    out
}

/// The device ids registered to `user_id`, revoked ones included.
///
/// Revoked devices are deliberately in: a revocation is one of the events that
/// MUST evict, and the tombstone is written before the eviction runs, so
/// filtering on `revoked_at IS NULL` would skip the very device being kicked.
pub async fn user_device_ids(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT device_id FROM user_device WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Ok(id) = row.get::<String>(0) {
            out.push(id);
        }
    }
    Ok(out)
}

/// The realtime rooms that belong to a group: the group's own room (one MLS
/// group per group, and the realtime connection is keyed on it) plus every
/// channel in it, because a voice/screenshare participant joins the CHANNEL as
/// the room (`commands/voice/lifecycle.rs`). Evicting only the group room would
/// leave an ex-member sitting in the voice channel.
pub async fn group_rooms(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    let mut out = vec![group_id.to_string()];
    let mut rows = conn
        .query(
            "SELECT id FROM channels WHERE group_id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        if let Ok(id) = row.get::<String>(0) {
            out.push(id);
        }
    }
    Ok(out)
}

/// The DM rooms `a` and `b` share. A block costs the blocked user their place
/// in exactly these rooms (see [`dm_peer_blocked`]).
pub async fn shared_dm_rooms(
    conn: &Connection,
    a: &str,
    b: &str,
) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT me.dm_channel_id FROM dm_channel_member me \
               JOIN dm_channel_member them ON them.dm_channel_id = me.dm_channel_id \
              WHERE me.user_id = ?1 AND them.user_id = ?2",
            libsql::params![a.to_string(), b.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        if let Ok(id) = row.get::<String>(0) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Every room `user_id` may currently be connected to: their own inbox, each
/// group they are in, each channel of those groups, and each DM.
///
/// `call-<ulid>` rooms are deliberately absent — they have no membership row at
/// all (the ULID is the capability) and nothing in the DB can enumerate them.
pub async fn user_rooms(conn: &Connection, user_id: &str) -> anyhow::Result<Vec<String>> {
    let mut out = vec![format!("inbox-{user_id}")];
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_member WHERE user_id = ?1 \
             UNION SELECT c.id FROM channels c \
                     JOIN group_member gm ON gm.group_id = c.group_id \
                    WHERE gm.user_id = ?1 \
             UNION SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        if let Ok(id) = row.get::<String>(0) {
            out.push(id);
        }
    }
    Ok(out)
}

/// Kick `user_id` out of every room in `rooms` — every device, every capability.
///
/// This is the membership-loss eviction (removed, left, blocked): the user has
/// no business in the room from any device, so every device they own is kicked,
/// plus the legacy no-device identity (`participant_pseudonym` accepts `""`, and
/// a client old enough to hold one is exactly the client that will not notice
/// being un-authorized).
pub async fn evict_user_from_rooms(state: &AppState, rooms: &[String], user_id: &str) {
    let mut device_ids = match state.db.conn().await {
        Ok(conn) => user_device_ids(&conn, user_id).await.unwrap_or_default(),
        Err(e) => {
            tracing::warn!("eviction device lookup failed: {e}");
            Vec::new()
        }
    };
    device_ids.push(String::new());
    evict_identities(state, rooms, user_id, &device_ids).await;
}

/// Kick ONE device of `user_id` out of every room in `rooms`, leaving that
/// user's other sessions connected. The revoked-device case: the account keeps
/// its access, this device does not.
pub async fn evict_device_from_rooms(
    state: &AppState,
    rooms: &[String],
    user_id: &str,
    device_id: &str,
) {
    evict_identities(state, rooms, user_id, &[device_id.to_string()]).await;
}

/// Fire-and-forget in the same sense as [`room_send_data`]: a LiveKit that is
/// down or unconfigured must not fail the write that already committed — the
/// user IS removed either way, and the stale session then dies at its next
/// reconnect, when [`authorize_room`] refuses it a token. Failures are logged,
/// never returned.
///
/// The calls run CONCURRENTLY and are awaited before the handler answers. There
/// are `3 × devices` of them per room and each carries the 5s LiveKit deadline,
/// so running them in sequence would put a stalled SFU on the critical path of a
/// `POST /v1/members/remove` for a minute; concurrently the worst case is one
/// deadline.
async fn evict_identities(
    state: &AppState,
    rooms: &[String],
    user_id: &str,
    device_ids: &[String],
) {
    let Some((_, api_secret, _)) = state.broker.livekit_ready() else {
        return;
    };
    if rooms.is_empty() || device_ids.is_empty() {
        return;
    }

    let mut tasks = tokio::task::JoinSet::new();
    for room in rooms {
        for identity in participant_identities(api_secret, room, user_id, device_ids) {
            let state = state.clone();
            let room = room.clone();
            tasks.spawn(async move {
                if let Err(e) = room_remove_participant(&state, &room, &identity).await {
                    // The identity is a pseudonym and the room is logical, so
                    // this names what failed without logging either in a form
                    // that links a user to a conversation.
                    tracing::warn!("RemoveParticipant failed: {e}");
                }
            });
        }
    }
    while tasks.join_next().await.is_some() {}
}

// ── POST /v1/livekit/participants ─────────────────────────────────────────────

/// POST /v1/livekit/participants — return the voice roster for `room` via
/// server-side `RoomService/ListParticipants`. Same room authz as the token
/// endpoint (own inbox always ok, else current membership). Internal
/// participants (`server` / `pollis-*` / `:view`) are filtered out. A 404 (room
/// doesn't exist yet) returns an empty roster.
pub async fn livekit_participants(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LivekitParticipantsBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    if parsed.room.trim().is_empty() {
        return Ok(bad_request("room required"));
    }

    let (api_key, api_secret, url) = match state.broker.livekit_ready() {
        Some(t) => t,
        None => return Ok(not_configured("livekit")),
    };

    let user_id = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    // Roster is a read — gate it to members (own inbox is always ok), mirroring
    // the token endpoint. Only enforced on the signed path.
    if authed.is_some() {
        let inbox = format!("inbox-{user_id}");
        if parsed.room != inbox {
            let conn = state.db.conn().await?;
            if !is_member(&conn, &parsed.room, &user_id).await? {
                return Ok(AuthRejection::Forbidden.into_response());
            }
        }
    }

    // #828: authorize the logical room, address LiveKit by its pseudonym.
    let wire_room = crate::room_id::room_pseudonym(api_secret, &parsed.room);
    let token = sign_livekit_admin_token(api_key, api_secret, &wire_room, crate::util::now_unix())?;
    let endpoint = format!("{}/twirp/livekit.RoomService/ListParticipants", twirp_base(url));

    let listed = crate::util::http_post(crate::util::Upstream::LiveKit, &endpoint)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "room": wire_room }))
        .send()
        .await;

    #[derive(Deserialize)]
    struct RsResp {
        #[serde(default)]
        participants: Vec<RsParticipant>,
    }
    #[derive(Deserialize)]
    struct RsParticipant {
        #[serde(default)]
        identity: String,
    }

    match listed {
        Ok(r) if r.status() == StatusCode::NOT_FOUND => Ok(ok_response::<
            LivekitParticipantsBody,
        >(
            LivekitParticipantsResponse { participants: Vec::new() },
        )),
        Ok(r) if r.status().is_success() => {
            let parsed_resp: RsResp = match r.json().await {
                Ok(p) => p,
                Err(e) => return Ok(bad_gateway(format!("ListParticipants decode: {e}"))),
            };
            // #836: identities off the SFU are opaque, so the filtering that used
            // to read them as strings (`ends_with(":view")`, the internal names)
            // now falls out of decryption: anything that doesn't resolve to a
            // real user under THIS room's key is not a roster entry, and the
            // `view` kind is a screenshare receiver rather than a person.
            let resolved: Vec<(String, String)> = parsed_resp
                .participants
                .into_iter()
                .filter_map(|p| {
                    let (user_id, kind) = crate::participant_id::resolve_participant(
                        api_secret,
                        &parsed.room,
                        &p.identity,
                    )?;
                    (kind != crate::participant_id::ParticipantKind::View)
                        .then_some((p.identity, user_id))
                })
                .collect();

            // Display names no longer ride in on the LiveKit `name` claim — the
            // roster is where they get re-attached, from the DS's own DB.
            let names = {
                let ids: Vec<String> = resolved.iter().map(|(_, u)| u.clone()).collect();
                let conn = state.db.conn().await?;
                lookup_usernames(&conn, &ids).await.unwrap_or_default()
            };
            let participants: Vec<ResolvedIdentity> = resolved
                .into_iter()
                .map(|(identity, uid)| {
                    let name = names.get(&uid).cloned().unwrap_or_else(|| uid.clone());
                    // No `kind` on the roster — it is already filtered to real
                    // voice participants, and the field is skipped when absent
                    // so these bytes are unchanged.
                    ResolvedIdentity { identity, user_id: uid, name, kind: None }
                })
                .collect();
            Ok(ok_response::<LivekitParticipantsBody>(LivekitParticipantsResponse { participants }))
        }
        Ok(r) => {
            let status = r.status();
            let text = r.text().await.unwrap_or_default();
            Ok(bad_gateway(format!("ListParticipants {status}: {text}")))
        }
        Err(e) => Ok(upstream_error(
            crate::util::Upstream::LiveKit,
            "ListParticipants",
            &e,
        )),
    }
}

// ── POST /v1/livekit/identities ───────────────────────────────────────────────

/// POST /v1/livekit/identities — resolve opaque LiveKit participant identities
/// back to Pollis users (#836).
///
/// A client learns peer identities from the LiveKit event stream, where they are
/// per-room pseudonyms it has no key for. This is the resolver. Same room authz
/// as minting a token, so it discloses nothing the caller could not learn by
/// joining the room and reading the roster.
///
/// Unresolvable entries are omitted rather than erroring: an internal
/// participant (`pollis-ds`), an identity from another room, and a hostile SFU's
/// forgery are all simply "not a Pollis user here", and a roster with one
/// unattributed tile is a better outcome than a failed batch.
pub async fn livekit_identities(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LivekitIdentitiesBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    if parsed.room.trim().is_empty() {
        return Ok(bad_request("room required"));
    }

    let (_, api_secret, _) = match state.broker.livekit_ready() {
        Some(t) => t,
        None => return Ok(not_configured("livekit")),
    };

    let user_id = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    if !authorize_room(&state, &authed, &parsed.room, &user_id).await? {
        return Ok(AuthRejection::Forbidden.into_response());
    }

    let resolved: Vec<(String, String, crate::participant_id::ParticipantKind)> = parsed
        .identities
        .iter()
        .filter_map(|identity| {
            let (uid, kind) =
                crate::participant_id::resolve_participant(api_secret, &parsed.room, identity)?;
            Some((identity.clone(), uid, kind))
        })
        .collect();

    let names = {
        let ids: Vec<String> = resolved.iter().map(|(_, u, _)| u.clone()).collect();
        let conn = state.db.conn().await?;
        lookup_usernames(&conn, &ids).await.unwrap_or_default()
    };

    let identities: Vec<ResolvedIdentity> = resolved
        .into_iter()
        .map(|(identity, uid, kind)| {
            let name = names.get(&uid).cloned().unwrap_or_else(|| uid.clone());
            ResolvedIdentity {
                identity,
                user_id: uid,
                name,
                kind: Some(
                    match kind {
                        crate::participant_id::ParticipantKind::Voice => "voice",
                        crate::participant_id::ParticipantKind::View => "view",
                        crate::participant_id::ParticipantKind::Realtime => "realtime",
                    }
                    .to_string(),
                ),
            }
        })
        .collect();

    Ok(ok_response::<LivekitIdentitiesBody>(LivekitIdentitiesResponse { identities }))
}

// #987 deleted `POST /v1/turso/token` here.
//
// It minted a short-TTL, READ-ONLY Turso token so the client would stop
// shipping a long-lived one in its bundle (#393) — the right move at the time,
// and the wrong shape in the end: short-TTL or not, the token was whole-DATABASE
// and read-only was its only scope, so any authenticated device could read every
// row in the deployment for its lifetime. That is the residual #917 named and
// could not close.
//
// #987 closed it by removing the client's database access entirely rather than
// by shortening the credential's life. With no client connection left, an
// endpoint that hands out database credentials is pure attack surface — so it
// goes, along with `TURSO_PLATFORM_TOKEN` / `TURSO_ORG` / `TURSO_DB`, which the
// DS needed for nothing else. (`TURSO_URL` / `TURSO_TOKEN` /
// `TURSO_ADMIN_TOKEN` / `LOG_DB_ADMIN_TOKEN` are UNRELATED and still required:
// they are the DS's own data-plane connection and the migration runner's.)

// ── 2. POST /v1/r2/presign ───────────────────────────────────────────────────

/// Default presigned-URL lifetime, in seconds.
const PRESIGN_EXPIRES_SECS: u64 = 900;

/// The longest key the bucket will ever be asked to sign.
const R2_KEY_MAX_LEN: usize = 512;

/// The object families the bucket holds. Nothing else is signable — see
/// [`parse_r2_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R2Family {
    /// `media/<hex64>.enc` (and the legacy `media/<hex64>/<name>`) — convergent,
    /// reference-counted attachment ciphertext.
    Media,
    /// `emoji/<hex64>.<ext>` — unencrypted custom-emoji images (#848).
    Emoji,
    /// `avatars/<user_id>/<hex64>.<ext>` (and the legacy `avatars/<user_id>`).
    Avatar,
    /// `group-icons/<group_id>/<hex64>.<ext>` (and legacy `group-icons/<id>/…`).
    GroupIcon,
}

/// A key the DS is willing to sign, decomposed into the facts the gates need.
#[derive(Debug, Clone, Copy)]
pub struct R2Key<'a> {
    pub family: R2Family,
    /// The user or group the prefix names, for the two owned families.
    pub owner: Option<&'a str>,
    /// The content hash this key names, for the two SHARED families — the value
    /// the reference gate is asked about. Present for legacy shapes too: an old
    /// object is every bit as shared as a new one, so it gets the same
    /// protection.
    pub content_hash: Option<&'a str>,
    /// True when the key is CONTENT-ADDRESSED — its name is a 64-hex digest of
    /// the bytes, which is what every key a current client writes looks like.
    /// Only these are writable; a legacy mutable key stays readable forever and
    /// writable never.
    pub content_addressed: bool,
}

/// A lowercase 64-char hex digest, the shape every content-addressed key uses.
fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// A filename's stem — `<stem>.<ext>`, or the whole name when it has no
/// extension. For the content-addressed families the stem IS the hash.
fn stem_of(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, _ext)) => stem,
        None => name,
    }
}

/// Parse an R2 key into the family it belongs to, or `None` if the DS will not
/// sign anything for it.
///
/// WHY AN ALLOW-LIST. The presign endpoint hands out a credential-free URL for
/// whatever key it is given. With no restriction, an authenticated device could
/// name any object in the bucket — including one belonging to a different
/// product surface, a key with `..` in it, or a key it invented purely to park
/// bytes under. The bucket holds exactly four families of object and every one
/// of them is written by code in this repository, so the set of writable shapes
/// is knowable and small; anything outside it is a request the product never
/// makes.
///
/// Segment hygiene is part of the same job: no empty segment, no `.`/`..`, and
/// a conservative character set, so a key can neither traverse nor smuggle a
/// query string or a signed-header separator into the canonical request.
pub fn parse_r2_key(key: &str) -> Option<R2Key<'_>> {
    if key.is_empty() || key.len() > R2_KEY_MAX_LEN {
        return None;
    }
    let segments: Vec<&str> = key.split('/').collect();
    for segment in &segments {
        if segment.is_empty() || *segment == "." || *segment == ".." {
            return None;
        }
        if !segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'~'))
        {
            return None;
        }
    }
    match segments.as_slice() {
        // `media/<hex64>.enc` — today's shape.
        ["media", name] => Some(R2Key {
            family: R2Family::Media,
            owner: None,
            content_hash: Some(stem_of(name)),
            content_addressed: is_hex64(stem_of(name)),
        }),
        // `media/<hex64>/<name>.enc` — pre-#762, when the filename rode along.
        ["media", hash, ..] => Some(R2Key {
            family: R2Family::Media,
            owner: None,
            content_hash: Some(hash),
            content_addressed: false,
        }),
        ["emoji", name] => Some(R2Key {
            family: R2Family::Emoji,
            owner: None,
            content_hash: Some(stem_of(name)),
            content_addressed: is_hex64(stem_of(name)),
        }),
        // `avatars/<user_id>` is the legacy mutable key; the second segment is
        // the content-addressed replacement (#874).
        ["avatars", user_id] => Some(R2Key {
            family: R2Family::Avatar,
            owner: Some(user_id),
            content_hash: None,
            content_addressed: false,
        }),
        ["avatars", user_id, name] => Some(R2Key {
            family: R2Family::Avatar,
            owner: Some(user_id),
            content_hash: None,
            content_addressed: is_hex64(stem_of(name)),
        }),
        // `group-icons/<id>/<ts>-<name>` is the legacy two-part filename; a
        // single content-addressed segment is the current one.
        ["group-icons", group_id, rest @ ..] if !rest.is_empty() => Some(R2Key {
            family: R2Family::GroupIcon,
            owner: Some(group_id),
            content_hash: None,
            content_addressed: rest.len() == 1 && is_hex64(stem_of(rest[0])),
        }),
        _ => None,
    }
}

/// The byte ceiling a PUT of this family may declare.
fn put_max_bytes(family: R2Family) -> u64 {
    match family {
        R2Family::Media => R2_MEDIA_MAX_BYTES,
        R2Family::Emoji => crate::emoji::EMOJI_MAX_BYTES,
        R2Family::Avatar | R2Family::GroupIcon => R2_PUBLIC_IMAGE_MAX_BYTES,
    }
}

/// POST /v1/r2/presign — return a SigV4 presigned URL for a GET, PUT or DELETE
/// against the configured R2 bucket. Requires an authenticated device (when auth
/// is enforced, [`gate_and_parse`] rejects an unsigned request with 401).
///
/// There is no per-object READ authz and there does not need to be — see the
/// module docs: the bucket holds convergently-encrypted ciphertext plus public
/// decoration, so a `get` presign discloses nothing a member did not already
/// hold the key for. WRITES are a different question, and the gates are:
///
///   * **The key must be one the product writes** ([`parse_r2_key`]). An
///     arbitrary key is an arbitrary object, and an arbitrary object is free
///     storage on someone else's bucket.
///   * **A PUT must be content-addressed.** A legacy mutable key stays readable
///     but is never writable again: whoever can overwrite `avatars/<uid>`
///     replaces that user's avatar for everyone, and nothing about the key says
///     what the bytes should be.
///   * **A PUT must declare its exact, bounded length.** Without a signed
///     `content-length` the URL is permission to write an object of ANY size;
///     the DS's own size checks at registration are then a promise the client
///     makes about bytes only R2 ever sees. Signing the length moves the check
///     to the one party that counts them.
///   * **A PUT or DELETE of an owned object needs the owner.** Only a user may
///     write or delete under their own `avatars/` prefix, and only an admin of
///     the group under its `group-icons/` prefix.
///   * **A PUT or DELETE of a shared object needs it unreferenced** (#690, #848)
///     — the integrity rule the module docs set out, now reached for the
///     `media/<hex64>.enc` key shape too, which the old hash extractor silently
///     missed (it returned `"<hash>.enc"`, matching no stored content hash, so
///     the gate never fired for any object written since #762).
pub async fn r2_presign(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<R2PresignBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    let http_method = match parsed.operation.as_str() {
        "get" => "GET",
        "put" => "PUT",
        "delete" => "DELETE",
        _ => return Ok(bad_request("operation must be \"get\", \"put\", or \"delete\"")),
    };

    let Some(object) = parse_r2_key(&parsed.key) else {
        return Ok(bad_request("key is not an object this service stores"));
    };

    let (endpoint, bucket, access_key, secret_key) = match state.broker.r2_ready() {
        Some(t) => t,
        None => return Ok(not_configured("r2")),
    };

    // The acting user: the verified signer when auth is on, the body's declared
    // id when it is off (`resolve_user` rejects an empty/absent one there).
    let actor = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let writing = http_method == "PUT" || http_method == "DELETE";

    // ── Owned families: avatars belong to a user, icons to a group ───────────
    if writing {
        match (object.family, object.owner) {
            (R2Family::Avatar, Some(user_id)) => {
                if user_id != actor {
                    return Ok(AuthRejection::Forbidden.into_response());
                }
            }
            (R2Family::GroupIcon, Some(group_id)) => {
                let conn = state.db.conn().await?;
                if !crate::groups::is_admin(&conn, group_id, &actor).await? {
                    return Ok(AuthRejection::Forbidden.into_response());
                }
            }
            _ => {}
        }
    }

    // ── Shared families: an object anybody still references is untouchable ───
    //
    // One `media/<hash>` blob backs every message carrying that file and one
    // `emoji/<hash>` blob every group that registered it, across conversations
    // and users. Overwriting one substitutes chosen bytes for everyone (and for
    // media the AEAD key is derived from the hash every recipient already knows,
    // so the substitution decrypts cleanly); deleting one 404s the attachment or
    // emoji for everyone else.
    if writing {
        if let Some(content_hash) = object.content_hash {
            let referenced = match object.family {
                R2Family::Media => {
                    let conn = state.db.conn().await?;
                    crate::messages::object_is_referenced(&conn, content_hash).await?
                }
                R2Family::Emoji => {
                    let conn = state.db.conn().await?;
                    crate::emoji::object_is_referenced(&conn, content_hash).await?
                }
                R2Family::Avatar | R2Family::GroupIcon => false,
            };
            if referenced {
                return Ok(AuthRejection::Forbidden.into_response());
            }
        }
    }

    // ── PUT: content-addressed, and exactly this many bytes ──────────────────
    let signed_content_length = if http_method == "PUT" {
        if !object.content_addressed {
            return Ok(bad_request("a put must name a content-addressed key"));
        }
        let max = put_max_bytes(object.family);
        match parsed.content_length {
            Some(n) if n > 0 && n <= max => Some(n),
            Some(_) => return Ok(bad_request("content_length out of range")),
            None => return Ok(bad_request("content_length required for put")),
        }
    } else {
        // Only a PUT can meaningfully bind a body length; a signed
        // `content-length` on GET/DELETE would just make the URL unusable.
        None
    };

    let url = presign_r2_url_bounded(
        endpoint,
        bucket,
        &state.broker.r2_region,
        access_key,
        secret_key,
        http_method,
        &parsed.key,
        PRESIGN_EXPIRES_SECS,
        &amz_datetime(),
        signed_content_length,
    );

    Ok(ok_response::<R2PresignBody>(R2PresignResponse {
        url,
        method: http_method.to_string(),
        expires_in: PRESIGN_EXPIRES_SECS,
    }))
}

// ── SigV4 query-string presign ───────────────────────────────────────────────
//
// The query-string ("presigned URL") variant of AWS SigV4, ported from the
// auth-header form in pollis-core's `r2.rs`. Single-chunk, `UNSIGNED-PAYLOAD`,
// `host` the only signed header. The five `X-Amz-*` params go in the canonical
// query string; the signature is appended last (it is never itself signed).

/// Compute the current UTC time as the SigV4 `YYYYMMDDTHHMMSSZ` basic-format
/// timestamp. Split out so the handler stays I/O-only and the pure
/// [`presign_r2_url`] takes the timestamp as an argument (testable).
fn amz_datetime() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Build a SigV4 presigned URL for `method` on `bucket/key`. Pure — `datetime`
/// is injected — so tests can pin the clock and reproduce the signature.
///
/// Signs `host` only. See [`presign_r2_url_bounded`] for the variant that also
/// binds an exact body length.
#[allow(clippy::too_many_arguments)]
pub fn presign_r2_url(
    endpoint: &str,
    bucket: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    method: &str,
    key: &str,
    expires: u64,
    datetime: &str,
) -> String {
    presign_r2_url_bounded(
        endpoint, bucket, region, access_key, secret_key, method, key, expires, datetime, None,
    )
}

/// [`presign_r2_url`], optionally binding an EXACT `Content-Length`.
///
/// With `content_length: Some(n)` the canonical request signs
/// `content-length;host` instead of `host` alone, so the URL only authorizes a
/// body of exactly `n` bytes — R2 rejects anything else with a signature
/// mismatch. That is the difference between a size the server believes and a
/// size the storage layer enforces, and it is what bounds `emoji/…` objects
/// (#848).
///
/// `None` reproduces the original single-signed-header form byte for byte, which
/// is why every existing media/avatar presign is unchanged.
#[allow(clippy::too_many_arguments)]
pub fn presign_r2_url_bounded(
    endpoint: &str,
    bucket: &str,
    region: &str,
    access_key: &str,
    secret_key: &str,
    method: &str,
    key: &str,
    expires: u64,
    datetime: &str,
    content_length: Option<u64>,
) -> String {
    let date = &datetime[..8];
    let host = host_of(endpoint);

    // Canonical URI: `/<bucket>/<key>`, each path segment URI-encoded but with
    // `/` preserved (S3 encodes paths exactly once).
    let canonical_uri = format!(
        "/{}/{}",
        uri_encode(bucket, false),
        uri_encode(key, false)
    );

    // Canonical headers are lowercase and sorted by name — `content-length`
    // sorts before `host`, and `SignedHeaders` (both the canonical-request line
    // and the `X-Amz-SignedHeaders` query param) must list them in that order.
    let (canonical_headers, signed_headers) = match content_length {
        Some(n) => (
            format!("content-length:{n}\nhost:{host}\n"),
            "content-length;host",
        ),
        None => (format!("host:{host}\n"), "host"),
    };

    let credential = format!("{access_key}/{date}/{region}/s3/aws4_request");
    // Canonical query: params sorted by name, values URI-encoded (the credential
    // `/`s become %2F, and the signed-headers `;` becomes %3B). X-Amz-Signature
    // is NOT part of the canonical query.
    let canonical_query = {
        let mut params = [
            ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
            ("X-Amz-Credential", uri_encode(&credential, true)),
            ("X-Amz-Date", datetime.to_string()),
            ("X-Amz-Expires", expires.to_string()),
            ("X-Amz-SignedHeaders", uri_encode(signed_headers, true)),
        ];
        params.sort_by(|a, b| a.0.cmp(b.0));
        params
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("&")
    };

    let payload_hash = "UNSIGNED-PAYLOAD";
    let canonical_request = format!(
        "{method}\n{canonical_uri}\n{canonical_query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}"
    );

    let scope = format!("{date}/{region}/s3/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{datetime}\n{scope}\n{}",
        sha256_hex(canonical_request.as_bytes())
    );

    let signing_key = derive_signing_key(secret_key, date, region, "s3");
    let signature = hex::encode(hmac_sha256(&signing_key, string_to_sign.as_bytes()));

    format!(
        "{}{canonical_uri}?{canonical_query}&X-Amz-Signature={signature}",
        scheme_host(endpoint)
    )
}

/// `https://host` (no path) of an endpoint URL, for building the final URL.
fn scheme_host(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("https", url),
    };
    let host = rest.split('/').next().unwrap_or(rest);
    format!("{scheme}://{host}")
}

/// Bare host (no scheme, no path) — the SigV4 `host` header value.
fn host_of(url: &str) -> &str {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://");
    rest.split('/').next().unwrap_or(rest)
}

/// AWS-style percent-encoding (RFC 3986). Unreserved chars pass through; when
/// `encode_slash` is false, `/` is preserved (used for path segments). Matches
/// the canonical encoding S3 SigV4 requires.
fn uri_encode(s: &str, encode_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'.' | b'_' | b'~')
            || (b == b'/' && !encode_slash);
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0x0f) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn derive_signing_key(secret: &str, date: &str, region: &str, service: &str) -> Vec<u8> {
    let k_secret = format!("AWS4{secret}");
    let k_date = hmac_sha256(k_secret.as_bytes(), date.as_bytes());
    let k_region = hmac_sha256(&k_date, region.as_bytes());
    let k_service = hmac_sha256(&k_region, service.as_bytes());
    hmac_sha256(&k_service, b"aws4_request")
}
