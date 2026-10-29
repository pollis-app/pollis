//! The MLS control-plane write endpoints beyond `POST /v1/commits` (W4–W8 in
//! `docs/goal-a-commit-log-sole-writer.md`), plus the auth gates and write-result
//! helpers every domain module shares. Writes land on `state.log_db`; membership
//! and auth read `state.db`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;

use crate::auth;
use crate::error::{AppError, AuthRejection};
use crate::AppState;
use crate::util::b64_decode;

// Re-exported so `pollis_delivery::writes::*Body` resolves for handlers and tests.
pub use pollis_api::writes::*;

/// Method, path, headers and the **raw** body bytes: exactly what a device
/// signature covers. The body cannot be deserialized before it is verified, so
/// `Json<T>` is not available to these routes. `Bytes` is extracted last because
/// it consumes the body.
pub struct RawRequest {
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap,
    pub body: Bytes,
}

#[axum::async_trait]
impl<S: Send + Sync> axum::extract::FromRequest<S> for RawRequest {
    type Rejection = axum::extract::rejection::BytesRejection;

    async fn from_request(req: axum::extract::Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        // Cloned, not moved, so `Bytes::from_request` gets the request back intact.
        let (method, uri, headers) = (parts.method.clone(), parts.uri.clone(), parts.headers.clone());
        let body =
            Bytes::from_request(axum::extract::Request::from_parts(parts, body), state).await?;
        Ok(Self {
            method,
            uri,
            headers,
            body,
        })
    }
}

/// The shared auth gate's outcome: the authenticated user, or `None` on the
/// no-auth path.
pub(crate) type Authed = Option<String>;

/// The one auth gate every write-domain module shares: when `require_auth` is on,
/// verify the device signature over the raw body and return the `user_id`; when
/// off, `None`. `Ok(Err(response))` is a rejection the caller forwards unchanged.
pub(crate) async fn gate(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    // Cached pubkey lookup (#658): this is the chokepoint every device-signed
    // endpoint goes through. A connection is checked out only on a miss (#1500).
    match auth::verify_request_cached(
        &state.device_keys,
        &state.db,
        &req.headers,
        req.method.as_str(),
        req.uri.path(),
        &req.body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok(user_id) => Ok(Ok(Some(user_id))),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// [`gate`], also accepting a verified OTP session (`X-Pollis-Session`), for the
/// account-lifecycle writes a PRE-ENROLLMENT device can reach (it has no key to
/// sign with). `user_id` binds from the session, never the body.
///
/// A request carrying the signature header always takes the signature gate, so a
/// bad signature is never rescued by a session token.
pub(crate) async fn gate_or_session(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    Ok(gate_or_session_kind(state, req).await?.map(|(authed, _)| authed))
}

/// WHICH credential [`gate_or_session`] accepted. `/v1/account/rotate-identity`
/// is a plain key rotation under a signature but the pre-enrollment soft reset
/// under a session (see `account::apply_rotate_identity`), so the gate must say.
/// Both `device_id`s are server-verified.
pub enum GateCredential {
    /// `require_auth = false` — no credential exists on this deployment.
    None,
    /// A verified device signature (`gate`).
    Signature { device_id: String },
    /// A verified-OTP session (`X-Pollis-Session`).
    Session { device_id: String },
}

/// [`gate_or_session`], also reporting which credential authenticated the
/// request.
pub(crate) async fn gate_or_session_kind(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<(Authed, GateCredential), Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok((None, GateCredential::None)));
    }
    if req.headers.contains_key(auth::H_SIGNATURE) {
        return Ok(match gate(state, req).await? {
            Ok(authed) => {
                // Verified by `gate`: the signer's pubkey was looked up under
                // exactly this `(device_id, user_id)`, so the header is bound.
                let device_id = req
                    .headers
                    .get(auth::H_DEVICE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                Ok((authed, GateCredential::Signature { device_id }))
            }
            Err(resp) => Err(resp),
        });
    }
    match crate::session::verify_session(&req.headers, &state.sessions, crate::util::now_unix()) {
        Ok(claims) => Ok(Ok((
            Some(claims.user_id),
            GateCredential::Session { device_id: claims.device_id },
        ))),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// [`gate`] followed by parsing the body. The order is the point: the signature
/// is verified over the raw bytes before the body is deserialized, so a forged
/// body never reaches `serde`, let alone the DB. `Ok(Err(response))` is a
/// rejection (401/403, or 400 for an unparseable body).
pub(crate) async fn gate_and_parse<B>(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<(Authed, B), Response>, AppError>
where
    B: serde::de::DeserializeOwned,
{
    let authed = match gate(state, req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(Err(resp)),
    };
    Ok(match serde_json::from_slice(&req.body) {
        Ok(parsed) => Ok((authed, parsed)),
        Err(_) => Err(bad_request("invalid body")),
    })
}

/// [`gate_and_parse`] over [`gate_or_session`] — same ordering guarantee, for the
/// endpoints an OTP session may also authenticate.
pub(crate) async fn gate_or_session_and_parse<B>(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<(Authed, B), Response>, AppError>
where
    B: serde::de::DeserializeOwned,
{
    Ok(gate_or_session_kind_and_parse::<B>(state, req)
        .await?
        .map(|(authed, _, parsed)| (authed, parsed)))
}

/// [`gate_or_session_and_parse`] over [`gate_or_session_kind`], also reporting
/// which credential authenticated the request.
pub(crate) async fn gate_or_session_kind_and_parse<B>(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<(Authed, GateCredential, B), Response>, AppError>
where
    B: serde::de::DeserializeOwned,
{
    let (authed, credential) = match gate_or_session_kind(state, req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(Err(resp)),
    };
    Ok(match serde_json::from_slice(&req.body) {
        Ok(parsed) => Ok((authed, credential, parsed)),
        Err(_) => Err(bad_request("invalid body")),
    })
}

/// The outcome of the identity gate: the authenticated `(user_id, device_id)`
/// pair, or `None` on the no-auth path. Same contract as [`Authed`], with the
/// DEVICE half kept.
pub(crate) type AuthedIdentity = Option<(String, String)>;

/// [`gate`] returning the verified `device_id` too, for writes keyed on the
/// device (e.g. `/v1/watermarks/advance`), so the handler binds the body's
/// `device_id` to the signer.
pub(crate) async fn gate_identity(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<AuthedIdentity, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    match auth::verify_request_identity_cached(
        &state.device_keys,
        &state.db,
        &req.headers,
        req.method.as_str(),
        req.uri.path(),
        &req.body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok(identity) => Ok(Ok(Some(identity))),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// [`gate_and_parse`] over [`gate_identity`].
pub(crate) async fn gate_identity_and_parse<B>(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<(AuthedIdentity, B), Response>, AppError>
where
    B: serde::de::DeserializeOwned,
{
    let authed = match gate_identity(state, req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(Err(resp)),
    };
    Ok(match serde_json::from_slice(&req.body) {
        Ok(parsed) => Ok((authed, parsed)),
        Err(_) => Err(bad_request("invalid body")),
    })
}

/// Resolve the recipient/owner a welcome op targets.
///
///   - auth ON  → the authenticated user. If the body also carries `user_id`,
///     it must equal the authenticated user (else 403).
///   - auth OFF → the body's `user_id` (the no-auth path has no signed identity,
///     so the recipient must be supplied explicitly). Missing/empty → 400.
// As in `broker::resolve_user`: the `Err` IS the response returned to the client.
#[allow(clippy::result_large_err)]
fn resolve_recipient(authed: Authed, body_user_id: Option<String>) -> Result<String, Response> {
    match authed {
        Some(user) => {
            if let Some(b) = body_user_id {
                if b != user {
                    return Err(AuthRejection::Forbidden.into_response());
                }
            }
            Ok(user)
        }
        None => match body_user_id {
            Some(b) if !b.is_empty() => Ok(b),
            _ => Err(bad_request("user_id required when auth is disabled")),
        },
    }
}

pub(crate) fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

/// 200 + the ONE success body `B`'s endpoint is declared to answer with (#922).
///
/// Taking `B::Response` (not `impl Serialize`) means the two ends cannot name
/// different shapes: the client decodes the same associated type through
/// `ds_post_json::<B>`. `pub` so the compile-fail doctest below (an external
/// crate) can prove it.
///
/// ```compile_fail,E0308
/// use pollis_delivery::writes::ok_response;
/// use pollis_api::{broker::LivekitTokenBody, StatusOk};
/// // `/v1/livekit/token` answers a token, not `{"status":"ok"}`.
/// let _ = ok_response::<LivekitTokenBody>(StatusOk::Ok);
/// ```
///
/// ```
/// use pollis_delivery::writes::ok_response;
/// use pollis_api::broker::{LivekitTokenBody, LivekitTokenResponse};
/// let _ = ok_response::<LivekitTokenBody>(LivekitTokenResponse {
///     token: "jwt".into(),
///     url: "wss://sfu".into(),
/// });
/// ```
pub fn ok_response<B: pollis_api::DsRequest>(body: B::Response) -> Response {
    (StatusCode::OK, Json(body)).into_response()
}

/// 200 with NO body, for the endpoints declared [`pollis_api::Empty`]; bounded on
/// `Response = Empty` so a handler cannot use it for an endpoint that owes a body.
pub fn ok_empty<B: pollis_api::DsRequest<Response = pollis_api::Empty>>() -> Response {
    StatusCode::OK.into_response()
}

/// The result of an authorized write: applied, or not allowed (→ 403). A genuine
/// failure is an `Err` on the enclosing `anyhow::Result`. `pub` so the external
/// test harness can match on it.
#[derive(Debug)]
pub enum WriteOutcome {
    Ok,
    Forbidden,
    /// An envelope write asserted a `(generation, epoch)` that is not the commit
    /// log's head (#1041). Nothing is stored; the client must catch up, re-seal
    /// and post again (→ 409 [`pollis_api::messages::EpochBehind`]). Only the
    /// two envelope endpoints produce this.
    EpochBehind {
        head_generation: i64,
        head_epoch: i64,
    },
    /// Well-formed but inadmissible value (→ 400), e.g. a `sent_at` that is not a
    /// canonical UTC RFC 3339 stamp or sits past the clock-skew allowance
    /// (`messages::check_cursor_stamp`).
    Invalid(&'static str),
}

/// The 409 body for [`WriteOutcome::EpochBehind`].
pub(crate) fn epoch_behind_response(head_generation: i64, head_epoch: i64) -> Response {
    (
        StatusCode::CONFLICT,
        Json(pollis_api::messages::EpochBehind {
            error: pollis_api::messages::EpochBehind::ERROR.to_string(),
            head_generation,
            head_epoch,
        }),
    )
        .into_response()
}

/// Map a [`WriteOutcome`] to the HTTP response (200 / 403 / 409 / 400). Generic
/// over the request type (#922): `B::Response` must be [`pollis_api::StatusOk`],
/// so an endpoint that declares a richer body cannot answer a bare
/// `{"status":"ok"}` through here.
pub(crate) fn outcome_response<B>(outcome: WriteOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    Ok(match outcome {
        WriteOutcome::Ok => ok_response::<B>(pollis_api::StatusOk::Ok),
        WriteOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => epoch_behind_response(head_generation, head_epoch),
        WriteOutcome::Invalid(msg) => bad_request(msg),
    })
}

/// Resolve the actor a write is performed as.
///
///   - auth ON  → the authenticated user. If the body also carries an actor id,
///     it must equal the authenticated user (else `Forbidden`).
///   - auth OFF → the body's actor id (no signed identity on the no-auth path).
///     Missing/empty → `Forbidden`.
///
/// Returns [`WriteOutcome::Forbidden`] so `apply_*` fns can bail uniformly.
pub(crate) fn resolve_actor(
    authed: Option<&str>,
    body_actor: Option<&str>,
) -> Result<String, WriteOutcome> {
    match authed {
        Some(u) => match body_actor {
            Some(b) if b != u => Err(WriteOutcome::Forbidden),
            _ => Ok(u.to_string()),
        },
        None => match body_actor {
            Some(b) if !b.is_empty() => Ok(b.to_string()),
            _ => Err(WriteOutcome::Forbidden),
        },
    }
}

/// Membership check against the main DB: is `user_id` a current member of the
/// MLS conversation `conversation_id`?
///
/// An MLS `conversation_id` is one of three things, depending on the surface
/// that created the group, so all three must be accepted:
///   - a **group id** — a group's text channels share one MLS group keyed by the
///     group id (membership via `group_member.group_id = conversation_id`);
///   - a **DM channel id** (membership via `dm_channel_member`);
///   - a **channel id** — e.g. a voice channel's own MLS group (membership via
///     the channel's owning group).
pub async fn is_member(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM dm_channel_member \
                        WHERE dm_channel_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_member \
                        WHERE group_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_member gm ON gm.group_id = c.group_id \
                        WHERE c.id = ?1 AND gm.user_id = ?2) \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Is `user_id` in this conversation's DESIRED roster
/// ([`crate::directory::desired_roster`]): `group_member` plus pending
/// `group_invite` invitees?
///
/// The right gate for a Welcome, not [`is_member`]: an invitee's devices are
/// Welcomed at INVITE time so accepting does not need the inviter online. This
/// only stops a Welcome addressed to someone the conversation is not admitting;
/// the forged-Welcome half (C1) is the client's `GroupId` check in
/// `join_from_welcome`.
pub async fn in_desired_roster(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM dm_channel_member \
                        WHERE dm_channel_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_member \
                        WHERE group_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_invite \
                        WHERE group_id = ?1 AND invitee_id = ?2) \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_member gm ON gm.group_id = c.group_id \
                        WHERE c.id = ?1 AND gm.user_id = ?2) \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_invite gi ON gi.group_id = c.group_id \
                        WHERE c.id = ?1 AND gi.invitee_id = ?2) \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The kinds of conversation that share one id namespace — exactly the three
/// legs [`is_member`] ORs over, which is precisely why they cannot be allowed to
/// disagree about what an id means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Dm,
    Group,
    Channel,
}

impl ConversationKind {
    /// The value stored in `conversation.kind`. Migration `000016`'s CHECK
    /// accepts these three strings and nothing else, so a fourth kind cannot be
    /// smuggled in past the three `is_member` knows how to answer for.
    pub fn as_str(self) -> &'static str {
        match self {
            ConversationKind::Dm => "dm",
            ConversationKind::Group => "group",
            ConversationKind::Channel => "channel",
        }
    }
}

/// Claim `id` for `kind` in the `conversation` registry (#880), which owns the
/// conversation-id namespace.
///
/// **Call this inside the transaction that inserts the row the id names**, so a
/// refused claim also refuses the row. The registry is append-only: a claimed id
/// stays claimed after its conversation is deleted, since with `foreign_keys=OFF`
/// the deleted group's rows would otherwise pass to the next claimant.
/// [`conversation_id_taken`] runs first so the ordinary case is a 403, not a 500.
pub async fn claim_conversation_id(
    conn: &Connection,
    id: &str,
    kind: ConversationKind,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO conversation (id, kind) VALUES (?1, ?2)",
        libsql::params![id.to_string(), kind.as_str().to_string()],
    )
    .await?;
    Ok(())
}

/// True when `id` is already in use as ANY kind of conversation, or claimed in
/// the registry.
///
/// Every create endpoint takes the id from the request body, and [`is_member`]
/// ORs across the three tables on one id, so a DM named with a victim group's id
/// would grant membership of that group (#880). The registry remembers retired
/// ids; the three tables cover rows that predate it. See
/// `.codesight/wiki/database.md` → `conversation`.
pub async fn conversation_id_taken(conn: &Connection, id: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM conversation WHERE id = ?1) \
             OR EXISTS (SELECT 1 FROM dm_channel   WHERE id = ?1) \
             OR EXISTS (SELECT 1 FROM groups       WHERE id = ?1) \
             OR EXISTS (SELECT 1 FROM channels     WHERE id = ?1) \
             LIMIT 1",
            libsql::params![id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Hard ceiling on one published GroupInfo blob: far above any real ratchet tree,
/// far below parking storage in a single mutable slot per conversation.
pub const GROUP_INFO_MAX_BYTES: usize = 1024 * 1024;

/// The verdict of a control-plane write that names a `(generation, epoch)`.
///
/// Separate from [`WriteOutcome`] because "you named a key the commit log has
/// not reached" is neither a permission problem nor a malformed body: it is a
/// statement about the log, and the answer carries the head so the caller can
/// catch up (the same 409 shape `/v1/commits` already returns).
#[derive(Debug)]
pub enum HeadBoundedOutcome {
    /// The write was admissible. `affected` is 0 when the monotone CAS declined
    /// it (a stale republish), which is a normal race outcome, not an error.
    Ok { affected: u64 },
    /// The named key sits ABOVE the log's head — an unreachable future epoch.
    AheadOfHead {
        head_generation: i64,
        head_epoch: i64,
    },
    /// `&'static str` so no caller input is ever echoed back.
    Invalid(&'static str),
}

/// The ceiling a `(generation, epoch)` control-plane write may not exceed:
/// `(head_generation, head_epoch_in(generation))`.
///
/// `mls_group_info` and `pin_keystate` CAS on strictly-greater `(generation,
/// epoch)`, which is monotone but unbounded: one write of `generation = 2^62`
/// would freeze the row forever. The commit log alone decides which epochs exist,
/// so its head is the ceiling. See `.codesight/wiki/mls.md`.
///
/// Read from the LOG DB, inside the write's transaction where the write lands
/// there too; a stale head read can only make the check stricter.
pub async fn head_bound(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
) -> anyhow::Result<(i64, i64)> {
    let head_generation = crate::commit::head_generation(log_conn, conversation_id).await?;
    let head_epoch = crate::commit::head_epoch_in(log_conn, conversation_id, generation).await?;
    Ok((head_generation, head_epoch))
}

/// `Some(outcome)` when `(generation, epoch)` is not admissible — negative, or
/// above the log's head. `None` when the write may proceed.
pub async fn refuse_above_head(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> anyhow::Result<Option<HeadBoundedOutcome>> {
    if generation < 0 || epoch < 0 {
        return Ok(Some(HeadBoundedOutcome::Invalid(
            "generation/epoch out of range",
        )));
    }
    let (head_generation, head_epoch) = head_bound(log_conn, conversation_id, generation).await?;
    if generation > head_generation {
        // The named lineage does not exist, so its own head is a meaningless 0.
        // Report the head LINEAGE's head instead — the position the caller has
        // to catch up to before anything it names can be admissible.
        let head_epoch =
            crate::commit::head_epoch_in(log_conn, conversation_id, head_generation).await?;
        return Ok(Some(HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        }));
    }
    if epoch > head_epoch {
        return Ok(Some(HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        }));
    }
    Ok(None)
}

/// Map a [`HeadBoundedOutcome`] to the HTTP response (200 / 409 with the head /
/// 400). The 409 body is the same [`pollis_api::messages::EpochBehind`] shape
/// `/v1/commits` answers a stale submit with, because it answers the same
/// question — "here is the head; you are not at it".
pub(crate) fn head_bounded_response<B>(outcome: HeadBoundedOutcome) -> Response
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    match outcome {
        HeadBoundedOutcome::Ok { .. } => ok_response::<B>(pollis_api::StatusOk::Ok),
        HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        } => epoch_behind_response(head_generation, head_epoch),
        HeadBoundedOutcome::Invalid(msg) => bad_request(msg),
    }
}

/// POST /v1/group-info — republish GroupInfo for a conversation, epoch-monotone
/// (an older epoch can never clobber a newer one). When auth is enforced, the
/// authenticated user must be a current member of `conversation_id`.
pub async fn group_info(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<GroupInfoBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    // Authz: a signed request may only republish for a conversation it belongs
    // to. Skipped on the no-auth path (mirrors submit).
    if let Some(user_id) = &authed {
        let conn = state.db.conn().await?;
        if !is_member(&conn, &parsed.conversation_id, user_id).await? {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let gi = match b64_decode(&parsed.group_info) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid group_info")),
    };

    let conn = state.log_db.conn().await?;
    let outcome = upsert_group_info(
        &conn,
        &parsed.conversation_id,
        parsed.generation,
        parsed.epoch,
        &gi,
        &parsed.updated_by_device_id,
    )
    .await?;

    Ok(head_bounded_response::<GroupInfoBody>(outcome))
}

/// Decode a [`GroupInfoBody`]'s base64 GroupInfo and UPSERT it (W4 minus
/// auth/authz), for the integration harness. A decode failure is an `Err` here.
pub async fn apply_group_info(
    log_conn: &Connection,
    body: &GroupInfoBody,
) -> anyhow::Result<HeadBoundedOutcome> {
    let gi = b64_decode(&body.group_info)?;
    upsert_group_info(
        log_conn,
        &body.conversation_id,
        body.generation,
        body.epoch,
        &gi,
        &body.updated_by_device_id,
    )
    .await
}

/// `(generation, epoch)`-monotone UPSERT of a conversation's GroupInfo (W4),
/// compared lexicographically. The generation term matters at a suite migration
/// (#454 P4): the successor lineage restarts at epoch 0, below the retired one's.
///
/// Also ceilinged at the commit log's head ([`refuse_above_head`]), read in the
/// same transaction, and bounded by [`GROUP_INFO_MAX_BYTES`].
pub async fn upsert_group_info(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
    group_info: &[u8],
    updated_by_device_id: &str,
) -> anyhow::Result<HeadBoundedOutcome> {
    if group_info.is_empty() {
        return Ok(HeadBoundedOutcome::Invalid("group_info is empty"));
    }
    if group_info.len() > GROUP_INFO_MAX_BYTES {
        return Ok(HeadBoundedOutcome::Invalid("group_info too large"));
    }

    // One transaction for the ceiling read and the CAS: the head the write is
    // checked against is the head the write lands on.
    let tx = log_conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;
    if let Some(refused) = refuse_above_head(&tx, conversation_id, generation, epoch).await? {
        tx.rollback().await?;
        return Ok(refused);
    }
    let affected = tx
        .execute(
            "INSERT INTO mls_group_info (conversation_id, generation, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 generation = excluded.generation, \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now') \
             WHERE excluded.generation > mls_group_info.generation \
                OR (excluded.generation = mls_group_info.generation \
                    AND excluded.epoch > mls_group_info.epoch)",
            libsql::params![
                conversation_id.to_string(),
                generation,
                epoch,
                group_info.to_vec(),
                updated_by_device_id.to_string(),
            ],
        )
        .await?;
    tx.commit().await?;
    Ok(HeadBoundedOutcome::Ok { affected })
}

/// POST /v1/welcomes/ack — mark the given Welcomes delivered, scoped to the
/// authenticated recipient so a user can only ack their own Welcomes.
pub async fn welcomes_ack(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AckBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn().await?;
    let updated = ack_welcomes(&conn, &recipient, &parsed.welcome_ids).await?;

    Ok(ok_response::<AckBody>(WelcomesUpdated::Ok { updated }))
}

/// Mark the given Welcomes `delivered = 1`, scoped to `recipient` so a user can
/// only ack their own Welcomes (W5). Pure conn-level write reused by the harness.
pub async fn ack_welcomes(
    log_conn: &Connection,
    recipient: &str,
    welcome_ids: &[String],
) -> anyhow::Result<u64> {
    if welcome_ids.is_empty() {
        return Ok(0);
    }

    // Recipient bound first so the filter never touches another user's Welcomes.
    // Chunked (#916) with one slot reserved for it: the id list comes off the wire,
    // and exceeding SQLite's parameter limit fails the whole statement.
    let mut updated: u64 = 0;
    for chunk in crate::util::bind_chunks(welcome_ids, 1) {
        let sql = format!(
            "UPDATE mls_welcome SET delivered = 1 \
             WHERE recipient_id = ?1 AND id IN ({})",
            crate::util::placeholders(chunk.len(), 2)
        );
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(libsql::Value::Text(recipient.to_string()));
        for id in chunk {
            params.push(libsql::Value::Text(id.clone()));
        }
        updated += log_conn.execute(&sql, libsql::params_from_iter(params)).await?;
    }

    Ok(updated)
}

/// POST /v1/welcomes/reset — re-arm Welcomes for redelivery (set `delivered=0`),
/// scoped to the authenticated recipient.
pub async fn welcomes_reset(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ResetBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn().await?;
    let updated = reset_welcomes(&conn, &recipient, parsed.device_id.as_deref()).await?;

    Ok(ok_response::<ResetBody>(WelcomesUpdated::Ok { updated }))
}

/// Re-arm Welcomes for redelivery (`delivered = 0`) for `recipient` (W6/W7).
/// `device_id` `Some` → device-scoped (W6); `None` → all the recipient's
/// Welcomes (W7). Pure conn-level write reused by the harness.
pub async fn reset_welcomes(
    log_conn: &Connection,
    recipient: &str,
    device_id: Option<&str>,
) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "UPDATE mls_welcome SET delivered = 0 \
             WHERE recipient_id = ?1 \
               AND (?2 IS NULL OR recipient_device_id = ?2 OR recipient_device_id IS NULL)",
            libsql::params![recipient.to_string(), device_id.map(|s| s.to_string())],
        )
        .await?)
}

/// POST /v1/welcomes/purge — delete all of the authenticated user's Welcomes
/// (identity-reset cleanup). Recipient is derived from auth; the body carries an
/// explicit `user_id` only on the no-auth path.
pub async fn welcomes_purge(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let authed = match gate_or_session(&state, &req).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };

    // An empty body is valid; tolerate it when auth is on (recipient from auth).
    let parsed: PurgeBody = if req.body.is_empty() {
        PurgeBody { user_id: None }
    } else {
        match serde_json::from_slice(&req.body) {
            Ok(b) => b,
            Err(_) => return Ok(bad_request("invalid body")),
        }
    };

    let recipient = match resolve_recipient(authed, parsed.user_id) {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.log_db.conn().await?;
    let deleted = purge_welcomes(&conn, &recipient).await?;

    Ok(ok_response::<PurgeBody>(WelcomesPurged::Ok { deleted }))
}

/// Delete all of `recipient`'s Welcomes (W8, identity-reset cleanup). Pure
/// conn-level write reused by the harness.
pub async fn purge_welcomes(log_conn: &Connection, recipient: &str) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "DELETE FROM mls_welcome WHERE recipient_id = ?1",
            libsql::params![recipient.to_string()],
        )
        .await?)
}

/// Hard ceiling on one Welcome blob. A Welcome carries the group's ratchet tree
/// plus the joiner's secrets, so it is bounded by the same reasoning (and the
/// same number) as a GroupInfo.
pub const WELCOME_MAX_BYTES: usize = GROUP_INFO_MAX_BYTES;

/// The verdict of `POST /v1/welcomes/resubmit`.
#[derive(Debug)]
pub enum ResubmitOutcome {
    Ok,
    Forbidden,
    /// `&'static str` so no caller input is ever echoed back.
    Invalid(&'static str),
    /// The Welcome names a lineage the commit log has not opened.
    AheadOfHead {
        head_generation: i64,
        head_epoch: i64,
    },
}

/// The verdict of the low-level Welcome upsert.
#[derive(Debug)]
pub enum WelcomeUpsertOutcome {
    Ok,
    /// The tuple already holds an UNDELIVERED Welcome published by somebody
    /// else, and the actor is neither its publisher, the head commit's author,
    /// nor an admin of the conversation's group.
    Forbidden,
}

/// The author of the newest commit in `generation` — the account that actually
/// performed the Add this Welcome belongs to. `None` for a lineage with no
/// commits.
pub async fn head_commit_sender(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
) -> anyhow::Result<Option<String>> {
    let mut rows = log_conn
        .query(
            "SELECT sender_id FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2 \
             ORDER BY epoch DESC LIMIT 1",
            libsql::params![conversation_id.to_string(), generation],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// True when `user_id` is an ADMIN of the group that owns `conversation_id`.
///
/// The two id kinds that can have admins are a group id and one of its channel
/// ids; a DM has no admins, so it simply matches nothing. Deliberately narrower
/// than [`is_member`], which ORs in `dm_channel_member` as well.
pub async fn is_conversation_admin(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM group_member \
                        WHERE group_id = ?1 AND user_id = ?2 AND role = 'admin') \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_member gm ON gm.group_id = c.group_id \
                        WHERE c.id = ?1 AND gm.user_id = ?2 AND gm.role = 'admin') \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// POST /v1/welcomes/resubmit — idempotently (re)insert a single Welcome for
/// `(conversation_id, recipient_id, recipient_device_id)`, so a recipient whose
/// Welcome went missing can be re-driven WITHOUT depending solely on the client's
/// external-join fallback.
pub async fn welcomes_resubmit(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ResubmitBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let main = state.db.conn().await?;
    let log = state.log_db.conn().await?;
    let outcome = apply_welcomes_resubmit(&main, &log, authed.as_deref(), &parsed).await?;
    Ok(match outcome {
        ResubmitOutcome::Ok => ok_response::<ResubmitBody>(pollis_api::StatusOk::Ok),
        ResubmitOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        ResubmitOutcome::Invalid(why) => bad_request(why),
        ResubmitOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        } => epoch_behind_response(head_generation, head_epoch),
    })
}

/// The resubmit decision and the write. Gates: the actor is a current member
/// (skipped on the no-auth path); the recipient is in the desired roster; the
/// lineage exists ([`refuse_above_head`]); and a pending Welcome someone else
/// published may be replaced only by the head commit's author or a group admin.
/// See `.codesight/wiki/mls.md`.
pub async fn apply_welcomes_resubmit(
    main_conn: &Connection,
    log_conn: &Connection,
    authed: Option<&str>,
    body: &ResubmitBody,
) -> anyhow::Result<ResubmitOutcome> {
    let Ok(welcome) = b64_decode(&body.welcome) else {
        return Ok(ResubmitOutcome::Invalid("invalid welcome"));
    };
    if welcome.is_empty() || welcome.len() > WELCOME_MAX_BYTES {
        return Ok(ResubmitOutcome::Invalid("welcome size out of range"));
    }

    if let Some(user_id) = authed {
        if !is_member(main_conn, &body.conversation_id, user_id).await? {
            return Ok(ResubmitOutcome::Forbidden);
        }
    }
    // The DESIRED roster, not `is_member`: the same predicate `/v1/commits` applies
    // to a bundle's Welcomes, and an invitee's Welcome must be re-drivable before
    // the invite is accepted.
    if !in_desired_roster(main_conn, &body.conversation_id, &body.recipient_id).await? {
        return Ok(ResubmitOutcome::Forbidden);
    }

    // A Welcome names only a lineage, not an epoch, so the ceiling that applies
    // is the generation half: epoch 0 is at or below every lineage's head.
    match refuse_above_head(log_conn, &body.conversation_id, body.generation, 0).await? {
        Some(HeadBoundedOutcome::Invalid(why)) => return Ok(ResubmitOutcome::Invalid(why)),
        Some(HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        }) => {
            return Ok(ResubmitOutcome::AheadOfHead {
                head_generation,
                head_epoch,
            })
        }
        Some(HeadBoundedOutcome::Ok { .. }) | None => {}
    }

    // Who may take a pending Welcome away from the member that wrote it.
    let may_override = match authed {
        // No signed identity to attribute anything to (dev/test path).
        None => true,
        Some(actor) => {
            head_commit_sender(log_conn, &body.conversation_id, body.generation)
                .await?
                .as_deref()
                == Some(actor)
                || is_conversation_admin(main_conn, &body.conversation_id, actor).await?
        }
    };

    match upsert_welcome(
        log_conn,
        &body.conversation_id,
        body.generation,
        &body.recipient_id,
        &body.recipient_device_id,
        &welcome,
        authed,
        may_override,
    )
    .await?
    {
        WelcomeUpsertOutcome::Ok => Ok(ResubmitOutcome::Ok),
        WelcomeUpsertOutcome::Forbidden => Ok(ResubmitOutcome::Forbidden),
    }
}

/// Idempotent (re)insert of a Welcome on the UNIQUE
/// `(conversation_id, recipient_id, recipient_device_id)` tuple: a resend
/// refreshes the blob and re-arms delivery. Mirrors the commit bundle's insert.
///
/// When `may_override` is false, a pending row another actor published
/// (`submitted_by`) is refused; the read and write share one IMMEDIATE
/// transaction so two racing resubmits cannot both pass.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_welcome(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
    recipient_id: &str,
    recipient_device_id: &str,
    welcome: &[u8],
    submitted_by: Option<&str>,
    may_override: bool,
) -> anyhow::Result<WelcomeUpsertOutcome> {
    let tx = log_conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    if !may_override {
        let mut rows = tx
            .query(
                "SELECT delivered, submitted_by FROM mls_welcome \
                 WHERE conversation_id = ?1 AND recipient_id = ?2 AND recipient_device_id = ?3",
                libsql::params![
                    conversation_id.to_string(),
                    recipient_id.to_string(),
                    recipient_device_id.to_string(),
                ],
            )
            .await?;
        let conflict = match rows.next().await? {
            Some(row) => {
                let delivered: i64 = row.get(0)?;
                let owner: Option<String> = row.get(1)?;
                // An unattributed (pre-000006) row and an already-delivered one
                // are both fair game: the first has no owner to protect, the
                // second is spent. Only a PENDING Welcome someone else published
                // is defended.
                delivered == 0 && owner.is_some() && owner.as_deref() != submitted_by
            }
            None => false,
        };
        drop(rows);
        if conflict {
            tx.rollback().await?;
            return Ok(WelcomeUpsertOutcome::Forbidden);
        }
    }

    tx.execute(
        "INSERT INTO mls_welcome \
             (id, conversation_id, generation, recipient_id, welcome_data, recipient_device_id, delivered, submitted_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7) \
         ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO UPDATE SET \
             generation = excluded.generation, \
             welcome_data = excluded.welcome_data, \
             submitted_by = excluded.submitted_by, \
             delivered = 0",
        libsql::params![
            ulid::Ulid::new().to_string(),
            conversation_id.to_string(),
            generation,
            recipient_id.to_string(),
            welcome.to_vec(),
            recipient_device_id.to_string(),
            submitted_by.map(|s| s.to_string()),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WelcomeUpsertOutcome::Ok)
}

#[cfg(test)]
mod conversation_namespace_tests {
    //! One id must never name two kinds of conversation (#880).

    use super::*;
    use libsql::Connection;

    /// 000017's guard triggers (#948) refuse unregistered rows going forward, but
    /// these tests pin rows that PREDATE the registry, so the fixture stops one
    /// migration short.
    const GUARD_TRIGGERS_VERSION: u32 = 17;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Foreign keys off, as in production (`Db::connect_local` does the same).
        c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        c.execute_batch(pollis_schema::BASELINE_SQL).await.expect("baseline");
        for (_, _, sql) in pollis_schema::POST_BASELINE_MIGRATIONS
            .iter()
            .filter(|(v, _, _)| *v < GUARD_TRIGGERS_VERSION)
        {
            c.execute_batch(sql).await.expect("pre-948 schema");
        }
        c
    }

    /// The exact attack: a group exists; its id must be refused everywhere else.
    #[tokio::test]
    async fn an_existing_group_id_is_taken_for_every_conversation_kind() {
        let c = conn().await;
        c.execute("INSERT INTO groups (id, name, owner_id) VALUES ('victim-group', 'grp', 'owner')", ())
            .await
            .unwrap();

        assert!(
            conversation_id_taken(&c, "victim-group").await.unwrap(),
            "a group's id must be refused to dm/create and channel/create — \
             otherwise is_member() grants membership of the group"
        );
    }

    #[tokio::test]
    async fn dm_and_channel_ids_are_taken_too() {
        let c = conn().await;
        c.execute("INSERT INTO dm_channel (id, created_by) VALUES ('a-dm', 'creator')", ())
            .await
            .unwrap();
        c.execute(
            "INSERT INTO channels (id, group_id, name) VALUES ('a-channel', 'g', 'chan')",
            (),
        )
        .await
        .unwrap();

        assert!(conversation_id_taken(&c, "a-dm").await.unwrap());
        assert!(conversation_id_taken(&c, "a-channel").await.unwrap());
    }

    /// A fresh id is still free — without this the guard could be "refuse
    /// everything" and the tests above would still pass.
    #[tokio::test]
    async fn an_unused_id_is_free() {
        let c = conn().await;
        c.execute("INSERT INTO groups (id, name, owner_id) VALUES ('some-group', 'grp', 'owner')", ())
            .await
            .unwrap();

        assert!(
            !conversation_id_taken(&c, "a-brand-new-ulid").await.unwrap(),
            "an unused id must remain creatable"
        );
    }

    /// The property the guard protects: sharing an id across two tables is what
    /// makes `is_member` answer for the wrong conversation.
    #[tokio::test]
    async fn a_shared_id_would_grant_membership_of_the_other_conversation() {
        let c = conn().await;
        // The victim's group — mallory is NOT a member.
        c.execute("INSERT INTO groups (id, name, owner_id) VALUES ('victim-group', 'grp', 'owner')", ())
            .await
            .unwrap();
        // What the attack inserts if the guard is absent.
        c.execute("INSERT INTO dm_channel (id, created_by) VALUES ('victim-group', 'creator')", ())
            .await
            .unwrap();
        c.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
             VALUES ('victim-group', 'mallory', 'creator')",
            (),
        )
        .await
        .unwrap();

        assert!(
            is_member(&c, "victim-group", "mallory").await.unwrap(),
            "documents WHY the guard exists: with the id shared, is_member \
             answers true for a group mallory was never in"
        );
    }
}
