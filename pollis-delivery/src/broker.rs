//! Authorized-secrets broker (#393).
//!
//! Mints LiveKit tokens and presigns R2 URLs server-side, so those secrets never ship in the client.
//!
//! Both reuse device-signature auth; identity comes from the verified signer, never the body.
//!
//! ## Why R2 presign needs no per-object READ authz (but delete is gated)
//!
//! Media is sealed under keys the DS never holds (#1382), so a presign only exposes ciphertext.
//!
//! `delete` is refused while a live message still references the object (#690, [`crate::messages::object_is_referenced`]).

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

// Request bodies live in `pollis-api`, shared with pollis-core.
pub use pollis_api::broker::*;

/// Secrets the broker needs, read from DS env in [`BrokerConfig::from_env`]. All
/// `Option`: a missing secret makes its endpoint answer 503 rather than fail startup.
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
            // Also accept the established client env names, reusing the same Doppler secrets.
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

/// LiveKit JWT claims; the shape is frozen by the LiveKit SFU.
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
/// #836: the identity is an opaque per-room pseudonym and the JWT carries no display name.
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

    // A clear 503 when the broker isn't configured.
    let (api_key, api_secret, url) = match state.broker.livekit_ready() {
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

    // Signed path: the header the signature was verified against; no-auth: the body.
    let device_id = if authed.is_some() {
        req.headers
            .get("x-pollis-device")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    } else {
        parsed.device_id.clone().unwrap_or_default()
    };
    // `view` = screenshare-receive (no data channel); the kind also picks the identity prefix.
    let kind = crate::participant_id::ParticipantKind::from_wire(parsed.kind.as_deref());
    let can_publish_data = kind != crate::participant_id::ParticipantKind::View;

    // #836: identity encrypted under a key scoped to the LOGICAL room; `device_id` keeps devices distinct (#140).
    let identity = crate::participant_id::participant_pseudonym(
        api_secret,
        &parsed.room,
        &user_id,
        &device_id,
        kind,
    );

    // #828: authz used the LOGICAL room; LiveKit sees only the pseudonym, and the client never holds the mapping.
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
    // A block only resets `accepted_at`, so refuse the token or the eviction undoes itself on reconnect.
    Ok(!dm_peer_blocked(&conn, room, user_id).await?)
}

/// True when another member of the DM `room` has blocked `user_id`.
///
/// `false` for non-DM rooms.
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
/// No display-name parameter, deliberately (#836): the SFU must not read the account.
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

// LiveKit RoomService (Twirp) calls need a short-lived admin JWT, signed here so the secret stays server-side.

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
/// A timeout is 504 (a retry may help); any other upstream failure is 502.
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
                "retryable": true,
            })),
        )
            .into_response();
    }
    bad_gateway(format!("{what}: {e}"))
}

// POST /v1/livekit/send-data. Authz: `gate`, and the TARGET room must be reachable:
//
//   - the signer's own inbox (`inbox-<signer>`) — always;
//   - another user's inbox (`inbox-<peer>`) — only while the two share a
//     conversation (a DM channel, a group, or a pending group invite from the
//     signer to the peer) AND neither has blocked the other; `call_invite`
//     additionally needs a relationship the peer consented to (an accepted DM or
//     a shared group) so a bare DM request cannot ring a stranger's devices;
//   - a conversation room — only for a current member (`is_member`).
//
// Only CLIENT-originated `type`s ([`ClientPayloadKind`]); `enrollment_requested` is DS-only.
//
// Identity keys are STRIPPED; private-inbox kinds get the VERIFIED signer stamped back in.

/// The control-payload types a client may fan out through `/v1/livekit/send-data`.
///
/// An allowlist: a `type` a client has no business publishing is unrepresentable.
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
/// Strips [`IDENTITY_KEYS`], then stamps the verified signer (and per-kind legacy keys) for private-inbox kinds.
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
/// server-side `RoomService/SendData`. A room with no participants (LiveKit's
/// 404, or its "no response from servers" 503 — see
/// [`send_data_room_is_empty`]) is success, mirroring the client's
/// fire-and-forget semantics.
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
    // Signed path only; checked BEFORE the secrets gate so a refused target is always a 403.
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
/// `RoomService/SendData`. A room with no participants (a 404, or the 503
/// [`send_data_room_is_empty`] recognises) is treated as success, matching the
/// fire-and-forget nudge semantics.
///
/// Shared with server-side emitters (e.g. `bootstrap::enrollment_request`, whose pre-enrollment device cannot sign).
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
    // #828: map at the chokepoint so no caller can hand LiveKit the logical room.
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
            // #1337: nobody is in the room, so there is nobody to nudge.
            if send_data_room_is_empty(status, &text) {
                // LiveKit gives this same answer when the node hosting a LIVE
                // room is down, so keep it countable: one line per occurrence,
                // naming the route only — no room, user, device or address.
                tracing::info!(
                    route = "livekit.RoomService/SendData",
                    "livekit: no server for room (empty or unhosted); nudge treated as delivered"
                );
                return Ok(());
            }
            Err(format!("SendData {status}: {text}"))
        }
        // Fire-and-forget callers just log; the point is to give up (#913).
        Err(e) if e.is_timeout() => Err(format!(
            "SendData: livekit timed out after {}s",
            crate::util::Upstream::LiveKit.timeout().as_secs()
        )),
        Err(e) => Err(format!("SendData: {e}")),
    }
}

/// Whether a non-2xx `RoomService/SendData` answer means "this room has no
/// participants (or does not exist)" rather than a real failure (#1337).
///
/// LiveKit answers an unhosted room with Twirp 503 `unavailable` / `no response from servers`, not 404.
///
/// Only that exact code + message, so a genuinely down LiveKit still reads as down.
pub fn send_data_room_is_empty(status: StatusCode, body: &str) -> bool {
    if status != StatusCode::SERVICE_UNAVAILABLE {
        return false;
    }
    let Ok(err) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    let code = err.get("code").and_then(|v| v.as_str());
    let msg = err.get("msg").and_then(|v| v.as_str());
    code == Some("unavailable") && msg == Some("no response from servers")
}

// RoomService/RemoveParticipant: a token is checked only at JOIN, so losing access needs a kick plus a short TTL.

/// Realtime/voice participant token lifetime, in seconds.
///
/// Short because membership is checked only at mint; a live connection is never dropped by expiry.
pub const LIVEKIT_TOKEN_TTL_SECS: u64 = 15 * 60;

/// Evict one participant identity from one LiveKit room via server-side
/// `RoomService/RemoveParticipant`.
///
/// A 404 (room or identity absent) is success: the desired end state.
pub async fn room_remove_participant(
    state: &AppState,
    room: &str,
    identity: &str,
) -> Result<(), String> {
    let (api_key, api_secret, url) = state
        .broker
        .livekit_ready()
        .ok_or_else(|| "livekit not configured".to_string())?;

    // #828: addressed by the room pseudonym, as in `room_send_data`.
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
/// One per `(device, kind)`, as `participant_pseudonym` keys; missing one leaves a session live.
///
/// Devices are explicit: membership loss evicts all, a revocation only one.
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
/// Revoked devices included: the tombstone is written before the eviction runs.
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
/// Includes the legacy no-device identity (`""`), which old clients may hold.
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

/// Fire-and-forget: failures are logged; the stale session dies when [`authorize_room`] refuses at reconnect.
///
/// Concurrent, so a stalled SFU costs one 5s deadline, not one per call.
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
                    // Pseudonym + logical room: names the failure without linking user to conversation.
                    tracing::warn!("RemoveParticipant failed: {e}");
                }
            });
        }
    }
    while tasks.join_next().await.is_some() {}
}

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
    // Members only (own inbox always ok), signed path only.
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
            // #836: anything not resolving to a real user under THIS room's key, or a `view` receiver, is dropped.
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

            // Display names are re-attached from the DS's DB (#836).
            let names = {
                let ids: Vec<String> = resolved.iter().map(|(_, u)| u.clone()).collect();
                let conn = state.db.conn().await?;
                lookup_usernames(&conn, &ids).await.unwrap_or_default()
            };
            let participants: Vec<ResolvedIdentity> = resolved
                .into_iter()
                .map(|(identity, uid)| {
                    let name = names.get(&uid).cloned().unwrap_or_else(|| uid.clone());
                    // No `kind` on the roster; skipped when absent.
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

/// POST /v1/livekit/identities — resolve opaque LiveKit participant identities
/// back to Pollis users (#836).
///
/// Same room authz as minting a token, so it discloses nothing new.
///
/// Unresolvable entries are omitted rather than erroring.
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

/// Default presigned-URL lifetime, in seconds.
const PRESIGN_EXPIRES_SECS: u64 = 900;

/// The longest key the bucket will ever be asked to sign.
const R2_KEY_MAX_LEN: usize = 512;

/// The object families the bucket holds. Nothing else is signable — see
/// [`parse_r2_key`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum R2Family {
    /// `media/<hex64>.enc` (and the legacy `media/<hex64>/<name>`) — reference-
    /// counted attachment ciphertext; the hex is a random file id (#1382).
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
    /// the reference gate is asked about; legacy shapes included.
    pub content_hash: Option<&'a str>,
    /// True when the key is CONTENT-ADDRESSED — its name is a 64-hex digest of
    /// the bytes. Only these are writable; legacy mutable keys stay read-only.
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
/// An allow-list: without one, any device could sign any object, a `..` key, or free storage.
///
/// Segment hygiene: no empty/`.`/`..` segment and a narrow charset, so keys can't traverse or inject.
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
/// No per-object READ authz is needed (see the module docs). WRITE gates:
///
///   * **The key must be one the product writes** ([`parse_r2_key`]).
///   * **A PUT must be content-addressed**, so nobody can overwrite e.g. `avatars/<uid>`.
///   * **A PUT must sign its exact, bounded length**, so R2 itself enforces the size.
///   * **A PUT or DELETE of an owned object needs the owner.** Only a user may
///     write or delete under their own `avatars/` prefix, and only an admin of
///     the group under its `group-icons/` prefix.
///   * **A PUT or DELETE of a shared object needs it unreferenced** (#690, #848).
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

    let actor = match resolve_user(&authed, parsed.user_id.as_deref()) {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let writing = http_method == "PUT" || http_method == "DELETE";

    // Owned families: avatars belong to a user, icons to a group.
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

    // Shared families: overwriting substitutes bytes for everyone; deleting 404s them for everyone.
    //
    // No identity in the answer: `object_is_referenced` is the one predicate, shared with `apply_delete_attachment`.
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

    // PUT: content-addressed, and exactly this many bytes.
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

// SigV4 query-string presign: single-chunk, `UNSIGNED-PAYLOAD`; the signature is appended last, unsigned.

/// Compute the current UTC time as the SigV4 `YYYYMMDDTHHMMSSZ` basic-format
/// timestamp, split out so [`presign_r2_url`] stays pure.
fn amz_datetime() -> String {
    chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

/// Build a SigV4 presigned URL for `method` on `bucket/key`. Pure — `datetime`
/// is injected — so tests can pin the clock and reproduce the signature.
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
/// With `Some(n)` the URL authorizes exactly `n` bytes; R2 rejects any other length (#848).
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
