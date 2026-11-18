//! Bots (#1182): a bot is a user scoped to one group, and its hosts are its
//! devices. Schema and the invalid states it refuses: migration `000052_bots.sql`.
//!
//! ## Pairing (first host = creation)
//!
//! 1. The host files a [`BotPairingRequestBody`] (unauthenticated, rate-limited,
//!    TTL [`PAIRING_TTL_SECS`]) carrying its ephemeral X25519 key and the bot's
//!    account key, both generated on the host; it prints a code derived from
//!    the ephemeral key.
//! 2. An admin's client looks the request up by the code's hash
//!    ([`apply_pairing_lookup`]), derives the code from the ephemeral key it
//!    reads back and refuses on a mismatch.
//! 3. [`apply_create_bot`] creates the bot user, its `bot` row, the host's
//!    device and the membership in one transaction.
//! 4. The host awaits [`apply_pairing_status`], then publishes its device cert
//!    (`/v1/auth/publish-device-cert`, cert-validity gate) and key packages.
//!
//! ## Adding a host
//!
//! The new host files an `ExistingBot` request; an admin stores a signed host
//! approval on it ([`apply_approve_bot_host`]); the bot's hosts read it
//! ([`pending_bot_hosts`]), verify it themselves, and one wraps the account key
//! ([`apply_wrap_bot_host`]), which also registers the new device. The DS only
//! stores and relays, but it refuses an approver who is not a current admin of
//! the bot's group, at approval and again at the wrap.
//!
//! ## Deletion
//!
//! Through the account-deletion path ([`delete_bot_in_tx`]), users row first
//! (the schema's delete order). Group teardown deletes a group's bots the same way.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;
use pollis_device_cert::constant_time_eq;
use sha2::{Digest, Sha256};

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate_and_parse, gate_identity_and_parse, ok_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

pub use pollis_api::bots::*;

/// How long a pairing request stays open, and how long an acted-on one stays
/// readable by its host. The same ten minutes as device enrollment.
pub const PAIRING_TTL_SECS: u64 = 10 * 60;

/// Open pairing requests the whole DS may hold. Filed pre-credential, so the
/// bound is global, like `links::MAX_OPEN_OFFERS`; the per-IP limit is the
/// `probe` tier.
pub const MAX_OPEN_PAIRINGS: i64 = 10_000;

/// How long a trigger claim is kept. A host that sees a trigger later than this
/// would claim it afresh; triggers are handled within seconds, so a week is far
/// past any redelivery.
pub const TRIGGER_CLAIM_RETENTION_SECS: u64 = 7 * 24 * 3600;

const ML_DSA_44_PUB_LEN: usize = pollis_device_cert::MLDSA44_PUB_LEN;

// ---------------------------------------------------------------- shared reads

/// Whether `user_id` is a bot. Keyed on the `bot` row, which the schema keeps in
/// step with `users.kind`.
pub async fn is_bot(conn: &Connection, user_id: &str) -> anyhow::Result<bool> {
    Ok(bot_group(conn, user_id).await?.is_some())
}

/// The group a bot belongs to, or `None` when `user_id` is not a bot.
pub async fn bot_group(conn: &Connection, user_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT group_id FROM bot WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get(0)?),
        None => None,
    })
}

/// The bots of a group.
pub async fn bots_of_group(conn: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    crate::util::query_strings(
        conn,
        "SELECT user_id FROM bot WHERE group_id = ?1 ORDER BY user_id",
        libsql::params![group_id.to_string()],
    )
    .await
}

/// Whether `user_id` is a current admin of the group `bot_user_id` belongs to.
pub async fn is_admin_of_bot(
    conn: &Connection,
    user_id: &str,
    bot_user_id: &str,
) -> anyhow::Result<bool> {
    match bot_group(conn, bot_user_id).await? {
        Some(group_id) => crate::groups::is_admin(conn, &group_id, user_id).await,
        None => Ok(false),
    }
}

/// A bot's email: the RFC 2606 `.invalid` placeholder `users.email NOT NULL
/// UNIQUE` needs, which can never receive mail. Exactly what the schema requires.
pub fn bot_email(user_id: &str) -> String {
    format!("bot-{user_id}@bot.invalid").to_lowercase()
}

/// Whether an address is at `bot.invalid` or under it — a bot's address, never
/// a person's. The same test as the schema's human-email triggers.
pub fn is_bot_address(email: &str) -> bool {
    let e = email.trim().to_lowercase();
    e.ends_with("@bot.invalid") || e.ends_with(".bot.invalid")
}

/// A bot's username: its name squeezed into `profile::is_valid_username`'s
/// shape, then the lowercased tail of its ULID for uniqueness.
pub fn bot_username(name: &str, user_id: &str) -> String {
    let suffix = user_id[user_id.len().saturating_sub(10)..].to_ascii_lowercase();
    let room = crate::profile::USERNAME_MAX_LEN - 1 - suffix.len();
    let mut stem: String = name
        .chars()
        .map(|c| c.to_ascii_lowercase())
        .map(|c| if crate::profile::is_username_char(c) { c } else { '_' })
        .take(room)
        .collect();
    if stem.trim_matches('_').is_empty() {
        stem = "bot".to_string();
    }
    format!("{stem}_{suffix}")
}

fn decode_exact(b64: &str, len: usize) -> Option<Vec<u8>> {
    match crate::util::b64_decode(b64) {
        Ok(v) if v.len() == len => Some(v),
        _ => None,
    }
}

fn clean_device_name(name: Option<&str>) -> Option<String> {
    name.map(|n| crate::util::redact_ip_literals(&n.chars().take(64).collect::<String>()))
        .filter(|n| !n.trim().is_empty())
}

fn valid_device_id(id: &str) -> bool {
    !id.trim().is_empty() && id.len() <= 64
}

/// Record a DS-authored security event. Not counted against the client budget.
pub(crate) async fn security_event(
    conn: &Connection,
    user_id: &str,
    kind: &str,
    device_id: Option<&str>,
    metadata: serde_json::Value,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            ulid::Ulid::new().to_string(),
            user_id.to_string(),
            kind.to_string(),
            device_id.map(str::to_string),
            metadata.to_string(),
        ],
    )
    .await?;
    Ok(())
}

fn internal(e: impl Into<anyhow::Error>) -> Response {
    AppError::from(e.into().context("bots")).into_response()
}

fn conflict(msg: &str) -> Response {
    (StatusCode::CONFLICT, Json(serde_json::json!({ "error": msg }))).into_response()
}

fn refused() -> Response {
    (StatusCode::UNAUTHORIZED, Json(serde_json::json!({ "error": "invalid or expired pairing request" })))
        .into_response()
}

/// The verified `(user_id, device_id)`. Every bot endpoint that records or
/// binds a device requires a real signature, so the no-auth path answers 401.
#[allow(clippy::result_large_err)]
fn signed(identity: Option<(String, String)>) -> Result<(String, String), Response> {
    identity.ok_or_else(|| AuthRejection::Unauthorized.into_response())
}

// ---------------------------------------------------------------- pairing request

/// What [`apply_pairing_request`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum PairingRequestOutcome {
    Created { request_id: String, expires_at: i64 },
    Invalid(&'static str),
    /// The DS holds [`MAX_OPEN_PAIRINGS`] open requests.
    Full,
    /// Another open request has the same code.
    CodeTaken,
}

/// POST /v1/bots/pairing/request — unauthenticated; rate-limited per IP (`probe`).
pub async fn pairing_request(State(state): State<AppState>, _headers: HeaderMap, body: Bytes) -> Response {
    let parsed: BotPairingRequestBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_pairing_request(&conn, &parsed, crate::util::now_unix()).await {
        Ok(PairingRequestOutcome::Created { request_id, expires_at }) => {
            ok_response::<BotPairingRequestBody>(BotPairingRequestResponse { request_id, expires_at })
        }
        Ok(PairingRequestOutcome::Invalid(why)) => bad_request(why),
        Ok(PairingRequestOutcome::Full) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "cannot hold another pairing request right now" })),
        )
            .into_response(),
        Ok(PairingRequestOutcome::CodeTaken) => conflict("code_taken"),
        Err(e) => internal(e),
    }
}

/// Validate and store a pairing request; the DS picks its id and expiry.
pub async fn apply_pairing_request(
    conn: &Connection,
    body: &BotPairingRequestBody,
    now: u64,
) -> anyhow::Result<PairingRequestOutcome> {
    use PairingRequestOutcome::Invalid;
    let Some(code_lookup) = decode_exact(&body.code_lookup, 32) else {
        return Ok(Invalid("code_lookup must be 32 bytes"));
    };
    let Some(poll_verifier) = decode_exact(&body.poll_verifier, 32) else {
        return Ok(Invalid("poll_verifier must be 32 bytes"));
    };
    let Some(ephemeral_pub) = decode_exact(&body.ephemeral_pub, 32) else {
        return Ok(Invalid("ephemeral_pub must be 32 bytes"));
    };
    if !valid_device_id(&body.device_id) {
        return Ok(Invalid("invalid device_id"));
    }
    let device_name = clean_device_name(body.device_name.as_deref());
    let (kind, account_id_pub, bot_user_id, group_id) = match &body.target {
        PairingTarget::NewBot { account_id_pub } => {
            let Some(pub_bytes) = decode_exact(account_id_pub, ML_DSA_44_PUB_LEN) else {
                return Ok(Invalid("account_id_pub must be an ML-DSA-44 verifying key"));
            };
            ("create", Some(pub_bytes), None, None)
        }
        PairingTarget::ExistingBot { bot_user_id } => {
            let Some(group_id) = bot_group(conn, bot_user_id).await? else {
                return Ok(Invalid("unknown bot"));
            };
            ("add_host", None, Some(bot_user_id.clone()), Some(group_id))
        }
    };

    let tx = crate::db::write_tx(conn).await?;
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM bot_pairing_request WHERE expires_at > ?1",
            libsql::params![now as i64],
        )
        .await?;
    let open: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    if open >= MAX_OPEN_PAIRINGS {
        return Ok(PairingRequestOutcome::Full);
    }
    // A lapsed request keeps its code until the sweep; it must not block a new one.
    tx.execute(
        "DELETE FROM bot_pairing_request WHERE code_lookup = ?1 AND expires_at <= ?2",
        libsql::params![code_lookup.clone(), now as i64],
    )
    .await?;
    let request_id = ulid::Ulid::new().to_string();
    let expires_at = now.saturating_add(PAIRING_TTL_SECS) as i64;
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO bot_pairing_request \
             (id, kind, code_lookup, poll_verifier, ephemeral_pub, device_id, device_name, \
              account_id_pub, user_id, group_id, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            libsql::params![
                request_id.clone(),
                kind,
                code_lookup,
                poll_verifier,
                ephemeral_pub,
                body.device_id.clone(),
                device_name,
                account_id_pub,
                bot_user_id,
                group_id,
                expires_at,
            ],
        )
        .await?;
    if inserted == 0 {
        return Ok(PairingRequestOutcome::CodeTaken);
    }
    tx.commit().await?;
    Ok(PairingRequestOutcome::Created { request_id, expires_at })
}

// ---------------------------------------------------------------- lookup

/// POST /v1/bots/pairing/lookup — device-signed by an admin of the group.
pub async fn pairing_lookup(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<BotPairingLookupBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_pairing_lookup(&conn, authed.as_deref(), &parsed, crate::util::now_unix()).await? {
        Ok(request) => Ok(ok_response::<BotPairingLookupBody>(BotPairingLookupResponse { request })),
        Err(_) => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// The open request whose code hashes to `code_lookup`, for an admin of
/// `group_id`. An `ExistingBot` request is visible only through its bot's group.
pub async fn apply_pairing_lookup(
    conn: &Connection,
    authed: Option<&str>,
    body: &BotPairingLookupBody,
    now: u64,
) -> anyhow::Result<Result<Option<BotPairingRequestWire>, WriteOutcome>> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(Err(o)),
    };
    if !crate::groups::is_admin(conn, &body.group_id, &actor).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    let Some(code_lookup) = decode_exact(&body.code_lookup, 32) else {
        return Ok(Ok(None));
    };
    let mut rows = conn
        .query(
            "SELECT id, kind, ephemeral_pub, device_id, device_name, account_id_pub, user_id, expires_at \
             FROM bot_pairing_request \
             WHERE code_lookup = ?1 AND status = 'pending' AND expires_at > ?2 \
               AND (kind = 'create' OR group_id = ?3)",
            libsql::params![code_lookup, now as i64, body.group_id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(Ok(None));
    };
    let kind: String = row.get(1)?;
    let target = if kind == "create" {
        let pub_bytes: Vec<u8> = row.get(5)?;
        PairingTarget::NewBot { account_id_pub: crate::util::b64(&pub_bytes) }
    } else {
        PairingTarget::ExistingBot { bot_user_id: row.get(6)? }
    };
    let ephemeral: Vec<u8> = row.get(2)?;
    Ok(Ok(Some(BotPairingRequestWire {
        request_id: row.get(0)?,
        ephemeral_pub: crate::util::b64(&ephemeral),
        device_id: row.get(3)?,
        device_name: row.get(4)?,
        target,
        expires_at: row.get(7)?,
    })))
}

// ---------------------------------------------------------------- status

/// POST /v1/bots/pairing/status — unauthenticated; the poll secret is the credential.
pub async fn pairing_status(State(state): State<AppState>, _headers: HeaderMap, body: Bytes) -> Response {
    let parsed: BotPairingStatusBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match apply_pairing_status(&conn, &parsed, crate::util::now_unix()).await {
        Ok(Some(status)) => ok_response::<BotPairingStatusBody>(status),
        Ok(None) => refused(),
        Err(e) => internal(e),
    }
}

/// The request's outcome, or `None` for unknown, expired or a wrong secret alike.
pub async fn apply_pairing_status(
    conn: &Connection,
    body: &BotPairingStatusBody,
    now: u64,
) -> anyhow::Result<Option<BotPairingStatus>> {
    let Some(secret) = decode_exact(&body.poll_secret, 32) else {
        return Ok(None);
    };
    let mut rows = conn
        .query(
            "SELECT poll_verifier, kind, status, user_id, group_id, wrapped_account_key, wrapped_by_device_id \
             FROM bot_pairing_request WHERE id = ?1 AND expires_at > ?2",
            libsql::params![body.request_id.clone(), now as i64],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let verifier: Vec<u8> = row.get(0)?;
    let presented: [u8; 32] = Sha256::digest(&secret).into();
    if !constant_time_eq(&presented, &verifier) {
        return Ok(None);
    }
    let kind: String = row.get(1)?;
    let status: String = row.get(2)?;
    if status == "pending" {
        return Ok(Some(BotPairingStatus::Pending));
    }
    // The schema guarantees these are set on an approved row.
    let bot_user_id: String = row.get(3)?;
    let group_id: String = row.get(4)?;
    Ok(Some(if kind == "create" {
        BotPairingStatus::Created { bot_user_id, group_id }
    } else {
        let wrapped: Vec<u8> = row.get(5)?;
        BotPairingStatus::Wrapped {
            bot_user_id,
            group_id,
            wrapped_account_key: crate::util::b64(&wrapped),
            wrapped_by_device_id: row.get(6)?,
        }
    }))
}

// ---------------------------------------------------------------- create

/// What [`apply_create_bot`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateBotOutcome {
    Created { bot_user_id: String, device_id: String, group_id: String },
    LimitReached,
    Forbidden,
    /// The request is not open, is not a new-bot request, names a different
    /// ephemeral key, or its device id is taken.
    Gone,
    Invalid(&'static str),
}

/// POST /v1/bots/create — device-signed by an admin of the group.
pub async fn create_bot(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<CreateBotBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.user_id.as_deref().is_some_and(|u| u != actor) {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    Ok(match apply_create_bot(&conn, &actor, &device, &parsed, crate::util::now_unix()).await? {
        CreateBotOutcome::Created { bot_user_id, device_id, group_id } => {
            ok_response::<CreateBotBody>(BotCreated::Ok { bot_user_id, device_id, group_id })
        }
        CreateBotOutcome::LimitReached => {
            ok_response::<CreateBotBody>(BotCreated::LimitReached { limit: MAX_BOTS_PER_GROUP })
        }
        CreateBotOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        CreateBotOutcome::Gone => conflict("pairing request is not open"),
        CreateBotOutcome::Invalid(why) => bad_request(why),
    })
}

/// Create the bot, its host's device and its membership in ONE transaction, in
/// the schema's insert order (`bot` -> `group_member` -> `users`).
pub async fn apply_create_bot(
    conn: &Connection,
    actor: &str,
    actor_device: &str,
    body: &CreateBotBody,
    now: u64,
) -> anyhow::Result<CreateBotOutcome> {
    if !crate::profile::is_valid_display_name(&body.name) {
        return Ok(CreateBotOutcome::Invalid("invalid name"));
    }
    let description = body.description.as_deref().map(str::trim).filter(|d| !d.is_empty());
    if description.is_some_and(|d| d.chars().count() > 500) {
        return Ok(CreateBotOutcome::Invalid("description is too long"));
    }
    let Ok(ephemeral) = crate::util::b64_decode(&body.ephemeral_pub) else {
        return Ok(CreateBotOutcome::Invalid("invalid ephemeral_pub"));
    };

    let tx = crate::db::write_tx(conn).await?;
    // The schema's insert order (bot, membership, user, device) lets the triggers refuse a bot user
    // without its row and membership; the membership names its user before that row exists, so
    // reference checks wait for COMMIT, where every one holds. Lasts to the end of this
    // transaction; a no-op where foreign keys are not enforced.
    tx.execute("PRAGMA defer_foreign_keys = ON", ()).await?;
    if !crate::groups::is_admin(&tx, &body.group_id, actor).await? {
        return Ok(CreateBotOutcome::Forbidden);
    }

    let mut rows = tx
        .query(
            "SELECT ephemeral_pub, device_id, device_name, account_id_pub FROM bot_pairing_request \
             WHERE id = ?1 AND kind = 'create' AND status = 'pending' AND expires_at > ?2",
            libsql::params![body.request_id.clone(), now as i64],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(CreateBotOutcome::Gone);
    };
    let stored_ephemeral: Vec<u8> = row.get(0)?;
    let device_id: String = row.get(1)?;
    let device_name: Option<String> = row.get(2)?;
    let account_id_pub: Vec<u8> = row.get(3)?;
    drop(rows);
    if !constant_time_eq(&stored_ephemeral, &ephemeral) {
        return Ok(CreateBotOutcome::Gone);
    }

    let existing = crate::util::query_strings(
        &tx,
        "SELECT device_id FROM user_device WHERE device_id = ?1",
        libsql::params![device_id.clone()],
    )
    .await?;
    if !existing.is_empty() {
        return Ok(CreateBotOutcome::Gone);
    }

    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM bot WHERE group_id = ?1",
            libsql::params![body.group_id.clone()],
        )
        .await?;
    let bots: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    if bots >= i64::from(MAX_BOTS_PER_GROUP) {
        return Ok(CreateBotOutcome::LimitReached);
    }

    let bot_user_id = ulid::Ulid::new().to_string();
    tx.execute(
        "INSERT INTO bot (user_id, group_id, created_by, description) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            bot_user_id.clone(),
            body.group_id.clone(),
            actor.to_string(),
            description.map(str::to_string),
        ],
    )
    .await?;
    tx.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'member')",
        libsql::params![body.group_id.clone(), bot_user_id.clone()],
    )
    .await?;
    tx.execute(
        "INSERT INTO users (id, email, username, preferred_name, account_id_pub, identity_version, kind) \
         VALUES (?1, ?2, ?3, ?4, ?5, 1, 'bot')",
        libsql::params![
            bot_user_id.clone(),
            bot_email(&bot_user_id),
            bot_username(&body.name, &bot_user_id),
            body.name.clone(),
            account_id_pub.clone(),
        ],
    )
    .await?;
    // Published to the account-key transparency log like any account's first key.
    tx.execute(
        "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) VALUES (?1, ?2, 1)",
        libsql::params![bot_user_id.clone(), account_id_pub],
    )
    .await?;
    // The host's device, without a cert: the host publishes it after it reads
    // the approval, through the cert-validity gate.
    tx.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3)",
        libsql::params![
            device_id.clone(),
            bot_user_id.clone(),
            device_name.unwrap_or_else(|| "host".to_string()),
        ],
    )
    .await?;
    tx.execute(
        "UPDATE bot_pairing_request \
         SET status = 'approved', user_id = ?1, group_id = ?2, reviewed_by = ?3, \
             approver_device_id = ?4, expires_at = ?5 \
         WHERE id = ?6",
        libsql::params![
            bot_user_id.clone(),
            body.group_id.clone(),
            actor.to_string(),
            actor_device.to_string(),
            now.saturating_add(PAIRING_TTL_SECS) as i64,
            body.request_id.clone(),
        ],
    )
    .await?;
    security_event(
        &tx,
        actor,
        "bot_created",
        Some(actor_device),
        serde_json::json!({ "bot_user_id": bot_user_id, "group_id": body.group_id }),
    )
    .await?;
    security_event(
        &tx,
        &bot_user_id,
        "bot_created",
        Some(&device_id),
        serde_json::json!({ "created_by": actor, "group_id": body.group_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(CreateBotOutcome::Created {
        bot_user_id,
        device_id,
        group_id: body.group_id.clone(),
    })
}

// ---------------------------------------------------------------- list

/// POST /v1/read/bots — device-signed by an admin of the group.
pub async fn list_bots(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ListBotsBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_list_bots(&conn, authed.as_deref(), &parsed).await? {
        Some(bots) => Ok(ok_response::<ListBotsBody>(ListBotsResponse { bots, limit: MAX_BOTS_PER_GROUP })),
        None => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// The group's bots with their hosts; `None` unless the caller is an admin.
pub async fn apply_list_bots(
    conn: &Connection,
    authed: Option<&str>,
    body: &ListBotsBody,
) -> anyhow::Result<Option<Vec<BotWire>>> {
    let Ok(actor) = resolve_actor(authed, body.user_id.as_deref()) else {
        return Ok(None);
    };
    if !crate::groups::is_admin(conn, &body.group_id, &actor).await? {
        return Ok(None);
    }
    let mut bots: Vec<BotWire> = Vec::new();
    let mut rows = conn
        .query(
            "SELECT b.user_id, u.username, u.preferred_name, b.description, b.created_by, b.created_at \
             FROM bot b JOIN users u ON u.id = b.user_id \
             WHERE b.group_id = ?1 ORDER BY b.created_at, b.user_id",
            libsql::params![body.group_id.clone()],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        bots.push(BotWire {
            user_id: row.get(0)?,
            username: row.get(1)?,
            preferred_name: row.get(2)?,
            description: row.get(3)?,
            created_by: row.get(4)?,
            created_at: row.get(5)?,
            hosts: Vec::new(),
        });
    }
    drop(rows);
    for bot in &mut bots {
        let mut rows = conn
            .query(
                "SELECT device_id, device_name, created_at, last_seen, \
                        mls_signature_pub_pq IS NOT NULL, revoked_at \
                 FROM user_device WHERE user_id = ?1 ORDER BY created_at, device_id",
                libsql::params![bot.user_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            bot.hosts.push(BotHostWire {
                device_id: row.get(0)?,
                device_name: row.get(1)?,
                created_at: row.get(2)?,
                last_seen: row.get(3)?,
                certified: row.get::<i64>(4)? != 0,
                revoked_at: row.get(5)?,
            });
        }
    }
    Ok(Some(bots))
}

// ---------------------------------------------------------------- delete

/// POST /v1/bots/delete — device-signed by an admin of the bot's group.
pub async fn delete_bot(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<DeleteBotBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.user_id.as_deref().is_some_and(|u| u != actor) {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    let left = crate::directory::mls_groups_of(&conn, &parsed.bot_user_id).await?;
    let outcome = apply_delete_bot(&conn, &actor, &device, &parsed.bot_user_id).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        crate::account::after_account_deleted(&state, &conn, &parsed.bot_user_id, left).await;
    }
    crate::writes::outcome_response::<DeleteBotBody>(outcome)
}

/// Delete a bot for an admin of its group, through the account-deletion path.
pub async fn apply_delete_bot(
    conn: &Connection,
    actor: &str,
    actor_device: &str,
    bot_user_id: &str,
) -> anyhow::Result<WriteOutcome> {
    let tx = crate::db::write_tx(conn).await?;
    let Some(group_id) = bot_group(&tx, bot_user_id).await? else {
        return Ok(WriteOutcome::Forbidden);
    };
    if !crate::groups::is_admin(&tx, &group_id, actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    delete_bot_in_tx(&tx, bot_user_id).await?;
    security_event(
        &tx,
        actor,
        "bot_deleted",
        Some(actor_device),
        serde_json::json!({ "bot_user_id": bot_user_id, "group_id": group_id }),
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// Every main-DB delete of a bot's account deletion, on an OPEN transaction, in
/// the schema's delete order: `users`, then `bot`, then everything else
/// ([`crate::teardown::purge_user_rows`]). Records the commit-log half owed.
pub async fn delete_bot_in_tx(tx: &Connection, bot_user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![bot_user_id.to_string()];
    // #1874: its group re-checks its tree before the next seal.
    crate::directory::bump_membership_versions_of(tx, bot_user_id).await?;
    // Read while `user_device` still has them, for the commit-log scrub.
    let device_ids = crate::broker::user_device_ids(tx, bot_user_id).await?;
    // The profile picture, read while the users row still names it.
    let avatar = crate::util::query_strings(
        tx,
        "SELECT avatar_url FROM users WHERE id = ?1 AND avatar_url IS NOT NULL",
        uid(),
    )
    .await?;
    for key in &avatar {
        crate::aliases::release_avatar(tx, Some(key)).await?;
    }
    tx.execute("DELETE FROM users WHERE id = ?1", uid()).await?;
    tx.execute("DELETE FROM bot WHERE user_id = ?1", uid()).await?;
    crate::teardown::purge_user_rows(tx, bot_user_id).await?;
    crate::teardown::record_account_log_purge(
        tx,
        bot_user_id,
        crate::teardown::OwedLogPurge::Deletion { device_ids: &device_ids },
        &[],
    )
    .await?;
    Ok(())
}

/// The bot-owned rows [`crate::teardown::purge_user_rows`] clears for any
/// account: a bot's pairing requests and trigger claims, and, for a human, the
/// audit pointers that name them on rows that outlive them.
pub async fn purge_bot_rows_of(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    let uid = || libsql::params![user_id.to_string()];
    conn.execute("DELETE FROM bot_pairing_request WHERE user_id = ?1 OR reviewed_by = ?1", uid())
        .await?;
    conn.execute("DELETE FROM bot_trigger_claim WHERE installation_id = ?1", uid()).await?;
    conn.execute("DELETE FROM bot_app_state WHERE installation_id = ?1", uid()).await?;
    crate::bot_apps::purge_bot_app_rows_of(conn, user_id).await?;
    conn.execute("UPDATE bot SET created_by = NULL WHERE created_by = ?1", uid()).await?;
    Ok(())
}

/// Delete every bot of `group_id` through [`delete_bot_in_tx`], before the group
/// goes (the schema refuses the group's delete while it has bots), and its
/// open add-host requests.
pub async fn purge_group_bots(tx: &Connection, group_id: &str) -> anyhow::Result<Vec<String>> {
    let bots = bots_of_group(tx, group_id).await?;
    for bot in &bots {
        delete_bot_in_tx(tx, bot).await?;
    }
    tx.execute(
        "DELETE FROM bot_pairing_request WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(bots)
}

// ---------------------------------------------------------------- host approval

/// POST /v1/bots/hosts/approve — device-signed by a current admin of the bot's group.
pub async fn approve_bot_host(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<ApproveBotHostBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_approve_bot_host(&conn, &actor, &device, &parsed, crate::util::now_unix()).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        // Nudge the bot's hosts through its inbox; they also read it on catch-up. Best-effort.
        let event = serde_json::json!({ "type": "bot_host_approval", "request_id": parsed.request_id });
        if let Err(e) = crate::broker::room_send_data(&state, &format!("inbox-{}", parsed.bot_user_id), &event).await {
            tracing::warn!("bot host approval: inbox notify failed (non-fatal): {e}");
        }
    }
    crate::writes::outcome_response::<ApproveBotHostBody>(outcome)
}

/// Store the admin's signed host approval on an open `ExistingBot` request for
/// `bot_user_id`. `Forbidden` unless the signer is a current admin of the
/// bot's group and the request is that bot's, open and not yet wrapped.
pub async fn apply_approve_bot_host(
    conn: &Connection,
    actor: &str,
    actor_device: &str,
    body: &ApproveBotHostBody,
    now: u64,
) -> anyhow::Result<WriteOutcome> {
    let approval = match crate::util::b64_decode(&body.host_approval) {
        Ok(a) if !a.is_empty() && a.len() <= 16384 => a,
        _ => return Ok(WriteOutcome::Invalid("invalid host_approval")),
    };
    let tx = crate::db::write_tx(conn).await?;
    let Some(group_id) = bot_group(&tx, &body.bot_user_id).await? else {
        return Ok(WriteOutcome::Forbidden);
    };
    if !crate::groups::is_admin(&tx, &group_id, actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let affected = tx
        .execute(
            "UPDATE bot_pairing_request \
             SET host_approval = ?1, reviewed_by = ?2, approver_device_id = ?3 \
             WHERE id = ?4 AND kind = 'add_host' AND user_id = ?5 AND group_id = ?6 \
               AND status = 'pending' AND expires_at > ?7",
            libsql::params![
                approval,
                actor.to_string(),
                actor_device.to_string(),
                body.request_id.clone(),
                body.bot_user_id.clone(),
                group_id.clone(),
                now as i64,
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(WriteOutcome::Forbidden);
    }
    let meta = serde_json::json!({
        "bot_user_id": body.bot_user_id,
        "request_id": body.request_id,
        "approved_by": actor,
    });
    security_event(&tx, actor, "bot_host_approved", Some(actor_device), meta.clone()).await?;
    security_event(&tx, &body.bot_user_id, "bot_host_approved", None, meta).await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/bots/hosts/pending — device-signed by one of a bot's hosts.
pub async fn pending_bot_hosts(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, _parsed) = match gate_identity_and_parse::<PendingBotHostsBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, _device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_pending_bot_hosts(&conn, &actor, crate::util::now_unix()).await? {
        Some(requests) => Ok(ok_response::<PendingBotHostsBody>(PendingBotHostsResponse { requests })),
        None => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// The approved, unwrapped add-host requests for the signing bot; `None` when
/// the signer is not a bot.
pub async fn apply_pending_bot_hosts(
    conn: &Connection,
    bot_user_id: &str,
    now: u64,
) -> anyhow::Result<Option<Vec<PendingBotHostWire>>> {
    if !is_bot(conn, bot_user_id).await? {
        return Ok(None);
    }
    let mut rows = conn
        .query(
            "SELECT id, device_id, device_name, ephemeral_pub, host_approval, reviewed_by, \
                    approver_device_id, expires_at \
             FROM bot_pairing_request \
             WHERE user_id = ?1 AND kind = 'add_host' AND status = 'pending' \
               AND host_approval IS NOT NULL AND expires_at > ?2 \
             ORDER BY created_at, id",
            libsql::params![bot_user_id.to_string(), now as i64],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let ephemeral: Vec<u8> = row.get(3)?;
        let approval: Vec<u8> = row.get(4)?;
        out.push(PendingBotHostWire {
            request_id: row.get(0)?,
            device_id: row.get(1)?,
            device_name: row.get(2)?,
            ephemeral_pub: crate::util::b64(&ephemeral),
            host_approval: crate::util::b64(&approval),
            approver_user_id: row.get(5)?,
            approver_device_id: row.get(6)?,
            expires_at: row.get(7)?,
        });
    }
    Ok(Some(out))
}

/// What [`apply_wrap_bot_host`] did.
#[derive(Debug, PartialEq, Eq)]
pub enum WrapOutcome {
    Wrapped,
    /// Not the signer's bot, no approval, or the approver is no longer an admin.
    Forbidden,
    /// Already wrapped by another host, expired, or the device id is taken.
    Gone,
    Invalid(&'static str),
}

/// POST /v1/bots/hosts/wrap — device-signed by one of the bot's hosts.
pub async fn wrap_bot_host(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<WrapBotHostBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    Ok(match apply_wrap_bot_host(&conn, &actor, &device, &parsed, crate::util::now_unix()).await? {
        WrapOutcome::Wrapped => {
            notify_admins_bots_changed(&state, &actor).await;
            ok_response::<WrapBotHostBody>(pollis_api::StatusOk::Ok)
        }
        WrapOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        WrapOutcome::Gone => conflict("pairing request is not open"),
        WrapOutcome::Invalid(why) => bad_request(why),
    })
}

/// Tell the current admins of `bot_user_id`'s group (their inboxes) that its bots changed: a host
/// was added by a wrap the admin's own client did not do, so a Bots page left open refetches.
/// Content-free; best-effort (the page also reads afresh on its next visit).
pub(crate) async fn notify_admins_bots_changed(state: &AppState, bot_user_id: &str) {
    if state.broker.livekit_ready().is_none() {
        return;
    }
    let Ok(conn) = state.db.conn().await else {
        return;
    };
    let Ok(Some(group_id)) = bot_group(&conn, bot_user_id).await else {
        return;
    };
    let mut admins = Vec::new();
    if let Ok(mut rows) = conn
        .query(
            "SELECT user_id FROM group_member WHERE group_id = ?1 AND role = 'admin'",
            libsql::params![group_id.clone()],
        )
        .await
    {
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(id) = row.get::<String>(0) {
                admins.push(id);
            }
        }
    }
    let event = serde_json::json!({ "type": "bots_changed", "group_id": group_id });
    for admin in admins {
        if let Err(e) = crate::broker::room_send_data(state, &format!("inbox-{admin}"), &event).await {
            tracing::warn!("bots changed: inbox notify failed (non-fatal): {e}");
        }
    }
}

/// Tell everyone in `bot_user_id`'s group (its realtime room) that its bots changed: an approval
/// changes which slash commands members can pick (#2018) and the marks they see (#2005), so an
/// open composer re-reads them. Content-free; best-effort.
pub(crate) async fn notify_group_bots_changed(state: &AppState, bot_user_id: &str) {
    if state.broker.livekit_ready().is_none() {
        return;
    }
    let Ok(conn) = state.db.conn().await else {
        return;
    };
    let Ok(Some(group_id)) = bot_group(&conn, bot_user_id).await else {
        return;
    };
    let event = serde_json::json!({ "type": "bots_changed", "group_id": group_id });
    if let Err(e) = crate::broker::room_send_data(state, &group_id, &event).await {
        tracing::warn!("bots changed: group room notify failed (non-fatal): {e}");
    }
}

/// Deliver the wrapped account key and register the new host as a device of
/// the bot, in one transaction. The `status = 'pending'` CAS makes the first
/// wrap win. The approver must still be an admin of the bot's group.
pub async fn apply_wrap_bot_host(
    conn: &Connection,
    bot_user_id: &str,
    wrapping_device: &str,
    body: &WrapBotHostBody,
    now: u64,
) -> anyhow::Result<WrapOutcome> {
    let wrapped = match crate::util::b64_decode(&body.wrapped_account_key) {
        Ok(w) if !w.is_empty() && w.len() <= 8192 => w,
        _ => return Ok(WrapOutcome::Invalid("invalid wrapped_account_key")),
    };
    let tx = crate::db::write_tx(conn).await?;
    let Some(group_id) = bot_group(&tx, bot_user_id).await? else {
        return Ok(WrapOutcome::Forbidden);
    };
    let mut rows = tx
        .query(
            "SELECT status, expires_at, host_approval IS NOT NULL, reviewed_by, device_id, device_name \
             FROM bot_pairing_request \
             WHERE id = ?1 AND kind = 'add_host' AND user_id = ?2",
            libsql::params![body.request_id.clone(), bot_user_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(WrapOutcome::Forbidden);
    };
    let status: String = row.get(0)?;
    let expires_at: i64 = row.get(1)?;
    let approved: bool = row.get::<i64>(2)? != 0;
    let approver: Option<String> = row.get(3)?;
    let device_id: String = row.get(4)?;
    let device_name: Option<String> = row.get(5)?;
    drop(rows);
    if status != "pending" || expires_at <= now as i64 {
        return Ok(WrapOutcome::Gone);
    }
    let Some(approver) = approver.filter(|_| approved) else {
        return Ok(WrapOutcome::Forbidden);
    };
    if !crate::groups::is_admin(&tx, &group_id, &approver).await? {
        return Ok(WrapOutcome::Forbidden);
    }
    let taken = crate::util::query_strings(
        &tx,
        "SELECT device_id FROM user_device WHERE device_id = ?1",
        libsql::params![device_id.clone()],
    )
    .await?;
    if !taken.is_empty() {
        return Ok(WrapOutcome::Gone);
    }
    let affected = tx
        .execute(
            "UPDATE bot_pairing_request \
             SET status = 'approved', wrapped_account_key = ?1, wrapped_by_device_id = ?2, expires_at = ?3 \
             WHERE id = ?4 AND status = 'pending'",
            libsql::params![
                wrapped,
                wrapping_device.to_string(),
                now.saturating_add(PAIRING_TTL_SECS) as i64,
                body.request_id.clone(),
            ],
        )
        .await?;
    if affected == 0 {
        return Ok(WrapOutcome::Gone);
    }
    // The new host's device, without a cert: it publishes one once it has
    // unwrapped the account key, through the cert-validity gate.
    tx.execute(
        "INSERT INTO user_device (device_id, user_id, device_name) VALUES (?1, ?2, ?3)",
        libsql::params![
            device_id.clone(),
            bot_user_id.to_string(),
            device_name.unwrap_or_else(|| "host".to_string()),
        ],
    )
    .await?;
    security_event(
        &tx,
        bot_user_id,
        "bot_host_added",
        Some(&device_id),
        serde_json::json!({ "request_id": body.request_id, "approved_by": approver, "wrapped_by": wrapping_device }),
    )
    .await?;
    tx.commit().await?;
    Ok(WrapOutcome::Wrapped)
}

// ---------------------------------------------------------------- trigger claims

/// POST /v1/bots/triggers/claim — device-signed by one of the bot's hosts.
pub async fn claim_trigger(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<ClaimTriggerBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.installation_id != actor {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    match apply_claim_trigger(&conn, &actor, &device, &parsed.trigger_key, crate::util::now_unix()).await? {
        Ok(won) => Ok(ok_response::<ClaimTriggerBody>(ClaimTriggerResponse { won })),
        Err(WriteOutcome::Invalid(why)) => Ok(bad_request(why)),
        Err(_) => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// Claim `(installation, trigger_key)`: the `PRIMARY KEY` makes the first insert
/// win. A retry by the device that won answers `true` again.
pub async fn apply_claim_trigger(
    conn: &Connection,
    installation_id: &str,
    device_id: &str,
    trigger_key: &str,
    now: u64,
) -> anyhow::Result<Result<bool, WriteOutcome>> {
    if trigger_key.is_empty() || trigger_key.len() > 256 {
        return Ok(Err(WriteOutcome::Invalid("invalid trigger_key")));
    }
    if !is_bot(conn, installation_id).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    let inserted = conn
        .execute(
            "INSERT OR IGNORE INTO bot_trigger_claim (installation_id, trigger_key, device_id, claimed_at) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                installation_id.to_string(),
                trigger_key.to_string(),
                device_id.to_string(),
                now as i64,
            ],
        )
        .await?;
    if inserted > 0 {
        return Ok(Ok(true));
    }
    let winner = crate::util::query_strings(
        conn,
        "SELECT device_id FROM bot_trigger_claim WHERE installation_id = ?1 AND trigger_key = ?2",
        libsql::params![installation_id.to_string(), trigger_key.to_string()],
    )
    .await?;
    Ok(Ok(winner.first().is_some_and(|d| d == device_id)))
}

// ---------------------------------------------------------------- App state (#2004)

/// The most sealed App state one installation may hold. The schema enforces it
/// too (`000053_bot_app_state.sql`); the manifest's `state.quota` is enforced by
/// the hosts on plaintext sizes, under this.
pub const BOT_APP_STATE_MAX_BYTES: u64 = 1 << 20;
/// The most sealed bytes one entry may hold (schema CHECK).
pub const BOT_APP_STATE_MAX_ENTRY_BYTES: usize = 70_000;

fn decode_key_ids(ids: &[String]) -> Option<Vec<Vec<u8>>> {
    if ids.len() > APP_STATE_MAX_KEYS_PER_CALL {
        return None;
    }
    ids.iter().map(|k| decode_exact(k, 32)).collect()
}

/// POST /v1/bots/app-state/read — device-signed by one of the bot's hosts.
pub async fn app_state_read(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<AppStateReadBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, _device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.installation_id != actor {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    match apply_app_state_read(&conn, &actor, &parsed.key_ids).await? {
        Ok(resp) => Ok(ok_response::<AppStateReadBody>(resp)),
        Err(WriteOutcome::Invalid(why)) => Ok(bad_request(why)),
        Err(_) => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// The entries for `key_ids` that exist, and the installation's usage.
pub async fn apply_app_state_read(
    conn: &Connection,
    installation_id: &str,
    key_ids: &[String],
) -> anyhow::Result<Result<AppStateReadResponse, WriteOutcome>> {
    let Some(ids) = decode_key_ids(key_ids) else {
        return Ok(Err(WriteOutcome::Invalid("invalid key_ids")));
    };
    if !is_bot(conn, installation_id).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    let mut entries = Vec::new();
    for id in ids {
        let mut rows = conn
            .query(
                "SELECT version, sealed FROM bot_app_state WHERE installation_id = ?1 AND key_id = ?2",
                libsql::params![installation_id.to_string(), id.clone()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            let sealed: Vec<u8> = row.get(1)?;
            entries.push(AppStateEntry {
                key_id: crate::util::b64(&id),
                version: row.get(0)?,
                sealed: crate::util::b64(&sealed),
            });
        }
    }
    let mut rows = conn
        .query(
            "SELECT COALESCE(SUM(length(sealed)), 0), COUNT(*) FROM bot_app_state WHERE installation_id = ?1",
            libsql::params![installation_id.to_string()],
        )
        .await?;
    let (used_bytes, count) = match rows.next().await? {
        Some(row) => (row.get::<i64>(0)?, row.get::<i64>(1)?),
        None => (0, 0),
    };
    Ok(Ok(AppStateReadResponse {
        entries,
        used_bytes: used_bytes.max(0) as u64,
        rows: count.max(0) as u64,
    }))
}

/// POST /v1/bots/app-state/commit — device-signed by the host that won the
/// claim on `trigger_key`.
pub async fn app_state_commit(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let (identity, parsed) = match gate_identity_and_parse::<AppStateCommitBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let (actor, device) = match signed(identity) {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    if parsed.installation_id != actor {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let conn = state.db.conn().await?;
    match apply_app_state_commit(&conn, &actor, &device, &parsed, crate::util::now_unix()).await? {
        Ok(resp) => Ok(ok_response::<AppStateCommitBody>(resp)),
        Err(WriteOutcome::Invalid(why)) => Ok(bad_request(why)),
        Err(_) => Ok(AuthRejection::Forbidden.into_response()),
    }
}

/// Apply a job's writes, all or none, once per claim, only for the device that
/// won the claim, each only at the version the host read.
pub async fn apply_app_state_commit(
    conn: &Connection,
    installation_id: &str,
    device_id: &str,
    body: &AppStateCommitBody,
    now: u64,
) -> anyhow::Result<Result<AppStateCommitResponse, WriteOutcome>> {
    if body.trigger_key.is_empty() || body.trigger_key.len() > 256 {
        return Ok(Err(WriteOutcome::Invalid("invalid trigger_key")));
    }
    if body.writes.is_empty() || body.writes.len() > APP_STATE_MAX_KEYS_PER_CALL {
        return Ok(Err(WriteOutcome::Invalid("invalid writes")));
    }
    let mut writes = Vec::with_capacity(body.writes.len());
    let mut seen = std::collections::HashSet::new();
    for w in &body.writes {
        let Some(key_id) = decode_exact(&w.key_id, 32) else {
            return Ok(Err(WriteOutcome::Invalid("invalid key_id")));
        };
        if !seen.insert(key_id.clone()) || w.expected_version < 0 {
            return Ok(Err(WriteOutcome::Invalid("invalid writes")));
        }
        let sealed = match &w.sealed {
            None => None,
            Some(b64) => match crate::util::b64_decode(b64) {
                Ok(v) if (30..=BOT_APP_STATE_MAX_ENTRY_BYTES).contains(&v.len()) => Some(v),
                _ => return Ok(Err(WriteOutcome::Invalid("invalid sealed value"))),
            },
        };
        writes.push((key_id, w.expected_version, sealed, w.key_id.clone()));
    }
    if !is_bot(conn, installation_id).await? {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    let tx = crate::db::write_tx(conn).await?;
    let mut rows = tx
        .query(
            "SELECT device_id, state_committed FROM bot_trigger_claim \
             WHERE installation_id = ?1 AND trigger_key = ?2",
            libsql::params![installation_id.to_string(), body.trigger_key.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        // No claim: state is only ever written under a won claim.
        return Ok(Err(WriteOutcome::Forbidden));
    };
    let winner: String = row.get(0)?;
    let committed: i64 = row.get(1)?;
    drop(rows);
    if winner != device_id {
        return Ok(Err(WriteOutcome::Forbidden));
    }
    if committed == 1 {
        return Ok(Ok(AppStateCommitResponse::AlreadyCommitted));
    }
    // Versions first, so a conflict changes nothing.
    let mut current_total: i64 = {
        let mut r = tx
            .query(
                "SELECT COALESCE(SUM(length(sealed)), 0) FROM bot_app_state WHERE installation_id = ?1",
                libsql::params![installation_id.to_string()],
            )
            .await?;
        match r.next().await? {
            Some(row) => row.get(0)?,
            None => 0,
        }
    };
    let mut current = Vec::with_capacity(writes.len());
    for (key_id, expected, sealed, key_b64) in &writes {
        let mut r = tx
            .query(
                "SELECT version, length(sealed) FROM bot_app_state WHERE installation_id = ?1 AND key_id = ?2",
                libsql::params![installation_id.to_string(), key_id.clone()],
            )
            .await?;
        let (version, old_len) = match r.next().await? {
            Some(row) => (row.get::<i64>(0)?, row.get::<i64>(1)?),
            None => (0, 0),
        };
        if version != *expected {
            return Ok(Ok(AppStateCommitResponse::Conflict { key_id: key_b64.clone() }));
        }
        current_total += sealed.as_ref().map_or(0, |s| s.len() as i64) - old_len;
        current.push(version);
    }
    if current_total > BOT_APP_STATE_MAX_BYTES as i64 {
        return Ok(Ok(AppStateCommitResponse::OverQuota { max_bytes: BOT_APP_STATE_MAX_BYTES }));
    }
    for ((key_id, _, sealed, _), version) in writes.iter().zip(current) {
        let id = || libsql::params![installation_id.to_string(), key_id.clone()];
        match (sealed, version) {
            (None, 0) => {}
            (None, _) => {
                tx.execute("DELETE FROM bot_app_state WHERE installation_id = ?1 AND key_id = ?2", id())
                    .await?;
            }
            (Some(s), 0) => {
                tx.execute(
                    "INSERT INTO bot_app_state (installation_id, key_id, sealed, version, updated_at) \
                     VALUES (?1, ?2, ?3, 1, ?4)",
                    libsql::params![installation_id.to_string(), key_id.clone(), s.clone(), now as i64],
                )
                .await?;
            }
            (Some(s), v) => {
                tx.execute(
                    "UPDATE bot_app_state SET sealed = ?3, version = ?4, updated_at = ?5 \
                     WHERE installation_id = ?1 AND key_id = ?2",
                    libsql::params![installation_id.to_string(), key_id.clone(), s.clone(), v + 1, now as i64],
                )
                .await?;
            }
        }
    }
    tx.execute(
        "UPDATE bot_trigger_claim SET state_committed = 1 WHERE installation_id = ?1 AND trigger_key = ?2",
        libsql::params![installation_id.to_string(), body.trigger_key.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(Ok(AppStateCommitResponse::Committed))
}

// ---------------------------------------------------------------- retention

/// Delete lapsed pairing requests and trigger claims past
/// [`TRIGGER_CLAIM_RETENTION_SECS`]. Run by the retention sweep. Returns
/// `(requests, claims)` deleted.
pub async fn sweep_expired(conn: &Connection, now: u64) -> anyhow::Result<(u64, u64)> {
    let requests = conn
        .execute(
            "DELETE FROM bot_pairing_request WHERE expires_at <= ?1",
            libsql::params![now as i64],
        )
        .await?;
    let claims = conn
        .execute(
            "DELETE FROM bot_trigger_claim WHERE claimed_at <= ?1",
            libsql::params![now.saturating_sub(TRIGGER_CLAIM_RETENTION_SECS) as i64],
        )
        .await?;
    Ok((requests, claims))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bot_email_is_the_lowercased_placeholder() {
        assert_eq!(bot_email("01HZXABC"), "bot-01hzxabc@bot.invalid");
        assert!(is_bot_address(&bot_email("01HZXABC")));
        assert!(is_bot_address(" X@Sub.Bot.Invalid "));
        assert!(!is_bot_address("x@robot.invalid"));
        assert!(!is_bot_address("x@pollis.com"));
    }

    #[test]
    fn a_bot_username_has_the_profile_shape() {
        for name in ["Grafana Alerts", "", "@@@", "a-very-long-bot-name-that-goes-on-and-on"] {
            let u = bot_username(name, "01HZXABCDEFGHJKMNPQRSTVWXY");
            assert!(crate::profile::is_valid_username(&u), "{u}");
        }
    }
}
