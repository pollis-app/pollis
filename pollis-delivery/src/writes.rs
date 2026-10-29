//! MLS control-plane writes beyond `POST /v1/commits` (#420 W4–W8), and the
//! shared auth gate every device-signed handler uses.
//!
//! Writes land on `state.log_db`; membership/auth lookups read `state.db`.

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

// Request bodies live in `pollis-api` so client and server share one
// declaration; re-exported for handlers, tests and the flows harness.
pub use pollis_api::writes::*;

/// Method, path, headers and the raw body: exactly what a device signature
/// covers.
///
/// The body must be verified before it is deserialized, so `Json<T>` is not
/// usable on these routes. `Bytes` is extracted last (it consumes the body), so
/// rejections are the same `DefaultBodyLimit` / 400 as separate extractors.
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
        // Cloned, not moved, so `Bytes::from_request` gets the request intact.
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

/// The authenticated user, or `None` when auth is disabled.
pub(crate) type Authed = Option<String>;

/// The one auth gate for every write domain: when `require_auth` is on, verify
/// the device signature over the raw body and return the `user_id`; `None` when
/// off. `Ok(Err(resp))` is a rejection the caller forwards unchanged.
pub(crate) async fn gate(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    // Cached pubkey lookup (#658); a connection is checked out only on a miss
    // (#1500).
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

/// [`gate`], also accepting a verified OTP session (`X-Pollis-Session`), for
/// account-lifecycle writes from a pre-enrollment device (no registered signing
/// key). `user_id` binds from the session record, never the body.
///
/// A request carrying the signature header always takes the signature gate, so
/// a bad signature is never rescued by a session token.
pub(crate) async fn gate_or_session(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    Ok(gate_or_session_kind(state, req).await?.map(|(authed, _)| authed))
}

/// Which credential [`gate_or_session`] accepted. `/v1/account/rotate-identity`
/// is a key rotation under a signature but the soft reset under a bare session
/// (`account::apply_rotate_identity`), so the gate must say which.
///
/// Both `device_id`s are server-verified: the signed `X-Pollis-Device`, or the
/// device the session was minted for.
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

/// [`gate`], then parse the body.
///
/// The order is the point: the signature is verified over the raw bytes before
/// deserializing, so a forged body never reaches `serde`. `Ok(Err(resp))` is a
/// 401/403/400 the caller forwards unchanged.
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

/// [`gate_or_session_and_parse`], also reporting which credential authenticated
/// the request.
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

/// The authenticated `(user_id, device_id)`, or `None` on the no-auth path.
pub(crate) type AuthedIdentity = Option<(String, String)>;

/// [`gate`] that also returns the verified `device_id`, for writes keyed on a
/// device (e.g. `/v1/watermarks/advance`), so the handler binds the body's
/// `device_id` to the signer instead of trusting it.
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

/// 200 with the one success body `B`'s endpoint declares (#922).
///
/// Taking `B::Response` ties the answer to the type the client decodes through
/// `ds_post_json::<B>`, so the two ends cannot name different shapes. `pub` so
/// the compile-fail doctest (an external crate) can prove it.
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

/// 200 with no body, for endpoints declared [`pollis_api::Empty`]; the bound
/// stops it answering an endpoint that owes a body.
pub fn ok_empty<B: pollis_api::DsRequest<Response = pollis_api::Empty>>() -> Response {
    StatusCode::OK.into_response()
}

/// The result of an authorized write. Genuine failures are an `Err` on the
/// enclosing `anyhow::Result` (500). `pub` so the integration harness can match
/// on it.
#[derive(Debug)]
pub enum WriteOutcome {
    Ok,
    Forbidden,
    /// An envelope write named a `(generation, epoch)` that is not the commit
    /// log's head (#1041); nothing is stored and the client must catch up and
    /// re-seal (409 [`pollis_api::messages::EpochBehind`]).
    EpochBehind {
        head_generation: i64,
        head_epoch: i64,
    },
    /// A well-formed body carrying an inadmissible value (400), e.g. a
    /// non-canonical or future-skewed stamp (`messages::check_cursor_stamp`).
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

/// Map a [`WriteOutcome`] to 200 / 403 / 409 / 400.
///
/// Bounded on `Response = StatusOk` (#922), so an endpoint declaring a richer
/// body cannot answer a bare `{"status":"ok"}` through here.
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
/// Errs with [`WriteOutcome::Forbidden`] so `apply_*` fns can `?`-bail.
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

/// Is `user_id` a current member of MLS conversation `conversation_id`?
///
/// The id may be a group id (its text channels share one MLS group), a DM
/// channel id, or a channel id (e.g. a voice channel, via its owning group).
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

/// Is `user_id` in this conversation's [`crate::directory::desired_roster`]
/// (members plus pending invitees)? The gate for Welcomes.
///
/// [`is_member`] would refuse the Welcome staged at invite time, which exists so
/// accepting does not need the inviter online.
///
/// Security (C1): an attacker can invite a victim and Welcome them, which the DS
/// cannot tell from an honest invite; server-side, a Welcome may only go to
/// someone the conversation is trying to admit. The forged-Welcome half is the
/// client's: `join_from_welcome` checks the blob's `GroupId` against the row's
/// `conversation_id`.
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

/// The conversation kinds sharing one id namespace (#880): the three legs
/// [`is_member`] ORs over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationKind {
    Dm,
    Group,
    Channel,
}

impl ConversationKind {
    /// The `conversation.kind` value; migration `000016`'s CHECK admits only
    /// these three.
    pub fn as_str(self) -> &'static str {
        match self {
            ConversationKind::Dm => "dm",
            ConversationKind::Group => "group",
            ConversationKind::Channel => "channel",
        }
    }
}

/// Claim `id` for `kind` in the `conversation` registry (#880).
///
/// **Call inside the transaction that inserts the row the id names**: the
/// registry's primary key refuses a second claim, so claim and row must succeed
/// or fail together.
///
/// Errs if the id was ever registered, even for a deleted conversation: the
/// registry is append-only because, with `foreign_keys=OFF`, a deleted group's
/// `group_member`/`channels` rows would pass to the next claimant. This is the
/// backstop when [`conversation_id_taken`] is skipped or a body carries two
/// equal ids.
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

/// True when `id` is in use as any conversation kind or claimed in the registry.
///
/// Create endpoints take the id from the request body and [`is_member`] ORs over
/// all three tables on one id, so a DM reusing a victim group's id would grant
/// membership of that group. Every `create_*` path calls this first so the
/// refusal is a 403, not the registry's constraint error; the table legs cover
/// rows that predate the registry.
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

/// Hard ceiling on one GroupInfo blob: far above any real ratchet tree, small
/// enough that a member cannot park storage in the per-conversation slot.
pub const GROUP_INFO_MAX_BYTES: usize = 1024 * 1024;

/// The verdict of a control-plane write naming a `(generation, epoch)`; the
/// ahead-of-head answer carries the head so the caller can catch up.
#[derive(Debug)]
pub enum HeadBoundedOutcome {
    /// `affected` is 0 when the monotone CAS declined a stale republish (a
    /// normal race, not an error).
    Ok { affected: u64 },
    /// The named key sits ABOVE the log's head — an unreachable future epoch.
    AheadOfHead {
        head_generation: i64,
        head_epoch: i64,
    },
    /// `&'static str` so no caller input is ever echoed back.
    Invalid(&'static str),
}

/// The ceiling `(head_generation, head_epoch_in(generation))` a control-plane
/// write may not exceed.
///
/// `mls_group_info` and `pin_keystate` are single rows behind a strictly-greater
/// `(generation, epoch)` CAS; unbounded, one member could publish
/// `generation = 2^62` and wedge external joins and pin re-wraps forever. The
/// commit log decides which epochs exist, so the ceiling is its head
/// (`MAX(epoch) + 1` in the lineage; 0/0 for an empty log).
///
/// Read from the log DB, inside the write's transaction where it lands there.
/// The head only advances, so a stale read only makes the check stricter.
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
        // The named lineage does not exist; report the head lineage's position.
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

/// Map a [`HeadBoundedOutcome`] to 200 / 409 (the `/v1/commits`
/// [`pollis_api::messages::EpochBehind`] shape) / 400.
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

/// Decode and [`upsert_group_info`] without auth, for the integration harness.
/// A decode failure is an `Err` here; the handler maps it to 400 itself.
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

/// `(generation, epoch)`-monotone UPSERT of a conversation's GroupInfo,
/// compared lexicographically.
///
/// The generation term matters at a suite migration (#454): the successor
/// lineage restarts at epoch 0, below the retired one's last epoch. The CAS is
/// also ceilinged at the log head ([`refuse_above_head`], same transaction).
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

/// Mark `welcome_ids` delivered, scoped to `recipient`.
pub async fn ack_welcomes(
    log_conn: &Connection,
    recipient: &str,
    welcome_ids: &[String],
) -> anyhow::Result<u64> {
    if welcome_ids.is_empty() {
        return Ok(0);
    }

    // Chunked (#916), one slot reserved for `recipient`: the list comes off the
    // wire, and a statement over SQLite's parameter limit fails to prepare.
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

/// Re-arm `recipient`'s Welcomes for redelivery; `device_id` scopes it to one
/// device (plus unscoped rows).
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

/// Delete all of `recipient`'s Welcomes.
pub async fn purge_welcomes(log_conn: &Connection, recipient: &str) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "DELETE FROM mls_welcome WHERE recipient_id = ?1",
            libsql::params![recipient.to_string()],
        )
        .await?)
}

/// Hard ceiling on one Welcome blob; it carries the ratchet tree, so the same
/// bound as a GroupInfo.
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

/// True when `user_id` is an admin of the group owning `conversation_id` (a
/// group or channel id; a DM has no admins).
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

/// POST /v1/welcomes/resubmit — idempotently (re)insert one Welcome for
/// `(conversation_id, recipient_id, recipient_device_id)`, so a lost Welcome can
/// be re-driven without relying on external join.
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

/// The resubmit decision and write, behind four gates:
///
///   1. The actor is a current member (skipped on the no-auth path).
///   2. The recipient is in the desired roster, so a member cannot park a
///      Welcome for an arbitrary user/device. Checked on both paths: it is a
///      property of the row, not the credential.
///   3. The lineage exists ([`refuse_above_head`]).
///   4. The tuple is UNIQUE, so a resubmit is an overwrite: someone else's
///      pending Welcome may be replaced only by the head commit's author (who
///      performed the Add) or a group admin.
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
    // Same predicate `/v1/commits` applies to bundle Welcomes.
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
/// refreshes the blob and re-arms delivery. Same rule as the commit bundle's
/// inline insert.
///
/// When `may_override` is false, an undelivered row from a different
/// `submitted_by` is refused; read and write share one IMMEDIATE transaction so
/// racing resubmits cannot both pass.
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
                // Only a pending Welcome someone else published is defended;
                // unattributed (pre-000006) and delivered rows are fair game.
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
    //! One id must never name two kinds of conversation (see
    //! [`conversation_id_taken`]).

    use super::*;
    use libsql::Connection;

    /// 000017's guard triggers (#948) refuse unregistered rows; these tests pin
    /// rows that predate the registry, so the fixture stops one migration short.
    const GUARD_TRIGGERS_VERSION: u32 = 17;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Match production Turso (foreign keys off); local libsql defaults on.
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
