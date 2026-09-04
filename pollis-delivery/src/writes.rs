//! The MLS control-plane write endpoints beyond `POST /v1/commits`.
//!
//! These cover the non-`direct_submit` client writes (W4–W8 in
//! `docs/goal-a-commit-log-sole-writer.md`) that must move behind the DS once
//! the client holds only a read-only token on the log DB:
//!
//!   - `POST /v1/group-info`     — republish GroupInfo (W4), epoch-monotone.
//!   - `POST /v1/welcomes/ack`   — mark Welcomes delivered (W5).
//!   - `POST /v1/welcomes/reset` — re-arm Welcomes for redelivery (W6/W7).
//!   - `POST /v1/welcomes/purge` — delete all of a user's Welcomes (W8).
//!
//! Every endpoint is gated by `require_auth` exactly like `/v1/commits`: when
//! enforced, the request is device-signature-verified and the authenticated
//! user must equal the actor/owner the write targets. All writes land on
//! `state.log_db`; membership/auth lookups read `state.db` (the main DB, where
//! `user_device` and group/DM membership live).

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

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::writes::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::writes::*;

// ── The request the signature covers ─────────────────────────────────────────

/// Method, path, headers and the **raw** body bytes — exactly the material a
/// device signature is computed over, in one extractor.
///
/// Every `POST /v1/...` handler in this crate needs all four and nothing else,
/// because the signature binds `sha256(body)` and the canonical signing message
/// covers the method and path: the body cannot be deserialized before it is
/// verified, so `Json<T>` is not available to these routes and the parts have to
/// arrive raw. Ninety-odd handlers therefore declared the same four extractors
/// positionally, which is four chances each to reorder them into a different
/// meaning and no place to say what they are *for*.
///
/// `Bytes` is extracted last, as it must be (it consumes the body), so this
/// rejects exactly what the four separate extractors rejected — the same
/// `DefaultBodyLimit` and the same 400 on a body that cannot be read.
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
        // Cloned, not moved out of `parts`: `Bytes::from_request` is handed the
        // request back intact, exactly as it was when the four separate
        // extractors ran in this order.
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

// ── Shared auth gate ─────────────────────────────────────────────────────────

/// The outcome of the shared auth gate: either an authenticated user (auth
/// enforced + verified), or `None` (auth disabled — the no-auth path).
///
/// `pub(crate)` so sibling write-domain modules (e.g. [`crate::messages`]) reuse
/// the exact same gate type instead of re-declaring the no-auth contract.
pub(crate) type Authed = Option<String>;

/// Run the same auth gate as `submit`: when `require_auth` is on, verify the
/// device signature over the raw body and return the authenticated `user_id`;
/// when off, return `None` (the no-auth test/pre-cutover path).
///
/// Returns `Ok(Err(response))` when auth is enforced but the request is
/// rejected — the caller forwards that response unchanged.
///
/// `pub(crate)` so every write-domain module (commits, welcomes, messages, …)
/// shares ONE auth gate. Adding a new domain must not re-implement
/// `auth::verify_request` plumbing — it calls this.
pub(crate) async fn gate(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    let conn = state.db.conn().await?;
    // Cached pubkey lookup (#658): this gate is the single chokepoint every
    // device-signed endpoint funnels through, so caching here is what removes
    // the per-request Turso round trip fleet-wide. Signature verification is
    // unchanged.
    match auth::verify_request_cached(
        &state.device_keys,
        &conn,
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

/// [`gate`], extended to ALSO accept a verified OTP session (`X-Pollis-Session`)
/// as the authenticating credential — for the account-lifecycle writes reachable
/// from a PRE-ENROLLMENT device (the soft reset offered on the login gate). Such
/// a device by definition has no registered `mls_signature_pub` to sign with:
/// its proof of authorization is the verified-OTP session, exactly like the
/// bootstrap endpoints (`crate::bootstrap`), and `user_id` binds from the
/// session record, never the body. The soft reset was always email-OTP
/// authorized by design — this enforces that server-side instead of trusting
/// the client's direct write.
///
/// A request carrying the signature header goes through the signature gate
/// unconditionally, so an enrolled caller keeps the stronger credential and a
/// bad signature is never "rescued" by a session token.
pub(crate) async fn gate_or_session(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<Authed, Response>, AppError> {
    if !state.require_auth {
        return Ok(Ok(None));
    }
    if req.headers.contains_key(auth::H_SIGNATURE) {
        return gate(state, req).await;
    }
    match crate::session::verify_session(&req.headers, &state.sessions, crate::util::now_unix()) {
        Ok(claims) => Ok(Ok(Some(claims.user_id))),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// [`gate`] followed by parsing the body — the preamble every device-signed
/// endpoint in this crate runs, in one call.
///
/// The ORDER is the point, not the line count: the signature is verified over
/// the RAW bytes and the body is deserialized only afterwards, so a forged body
/// can never reach a `serde` impl, let alone the DB. Sixty-odd handlers used to
/// spell that ordering out by hand, which meant sixty-odd chances to write it
/// the other way round; there is now one place where it is written, and a
/// handler that wants the wrong order has to visibly stop using this.
///
/// Returns `Ok(Err(response))` when the request is rejected (401/403 from the
/// gate, or 400 for a body that will not parse) — the caller forwards it
/// unchanged.
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
    let authed = match gate_or_session(state, req).await? {
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
/// The response half of what `pollis-api` already did for requests. Until #922
/// every handler built its answer as a `json!{}` literal and `pollis-core`
/// declared a private `#[derive(Deserialize)]` mirror of it, which is the same
/// two-hand-maintained-lists bug that produced the actor-field outage — only
/// quieter, because a response field the server renames does not fail on the
/// client, it just arrives absent and defaults.
///
/// Taking `B::Response` (not `impl Serialize`) is the whole point: the client
/// decodes that same associated type through `ds_post_json::<B>`, so the two
/// ends cannot name different shapes. Answering with another endpoint's body is
/// `E0308` here, and reading a field the server does not send is `E0609` there.
///
/// `pub` rather than `pub(crate)` so the compile-fail doctest below — which is
/// an external crate, as every doctest is — can prove that.
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

/// 200 with NO body, for the endpoints declared [`pollis_api::Empty`].
///
/// Separate from [`ok_response`] and bounded on `Response = Empty`, so "this
/// endpoint answers a bare 200" is a checked fact rather than a convention. A
/// handler cannot use this for an endpoint that owes a body, and cannot
/// accidentally start sending `null` for one that does not.
pub fn ok_empty<B: pollis_api::DsRequest<Response = pollis_api::Empty>>() -> Response {
    StatusCode::OK.into_response()
}

// ── Shared write-result helpers (every domain reuses these) ──────────────────

/// The result of an authorized write: either it was applied, or the
/// authenticated user was not allowed to make it (→ 403). A genuine failure (DB
/// error, etc.) is an `Err` on the enclosing `anyhow::Result`, mapped to 500 by
/// the handler. Shared across every write-domain module so the handler ⇆ harness
/// pair stays uniform.
///
/// `pub` (not `pub(crate)`) so the integration-test harness — an external crate —
/// can match on it when driving the `apply_*` fns directly.
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

/// Map a [`WriteOutcome`] to the HTTP response (200 ok / 403 forbidden / 409
/// epoch-behind).
///
/// Generic over the endpoint's request type since #922, which is what ties this
/// shared success body to a specific route: `B::Response` must be
/// [`pollis_api::StatusOk`] for `outcome_response::<B>` to typecheck, so an
/// endpoint whose table row declares a richer body cannot silently answer a bare
/// `{"status":"ok"}` through the shared helper. One turbofish per call site, and
/// it is the only thing that makes the ~50 endpoints sharing this path as
/// strongly typed as the handful that build their own body.
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
    })
}

/// Resolve the actor a write is performed as.
///
///   - auth ON  → the authenticated user. If the body also carries an actor id,
///     it must equal the authenticated user (else `Forbidden`).
///   - auth OFF → the body's actor id (no signed identity on the no-auth path).
///     Missing/empty → `Forbidden`.
///
/// Returns the actor or [`WriteOutcome::Forbidden`] so `apply_*` fns can `?`-bail
/// uniformly. (Distinct from [`resolve_recipient`], which returns an HTTP
/// response for the welcome endpoints' 400-on-missing contract.)
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
///
/// Takes a bare [`Connection`] (the main DB) so the same gate is reusable from
/// the integration harness, which drives a single shared connection rather than
/// the DS's `AppState`.
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

// ── the conversation-id namespace (#880) ─────────────────────────────────────

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

/// Claim `id` for `kind` in the `conversation` registry (#880), the table that
/// owns the conversation-id namespace.
///
/// **Call this inside the transaction that inserts the row the id names, never
/// beside it.** The registry's primary key is what refuses a second claim, so
/// the claim and the row it describes have to succeed or fail together —
/// otherwise a rejected claim leaves the conversation created anyway, which is
/// the whole hole.
///
/// Returns `Err` when the id is already registered, as any kind, *including one
/// whose conversation has since been deleted*: the registry is append-only, so a
/// claimed id is claimed forever. Releasing it would re-open the reuse window,
/// and with `foreign_keys=OFF` in production a deleted group leaves its
/// `group_member` and `channels` rows behind for the next claimant to inherit.
///
/// [`conversation_id_taken`] still runs first on every create path, so the
/// ordinary case is a clean 403 rather than a 500. This is what holds when it is
/// not consulted — a create path that forgets it, or a body carrying two ids
/// that are equal to each other, which no lookup of existing rows can catch.
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

/// True when `id` is already in use as ANY kind of conversation — a DM channel,
/// a group, or a channel, or a registry claim outliving the conversation that
/// made it.
///
/// The three tables have separate primary keys, so SQLite happily accepts the
/// same id in all three. That is not a ULID collision (they do not collide by
/// chance); every creation endpoint takes the id straight from the request body,
/// so an attacker simply *names* an existing conversation. [`is_member`] then
/// ORs across all three tables on one id, so creating a DM whose id is a victim
/// group's id and adding yourself to it makes `is_member(victim_group, you)`
/// true — membership of a group you were never in, and with it commit injection,
/// GroupInfo/Welcome overwrite, and a LiveKit token for their room.
///
/// Every `create_*` path calls this before inserting, and it is no longer the
/// only thing standing between an attacker and a shared id: since #880 the
/// `conversation` registry claims the id in the same transaction, so this is the
/// chokepoint that turns the refusal into a 403 instead of a constraint error.
/// It queries the registry as well as the three tables because the two answer
/// slightly different questions — the registry remembers retired ids, and the
/// three tables cover any row that predates the registry.
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

// ── W4 — POST /v1/group-info ─────────────────────────────────────────────────

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
    upsert_group_info(
        &conn,
        &parsed.conversation_id,
        parsed.generation,
        parsed.epoch,
        &gi,
        &parsed.updated_by_device_id,
    )
    .await?;

    Ok(ok_response::<GroupInfoBody>(pollis_api::StatusOk::Ok))
}

/// Decode a parsed [`GroupInfoBody`]'s base64 GroupInfo and UPSERT it (the W4
/// write minus auth/authz). Exposed so the integration harness reuses the exact
/// decode + write without re-implementing base64 handling. A decode failure is
/// an `Err` here (the axum handler maps the same case to 400 itself, before
/// calling [`upsert_group_info`], so its 400-vs-500 behavior is unchanged).
pub async fn apply_group_info(log_conn: &Connection, body: &GroupInfoBody) -> anyhow::Result<u64> {
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
/// compared lexicographically: older GroupInfo can never clobber newer. The
/// generation term is load-bearing at a suite migration (#454 P4) — the successor
/// lineage restarts at epoch 0, NUMERICALLY BELOW the retired lineage's last
/// epoch, so an epoch-only guard would reject the successor's GroupInfo and
/// strand every externally-joining device on the retired suite. Pure conn-level
/// write so the integration harness can reuse it against its shared log
/// connection.
pub async fn upsert_group_info(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
    group_info: &[u8],
    updated_by_device_id: &str,
) -> anyhow::Result<u64> {
    let affected = log_conn
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
    Ok(affected)
}

// ── W5 — POST /v1/welcomes/ack ───────────────────────────────────────────────

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

    // `id IN (?2, ?3, …)` with the recipient bound first so the filter can never
    // touch another user's Welcomes.
    //
    // Chunked (#916) with ONE slot reserved for `recipient`. This list comes off
    // the wire — a device acking a long backlog, or a caller sending an
    // arbitrarily long one — so it is the batched query in the workspace most
    // able to exceed SQLite's parameter limit, and going over does not degrade:
    // the statement fails to prepare and the Welcomes never get marked
    // delivered.
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

// ── W6/W7 — POST /v1/welcomes/reset ──────────────────────────────────────────

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

// ── W8 — POST /v1/welcomes/purge ─────────────────────────────────────────────

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

// ── POST /v1/welcomes/resubmit (issue #430 P2) ───────────────────────────────

/// POST /v1/welcomes/resubmit — idempotently (re)insert a single Welcome for
/// `(conversation_id, recipient_id, recipient_device_id)`, so a recipient whose
/// Welcome went missing can be re-driven WITHOUT depending solely on the client's
/// external-join fallback. When auth is enforced, the authenticated user must be
/// a current member of the conversation (mirrors `/v1/group-info` authz — only a
/// member can re-drive a Welcome for the group they belong to).
pub async fn welcomes_resubmit(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ResubmitBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };

    // Authz: a signed request may only resubmit for a conversation it belongs to.
    // Skipped on the no-auth path (mirrors submit / group_info).
    if let Some(user_id) = &authed {
        let conn = state.db.conn().await?;
        if !is_member(&conn, &parsed.conversation_id, user_id).await? {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let welcome = match b64_decode(&parsed.welcome) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid welcome")),
    };

    let conn = state.log_db.conn().await?;
    upsert_welcome(
        &conn,
        &parsed.conversation_id,
        parsed.generation,
        &parsed.recipient_id,
        &parsed.recipient_device_id,
        &welcome,
    )
    .await?;

    Ok(ok_response::<ResubmitBody>(pollis_api::StatusOk::Ok))
}

/// Idempotent (re)insert of a Welcome, keyed on the UNIQUE
/// `(conversation_id, recipient_id, recipient_device_id)` tuple added in
/// migration 000002 (commit-log DB): a resend for the same tuple refreshes the blob and re-arms
/// delivery (`delivered = 0`) rather than duplicating or erroring. Mirrors the
/// inline Welcome insert in the commit bundle so both writers of `mls_welcome`
/// obey one rule. Pure conn-level write reused by the harness.
pub async fn upsert_welcome(
    log_conn: &Connection,
    conversation_id: &str,
    generation: i64,
    recipient_id: &str,
    recipient_device_id: &str,
    welcome: &[u8],
) -> anyhow::Result<u64> {
    Ok(log_conn
        .execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, generation, recipient_id, welcome_data, recipient_device_id, delivered) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0) \
             ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO UPDATE SET \
                 generation = excluded.generation, \
                 welcome_data = excluded.welcome_data, \
                 delivered = 0",
            libsql::params![
                ulid::Ulid::new().to_string(),
                conversation_id.to_string(),
                generation,
                recipient_id.to_string(),
                welcome.to_vec(),
                recipient_device_id.to_string(),
            ],
        )
        .await?)
}

// ── helpers ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod conversation_namespace_tests {
    //! One id must never name two kinds of conversation.
    //!
    //! Not a ULID-collision concern — ULIDs do not collide by chance. Every
    //! `create_*` endpoint takes the id straight from the request body, so an
    //! attacker *names* an existing conversation rather than colliding with it.
    //! Because `dm_channel`, `groups` and `channels` are separate tables with
    //! separate primary keys, SQLite accepts the same id in all three, and
    //! `is_member` ORs across all three on one id — so a DM whose id is a
    //! victim group's id grants membership of that group.

    use super::*;
    use libsql::Connection;

    /// 000017's guard triggers (#948) make an unregistered row in the three
    /// tables unrepresentable going FORWARD — but the three-table legs of
    /// `conversation_id_taken` exist for rows that PREDATE the registry, and
    /// these tests pin exactly that state. So the fixture stops one migration
    /// short of the triggers; seeding a bare table row on the full schema
    /// would simply be refused by the database now (which is pinned by
    /// tests/conversation_namespace.rs, not here).
    const GUARD_TRIGGERS_VERSION: u32 = 17;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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
