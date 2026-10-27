//! Content-free push fan-out, server-side (#987).
//!
//! # Why this moved
//!
//! The client used to do this: read the conversation's members, read their
//! `push_token` rows, and POST to Expo itself. Three consequences, all bad:
//!
//!   1. It needed a whole-database read credential to see other people's push
//!      tokens — the single most identifying row a client had any reason to
//!      read about someone else.
//!   2. It put two more dependent round trips on the hot send path, after the
//!      send had already landed.
//!   3. Every desktop and mobile client talked to `exp.host` directly, which is
//!      a non-first-party host outside the overlay's allowlist — so the relay
//!      could not carry it, and the module's own comment said the DS should
//!      proxy it "longer term".
//!
//! The DS already knows the conversation, the sender and the mention list from
//! `POST /v1/messages/send`. Doing the fan-out here removes all three at once,
//! and takes `push_token` out of client visibility entirely.
//!
//! # What a push carries
//!
//! An opaque per-notification handle `h` (#1122/#1157) and nothing else —
//! enough for the client to route and re-ingest the (still-encrypted) message
//! after resolving it over its own channel, never the plaintext, the sender, the
//! conversation, or any content. On Android the handle is also the
//! notification's `tag` (#1508), so the app can find and clear it in the tray.
//!
//! When the user reads a conversation on any client, `POST /v1/push/dismiss`
//! sends their devices one silent, data-only push `{ d: [handles] }` naming the
//! notifications to clear (#1508) — the same random handles, so again no
//! conversation and no content. See [`dismiss_push`].
//!
//! Best-effort throughout: a push that fails to send must never fail a send that
//! already landed.
//!
//! # What Expo said back (#1423)
//!
//! Expo answers 200 with one TICKET per message, and an error there
//! (`DeviceNotRegistered`, `MessageRateExceeded`, `InvalidCredentials`, ...) is
//! not an HTTP error. What APNs/FCM later made of an accepted message arrives
//! only as a RECEIPT, fetched by ticket id. So:
//!
//!   * every ticket is read ([`parse_tickets`]); an error is counted and logged
//!     by CLASS only, never the token, the user, the conversation or Expo's
//!     free-text message (which quotes the token);
//!   * `DeviceNotRegistered` deletes that `push_token` row on the spot;
//!   * an accepted ticket's id is kept in `push_ticket` (migration 000037) and
//!     the GC sweep asks Expo for its receipt ([`check_push_receipts`]), with
//!     the same handling; a ticket is dropped once answered or after 24 hours;
//!   * the counts are served to the operator on `GET /v1/ops/push`
//!     ([`ops_push`]). They are in memory, counted since the last start.

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use libsql::Connection;
use pollis_api::devices::{
    DismissEntry, DismissPushBody, DismissPushResponse, DismissedPushHandlesBody,
    DismissedPushHandlesResponse, ResolvePushHandleBody, ResolvePushHandleResponse,
    DISMISSED_QUERY_MAX_HANDLES, DISMISS_MAX_CONVERSATIONS,
};
use rand::rngs::OsRng;
use rand::RngCore as _;

use crate::error::AppError;
use crate::reads::authed_user;
use crate::writes::RawRequest;
use crate::AppState;
use crate::util::{BIND_CHUNK, placeholders};

/// Expo's push service endpoint. Accepts a JSON array of up to 100 messages.
const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";

/// Expo's receipts endpoint. Accepts up to 1000 ticket ids per request.
const EXPO_RECEIPTS_URL: &str = "https://exp.host/--/api/v2/push/getReceipts";

/// Expo accepts up to 100 messages per request.
const EXPO_BATCH: usize = 100;

/// Expo accepts up to 1000 ticket ids per receipts request.
const RECEIPT_BATCH: usize = 1000;

/// Tickets asked about per sweep, at most. Anything beyond waits for the next
/// sweep or ages out, which only delays a prune.
const RECEIPTS_PER_SWEEP: usize = 10 * RECEIPT_BATCH;

/// How long a ticket is worth keeping. Expo discards receipts after 24 hours,
/// so an older ticket can never be answered. Migration 000037's trigger
/// enforces the same bound.
pub const PUSH_TICKET_MAX_AGE_SECS: i64 = 24 * 60 * 60;

/// The Expo access token the fan-out authenticates with (`EXPO_TOKEN`).
///
/// Optional while the Expo account leaves push sends unauthenticated, and
/// REQUIRED the moment "Enhanced Security for Push Notifications" is switched on
/// there: with it on, Expo rejects any send that does not carry the token, which
/// is what stops someone who has scraped an `ExponentPushToken[...]` off a device
/// from pushing to our users themselves. Set it BEFORE enabling enforcement —
/// the other order silently drops every push in between.
///
/// Read once, like every other DS env value; the DS never reloads its
/// environment.
fn expo_token() -> Option<&'static str> {
    static TOKEN: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    TOKEN
        .get_or_init(|| std::env::var("EXPO_TOKEN").ok().filter(|s| !s.is_empty()))
        .as_deref()
}

/// The POST for one batch, bearing the access token when one is configured.
///
/// Split out from the send loop so the "token set => `Authorization` present,
/// token unset => absent" invariant is testable without touching the process
/// environment, which no parallel test can do safely.
///
/// Also builds the receipts request: with enhanced security on, Expo wants the
/// token there too.
fn expo_request<B: serde::Serialize + ?Sized>(
    url: &str,
    token: Option<&str>,
    body: &B,
) -> reqwest::RequestBuilder {
    let req = crate::util::http_post(crate::util::Upstream::ExpoPush, url).json(body);
    match token {
        Some(token) => req.bearer_auth(token),
        None => req,
    }
}

/// Who a send asked the DS to wake (#987, #843, #1431), decoded from the
/// `push_to` / `push_mentions` pair on `POST /v1/messages/send`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushAudience {
    /// `None` = every other member (a DM, any current channel message, an old
    /// client's `@all`). `Some(ids)` = only these — an old client's
    /// `@username`. Always INTERSECTED with real membership, never trusted.
    pub only: Option<Vec<String>>,
    /// Members named directly by `@username`. They are woken even if they muted
    /// the channel; everyone else who muted it is skipped.
    pub mentioned: Vec<String>,
}

impl PushAudience {
    /// Decode the wire pair. `None` when the send asked for no push at all
    /// (`push_to` absent — an old client's ordinary channel chatter, or an
    /// edit/delete/receipt envelope).
    ///
    /// `push_mentions` absent means a client from before #1431, which only ever
    /// sent a non-empty `push_to` for direct `@username` mentions — so that list
    /// IS its mention list, and its named members still get through a mute.
    pub fn from_send(
        push_to: Option<Vec<String>>,
        push_mentions: Option<Vec<String>>,
    ) -> Option<Self> {
        let push_to = push_to?;
        let mentioned = push_mentions.unwrap_or_else(|| push_to.clone());
        let only = if push_to.is_empty() { None } else { Some(push_to) };
        Some(Self { only, mentioned })
    }
}

/// Wake the members of a conversation a send asked for with a content-free
/// notification.
///
/// `audience.only` narrows the recipients to a subset; it is INTERSECTED with
/// real membership rather than trusted, so it can only ever REMOVE recipients:
/// it cannot be used to push to someone outside the conversation. Then every
/// recipient who muted the channel (`channel_mute`, #1431) is dropped unless
/// the message names them directly — Slack's rule, so a muted channel still
/// reaches you on `@you` but not on `@all` or chatter.
pub async fn notify_new_message(
    conn: &Connection,
    conversation_id: &str,
    sender_id: &str,
    audience: &PushAudience,
) -> anyhow::Result<()> {
    notify_new_message_at(conn, EXPO_PUSH_URL, conversation_id, sender_id, audience).await
}

/// [`notify_new_message`] against an explicit Expo URL, so a test can stand a
/// fake Expo up and see exactly which tokens a send would wake.
pub async fn notify_new_message_at(
    conn: &Connection,
    expo_url: &str,
    conversation_id: &str,
    sender_id: &str,
    audience: &PushAudience,
) -> anyhow::Result<()> {
    let user_ids = push_recipients(conn, conversation_id, sender_id, audience).await?;
    if user_ids.is_empty() {
        return Ok(());
    }

    let tokens = push_tokens(conn, &user_ids).await?;
    if tokens.is_empty() {
        return Ok(());
    }

    // `kind` mirrors the values the mobile push router understands
    // (mobile/hooks/usePushNotifications.ts): "channel" | "dm".
    let kind = if is_dm(conn, conversation_id).await? {
        "dm"
    } else {
        "channel"
    };

    let handles = mint_push_handles(conn, &user_ids, conversation_id, kind).await;

    let messages: Vec<serde_json::Value> = tokens
        .iter()
        .map(|(token, platform, user_id)| {
            expo_message(token, platform.as_deref(), handles.get(user_id))
        })
        .collect();
    let push_tokens: Vec<String> = tokens.into_iter().map(|(token, _, _)| token).collect();

    let auth = expo_token();
    for (chunk, chunk_tokens) in messages
        .chunks(EXPO_BATCH)
        .zip(push_tokens.chunks(EXPO_BATCH))
    {
        send_batch(conn, expo_url, auth, chunk, chunk_tokens, stats()).await;
    }
    Ok(())
}

/// The users a send wakes: every member but the sender, narrowed to
/// `audience.only`, minus whoever muted the channel and is not named in
/// `audience.mentioned`.
pub async fn push_recipients(
    conn: &Connection,
    conversation_id: &str,
    sender_id: &str,
    audience: &PushAudience,
) -> anyhow::Result<Vec<String>> {
    // Recipients = conversation members other than the sender. `is_member`'s
    // three-legged id namespace applies here too: the id may be a DM, a group or
    // a channel, and a channel's members live on its group.
    let mut user_ids = conversation_recipients(conn, conversation_id, sender_id).await?;
    if let Some(allowed) = &audience.only {
        user_ids.retain(|u| allowed.contains(u));
    }
    if user_ids.is_empty() {
        return Ok(user_ids);
    }
    let muted = muted_by(conn, conversation_id).await;
    user_ids.retain(|u| !muted.contains(u) || audience.mentioned.contains(u));
    Ok(user_ids)
}

/// Every user who muted `channel_id` (#1431). A failed read — a DS running
/// before migration 000038 is applied — counts as nobody: an unwanted push is
/// recoverable, a dropped one is not.
async fn muted_by(conn: &Connection, channel_id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let rows = conn
        .query(
            "SELECT user_id FROM channel_mute WHERE channel_id = ?1",
            libsql::params![channel_id.to_string()],
        )
        .await;
    let mut rows = match rows {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("channel_mute read skipped: {e}");
            return out;
        }
    };
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(user) = row.get::<String>(0) {
            out.push(user);
        }
    }
    out
}

// ── Tickets, receipts and outcome counts (#1423) ────────────────────────────

/// What Expo said went wrong with one message, as a fixed set of names.
///
/// Logged and counted INSTEAD of Expo's own text: the `message` Expo sends with
/// an error quotes the push token, and the `details` may too. An unknown or
/// missing class is `Other`, so nothing Expo sends can reach a log line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PushErrorClass {
    DeviceNotRegistered,
    MessageTooBig,
    MessageRateExceeded,
    MismatchSenderId,
    InvalidCredentials,
    Other,
}

impl PushErrorClass {
    /// Classify Expo's `details.error`.
    pub fn from_expo(error: Option<&str>) -> Self {
        match error {
            Some("DeviceNotRegistered") => Self::DeviceNotRegistered,
            Some("MessageTooBig") => Self::MessageTooBig,
            Some("MessageRateExceeded") => Self::MessageRateExceeded,
            Some("MismatchSenderId") => Self::MismatchSenderId,
            Some("InvalidCredentials") => Self::InvalidCredentials,
            _ => Self::Other,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DeviceNotRegistered => "DeviceNotRegistered",
            Self::MessageTooBig => "MessageTooBig",
            Self::MessageRateExceeded => "MessageRateExceeded",
            Self::MismatchSenderId => "MismatchSenderId",
            Self::InvalidCredentials => "InvalidCredentials",
            Self::Other => "Other",
        }
    }
}

/// One ticket or one receipt, reduced to what the DS acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PushOutcome {
    /// Accepted. For a ticket, `id` is what the receipt is fetched by; a
    /// receipt carries none.
    Ok { id: Option<String> },
    Error(PushErrorClass),
}

/// Read one ticket or receipt object: `{"status":"ok","id":…}` or
/// `{"status":"error","details":{"error":…}}`. Anything else is an `Other`
/// error, since a ticket we cannot read is one we cannot follow up.
fn parse_outcome(v: &serde_json::Value) -> PushOutcome {
    match v.get("status").and_then(|s| s.as_str()) {
        Some("ok") => PushOutcome::Ok {
            id: v.get("id").and_then(|i| i.as_str()).map(str::to_string),
        },
        Some("error") => PushOutcome::Error(PushErrorClass::from_expo(
            v.get("details")
                .and_then(|d| d.get("error"))
                .and_then(|e| e.as_str()),
        )),
        _ => PushOutcome::Error(PushErrorClass::Other),
    }
}

/// The tickets in a send response, in message order. `None` when the body has
/// no `data` to read (a request-level error).
pub fn parse_tickets(body: &serde_json::Value) -> Option<Vec<PushOutcome>> {
    match body.get("data")? {
        serde_json::Value::Array(items) => Some(items.iter().map(parse_outcome).collect()),
        // Expo answers a single-message request with a bare object. We always
        // send an array, but reading both costs nothing.
        obj @ serde_json::Value::Object(_) => Some(vec![parse_outcome(obj)]),
        _ => None,
    }
}

/// The receipts in a getReceipts response, keyed by ticket id. A ticket Expo
/// has no receipt for yet is simply absent.
pub fn parse_receipts(body: &serde_json::Value) -> Option<Vec<(String, PushOutcome)>> {
    let data = body.get("data")?.as_object()?;
    Some(
        data.iter()
            .map(|(id, v)| (id.clone(), parse_outcome(v)))
            .collect(),
    )
}

/// Expo's request-level error codes (`errors[].code`), kept only when they have
/// the shape Expo uses (upper-case words and underscores), so a code cannot
/// carry a token or anything else into a log line or the counts.
fn request_error_codes(body: &serde_json::Value) -> Vec<String> {
    body.get("errors")
        .and_then(|e| e.as_array())
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.get("code").and_then(|c| c.as_str()))
                .filter(|c| {
                    !c.is_empty()
                        && c.len() <= 64
                        && c.chars().all(|ch| ch.is_ascii_uppercase() || ch == '_')
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Push outcome counts since the process started, for `GET /v1/ops/push`.
///
/// Counts only: no token, user, conversation, or time of any single push.
#[derive(Default)]
pub struct PushStats {
    inner: Mutex<PushCounts>,
}

#[derive(Default, Clone, serde::Serialize)]
struct PushCounts {
    since: u64,
    /// Messages in send requests Expo answered with tickets.
    messages_sent: u64,
    /// Send requests that failed outright: a transport error, a non-2xx, or a
    /// body with no tickets. Every message in such a request was lost.
    send_request_failures: u64,
    /// Request-level error codes Expo named, by code.
    send_request_errors: BTreeMap<String, u64>,
    tickets_ok: u64,
    ticket_errors: BTreeMap<&'static str, u64>,
    /// Receipts requests that failed outright; their tickets are asked about
    /// again on the next sweep.
    receipt_request_failures: u64,
    receipts_ok: u64,
    receipt_errors: BTreeMap<&'static str, u64>,
    /// Tickets dropped unanswered after 24 hours.
    tickets_expired: u64,
    /// `push_token` rows deleted on `DeviceNotRegistered`.
    tokens_pruned: u64,
}

impl PushStats {
    fn with(&self, f: impl FnOnce(&mut PushCounts)) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if g.since == 0 {
            g.since = crate::util::now_unix();
        }
        f(&mut g);
    }

    /// The counts as served on `GET /v1/ops/push`.
    pub fn snapshot(&self) -> serde_json::Value {
        let g = self.inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
        serde_json::to_value(g).unwrap_or(serde_json::Value::Null)
    }
}

/// The process-wide counts. A static rather than an `AppState` field because
/// the fan-out runs detached from its request with only a connection, and the
/// GC sweep runs outside the router altogether.
pub fn stats() -> &'static PushStats {
    static STATS: std::sync::OnceLock<PushStats> = std::sync::OnceLock::new();
    STATS.get_or_init(PushStats::default)
}

/// POST one batch to Expo and act on every ticket. Never fails: whatever goes
/// wrong is counted and logged, and the send that triggered it has already
/// been answered.
///
/// `tokens[i]` is the token `messages[i]` is addressed to; Expo returns the
/// tickets in message order.
async fn send_batch(
    conn: &Connection,
    url: &str,
    auth: Option<&str>,
    messages: &[serde_json::Value],
    tokens: &[String],
    stats: &PushStats,
) {
    // Through `util::http_post`, which forces a deadline (#913). A push nobody
    // is waiting on must never be the thing that pins a handler open.
    let response = match expo_request(url, auth, messages).send().await {
        Ok(r) => r,
        Err(e) => {
            stats.with(|c| c.send_request_failures += 1);
            tracing::warn!("expo push send failed: {}", e.without_url());
            return;
        }
    };
    let status = response.status();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let codes = request_error_codes(&body);
    let tickets = if status.is_success() {
        parse_tickets(&body)
    } else {
        None
    };
    let Some(tickets) = tickets else {
        stats.with(|c| {
            c.send_request_failures += 1;
            for code in &codes {
                *c.send_request_errors.entry(code.clone()).or_default() += 1;
            }
        });
        // The status and Expo's error codes only: the body can quote tokens.
        tracing::warn!("expo push request failed: status {status}, codes {codes:?}");
        return;
    };
    stats.with(|c| c.messages_sent += messages.len() as u64);
    if tickets.len() != tokens.len() {
        tracing::warn!(
            "expo push: {} ticket(s) for {} message(s); acting on the aligned prefix",
            tickets.len(),
            tokens.len()
        );
    }
    let now = crate::util::now_unix() as i64;
    for (outcome, token) in tickets.into_iter().zip(tokens) {
        match outcome {
            PushOutcome::Ok { id } => {
                stats.with(|c| c.tickets_ok += 1);
                let Some(id) = id else {
                    continue;
                };
                // Best-effort, like everything here: a DS running before
                // migration 000037 fails this and simply checks no receipt.
                let stored = conn
                    .execute(
                        "INSERT OR IGNORE INTO push_ticket (ticket_id, token, created_at) \
                         VALUES (?1, ?2, ?3)",
                        libsql::params![id, token.clone(), now],
                    )
                    .await;
                if let Err(e) = stored {
                    tracing::debug!("push_ticket insert skipped: {e}");
                }
            }
            PushOutcome::Error(class) => {
                stats.with(|c| *c.ticket_errors.entry(class.as_str()).or_default() += 1);
                tracing::warn!("expo push ticket error: {}", class.as_str());
                if class == PushErrorClass::DeviceNotRegistered {
                    prune_token(conn, token, stats).await;
                }
            }
        }
    }
}

/// Delete a token Expo says no longer reaches a device, and any tickets still
/// waiting on it. A device that is in fact still there re-registers on its next
/// launch or foreground.
async fn prune_token(conn: &Connection, token: &str, stats: &PushStats) {
    let deleted = conn
        .execute(
            "DELETE FROM push_token WHERE token = ?1",
            libsql::params![token.to_string()],
        )
        .await;
    match deleted {
        Ok(n) => {
            if n > 0 {
                stats.with(|c| c.tokens_pruned += n);
            }
        }
        Err(e) => tracing::warn!("push_token prune failed: {e}"),
    }
    let tickets = conn
        .execute(
            "DELETE FROM push_ticket WHERE token = ?1",
            libsql::params![token.to_string()],
        )
        .await;
    if let Err(e) = tickets {
        tracing::debug!("push_ticket prune skipped: {e}");
    }
}

/// What one receipts sweep did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReceiptSweep {
    /// Tickets dropped unanswered because they passed 24 hours.
    pub expired: u64,
    /// Tickets Expo had a receipt for, now deleted.
    pub checked: u64,
    /// Of those, the ones that were errors.
    pub errors: u64,
}

/// Ask Expo for the receipts of every outstanding ticket and act on them.
/// Called from the GC sweep in `main`.
pub async fn check_push_receipts(conn: &Connection) -> anyhow::Result<ReceiptSweep> {
    check_receipts_at(conn, EXPO_RECEIPTS_URL, expo_token(), stats()).await
}

async fn check_receipts_at(
    conn: &Connection,
    url: &str,
    auth: Option<&str>,
    stats: &PushStats,
) -> anyhow::Result<ReceiptSweep> {
    let mut sweep = ReceiptSweep::default();
    let cutoff = crate::util::now_unix() as i64 - PUSH_TICKET_MAX_AGE_SECS;
    sweep.expired = conn
        .execute(
            "DELETE FROM push_ticket WHERE created_at < ?1",
            libsql::params![cutoff],
        )
        .await?;
    if sweep.expired > 0 {
        stats.with(|c| c.tickets_expired += sweep.expired);
    }

    let mut rows = conn
        .query(
            "SELECT ticket_id, token FROM push_ticket ORDER BY created_at LIMIT ?1",
            libsql::params![RECEIPTS_PER_SWEEP as i64],
        )
        .await?;
    let mut pending: HashMap<String, String> = HashMap::new();
    while let Some(row) = rows.next().await? {
        pending.insert(row.get::<String>(0)?, row.get::<String>(1)?);
    }
    let ids: Vec<&String> = pending.keys().collect();

    for chunk in ids.chunks(RECEIPT_BATCH) {
        let body = serde_json::json!({ "ids": chunk });
        let response = match expo_request(url, auth, &body).send().await {
            Ok(r) => r,
            Err(e) => {
                stats.with(|c| c.receipt_request_failures += 1);
                tracing::warn!("expo receipts request failed: {}", e.without_url());
                continue;
            }
        };
        let status = response.status();
        let json: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
        let receipts = if status.is_success() {
            parse_receipts(&json)
        } else {
            None
        };
        let Some(receipts) = receipts else {
            stats.with(|c| c.receipt_request_failures += 1);
            let codes = request_error_codes(&json);
            tracing::warn!("expo receipts request failed: status {status}, codes {codes:?}");
            continue;
        };
        for (id, outcome) in receipts {
            // Only ids we asked about: a receipt for anything else is ignored
            // rather than trusted to name a row.
            let Some(token) = pending.get(&id) else {
                continue;
            };
            match outcome {
                PushOutcome::Ok { .. } => stats.with(|c| c.receipts_ok += 1),
                PushOutcome::Error(class) => {
                    sweep.errors += 1;
                    stats.with(|c| *c.receipt_errors.entry(class.as_str()).or_default() += 1);
                    tracing::warn!("expo push receipt error: {}", class.as_str());
                    if class == PushErrorClass::DeviceNotRegistered {
                        prune_token(conn, token, stats).await;
                    }
                }
            }
            conn.execute(
                "DELETE FROM push_ticket WHERE ticket_id = ?1",
                libsql::params![id],
            )
            .await?;
            sweep.checked += 1;
        }
    }
    Ok(sweep)
}

/// `GET /v1/ops/push` — push outcome counts since the DS started, plus how many
/// tickets are still waiting on a receipt. Behind the operator token: 404 when
/// none is configured, 401 without the right bearer token (like
/// `/v1/ops/errors`).
pub async fn ops_push(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(expected) = state.metrics_token.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !crate::bearer_token_matches(&headers, expected) {
        return crate::error::AuthRejection::Unauthorized.into_response();
    }
    let pending = match state.db.conn().await {
        Ok(conn) => pending_tickets(&conn).await.ok(),
        Err(_) => None,
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "now": crate::util::now_unix(),
            "pending_receipts": pending,
            "counts": stats().snapshot(),
        })),
    )
        .into_response()
}

async fn pending_tickets(conn: &Connection) -> anyhow::Result<i64> {
    let mut rows = conn.query("SELECT COUNT(*) FROM push_ticket", ()).await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<i64>(0)?),
        None => Ok(0),
    }
}

/// Every member of `conversation_id` except `sender_id`.
///
/// ORs across the same three membership shapes `writes::is_member` does, so a
/// channel id resolves through its group and a DM through its own table.
async fn conversation_recipients(
    conn: &Connection,
    conversation_id: &str,
    sender_id: &str,
) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT user_id FROM dm_channel_member \
             WHERE dm_channel_id = ?1 AND user_id <> ?2 \
             UNION \
             SELECT user_id FROM group_member \
             WHERE group_id = ?1 AND user_id <> ?2 \
             UNION \
             SELECT gm.user_id FROM channels c \
             JOIN group_member gm ON gm.group_id = c.group_id \
             WHERE c.id = ?1 AND gm.user_id <> ?2",
            libsql::params![conversation_id.to_string(), sender_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

/// `(token, platform, user_id)` per registered device.
///
/// The `user_id` rides along because the notification handle is minted per
/// RECIPIENT USER (#1122): every token belonging to one user carries that
/// user's handle, so the payload names no conversation and a provider cannot
/// correlate a handle across users.
async fn push_tokens(
    conn: &Connection,
    user_ids: &[String],
) -> anyhow::Result<Vec<(String, Option<String>, String)>> {
    let mut out = Vec::new();
    for chunk in user_ids.chunks(BIND_CHUNK) {
        let placeholders = placeholders(chunk.len(), 1);
        let sql = format!(
            "SELECT token, platform, user_id FROM push_token WHERE user_id IN ({placeholders})"
        );
        let params: Vec<libsql::Value> = chunk.iter().map(|u| u.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push((row.get(0)?, row.get(1)?, row.get(2)?));
        }
    }
    Ok(out)
}

/// One Expo message, built from nothing but the token, the platform and the
/// handle — so what leaves this process can be asserted on directly.
///
/// **The `data` blob is the whole privacy surface.** Expo, APNs and FCM all see
/// it, and all three sit outside the overlay by design. It therefore carries the
/// opaque handle and NOTHING else: no conversation id (#1157), no `kind`, no
/// sender, no preview, no plaintext. The title and body are fixed strings.
///
/// A `None` handle means minting failed for that recipient (best-effort by
/// design — a push nobody is waiting on must not fail a send). The notification
/// still goes out, because "you have a new message" is true and useful, but it
/// carries no routing and the tap opens the app. That is strictly better than
/// naming the conversation to three third parties in order to route a tap.
fn expo_message(
    token: &str,
    platform: Option<&str>,
    handle: Option<&String>,
) -> serde_json::Value {
    let mut msg = serde_json::json!({
        "to": token,
        "title": "New message",
        "body": "You have a new message",
        "priority": "high",
        "data": { "h": handle },
    });
    // Android posts to the channel the client created at startup.
    if platform == Some("android") {
        msg["channelId"] = serde_json::Value::String("default".into());
        // #1508: the system posts a backgrounded app's notification itself,
        // and none of `data` is readable from the tray afterwards — only its
        // tag. Tagging it with the handle (already in the payload, random per
        // notification) is what lets the app find it again to clear it once
        // read. A per-conversation tag would have been simpler and is exactly
        // what #1122 removed: a stable value the providers could count.
        if let Some(h) = handle {
            msg["tag"] = serde_json::Value::String(h.clone());
        }
    }
    msg
}

/// Mint one opaque handle per recipient user and record what it resolves to
/// (#1122). Returns `user_id -> handle`.
///
/// 128 bits from the OS CSPRNG, fresh every notification: an HMAC of the
/// conversation id would be a stable pseudonym the providers could COUNT even
/// without naming it, which is most of what the metadata was worth.
///
/// Best-effort by design — a push nobody is waiting on must not be the thing
/// that fails a send. A user with no handle simply gets no `h`, and the client
/// falls back to `conversationId` exactly as an older client does.
pub async fn mint_push_handles(
    conn: &Connection,
    user_ids: &[String],
    conversation_id: &str,
    kind: &str,
) -> HashMap<String, String> {
    use base64::Engine as _;
    let mut out = HashMap::new();
    for user in user_ids {
        let mut raw = [0u8; 16];
        OsRng.fill_bytes(&mut raw);
        let handle = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw);
        let wrote = conn
            .execute(
                "INSERT INTO push_handle (handle, user_id, conversation_id, kind) \
                 VALUES (?1, ?2, ?3, ?4)",
                libsql::params![
                    handle.clone(),
                    user.clone(),
                    conversation_id.to_string(),
                    kind.to_string()
                ],
            )
            .await;
        match wrote {
            Ok(_) => {
                out.insert(user.clone(), handle);
            }
            // No user id: push logs name no one (#1423).
            Err(e) => tracing::warn!("push handle insert failed: {e}"),
        }
    }
    out
}

async fn is_dm(conn: &Connection, conversation_id: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM dm_channel WHERE id = ?1 LIMIT 1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

// ── POST /v1/push/resolve ────────────────────────────────────────────────────

/// How long a minted handle stays resolvable (#1122).
///
/// A notification is worth resolving for days, not months: the tap may come
/// long after the buzz, and `onDataReceived` may re-ingest in the background
/// before any tap at all. Past this the handle is swept and the client degrades
/// to opening the app.
///
/// The TTL is the whole mitigation for the one cost this design has — the DS
/// holding a durable "a notification for conversation X went to user Y" record
/// it previously only held for the instant it built the payload.
pub const PUSH_HANDLE_TTL_DAYS: i64 = 7;

/// POST `/v1/push/resolve` — trade an opaque handle for its routing target.
///
/// Scoped to the authenticated user, and a handle that is not theirs answers
/// exactly like one that does not exist: `conversation_id: None`. A distinct
/// error would confirm the handle is real, which is the one thing the opaque
/// handle exists to avoid leaking.
///
/// **Deliberately not single-use.** `onDataReceived` resolves it for a
/// background re-ingest and a later tap resolves it again to navigate; burning
/// it on first use would break tap-to-conversation for every notification the
/// client had already ingested.
pub async fn resolve_push_handle(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ResolvePushHandleBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(crate::writes::bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, None).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let found = lookup_push_handle(&conn, &who, &parsed.handle).await?;
    let (conversation_id, kind) = match found {
        Some((c, k)) => (Some(c), Some(k)),
        None => (None, None),
    };
    Ok(crate::writes::ok_response::<ResolvePushHandleBody>(
        ResolvePushHandleResponse { conversation_id, kind },
    ))
}

/// The scoped lookup: handle AND owner, inside the TTL.
///
/// `user_id` is part of the predicate rather than checked afterwards, so a
/// handle belonging to someone else is indistinguishable from an absent one at
/// the SQL level — there is no branch that could later be made to differ.
pub async fn lookup_push_handle(
    conn: &Connection,
    user_id: &str,
    handle: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let mut rows = conn
        .query(
            "SELECT conversation_id, kind FROM push_handle \
             WHERE handle = ?1 AND user_id = ?2 \
               AND created_at >= datetime('now', ?3) \
             LIMIT 1",
            libsql::params![
                handle.to_string(),
                user_id.to_string(),
                format!("-{PUSH_HANDLE_TTL_DAYS} days")
            ],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some((row.get::<String>(0)?, row.get::<String>(1)?))),
        None => Ok(None),
    }
}

/// Drop handles past the TTL. Called from the DS retention sweep.
pub async fn sweep_push_handles(conn: &Connection) -> anyhow::Result<u64> {
    let n = conn
        .execute(
            "DELETE FROM push_handle WHERE created_at < datetime('now', ?1)",
            libsql::params![format!("-{PUSH_HANDLE_TTL_DAYS} days")],
        )
        .await?;
    Ok(n)
}

// ── POST /v1/push/dismiss + /v1/push/dismissed (#1508) ───────────────────────
//
// A notification used to stay in the tray after its message was read, unless
// it was the very notification the user tapped. The reading client — any of
// them: desktop, the TUI, the same phone, another phone — now reports the
// conversations whose read cursor moved, and the DS clears that user's
// notifications for them on every device:
//
//   1. stamp `dismissed_at` on the user's handles for those conversations that
//      were minted BEFORE the read (an age, so the clocks never have to agree:
//      a message that lands after the read keeps its notification);
//   2. send each of the user's devices one silent, data-only push carrying the
//      handles — the same random values the notifications already carried, so
//      the providers learn which notifications were read, never a conversation;
//   3. keep the stamped rows until the TTL sweep, so a device that missed the
//      silent push asks `/v1/push/dismissed` on its next foreground.
//
// Rate: nothing is sent when nothing is outstanding (re-reading a conversation
// is free), the client debounces its reports, and the DS coalesces one user's
// dismissals into at most one push per device per [`DISMISS_COALESCE`].

/// How long one user's dismissals are gathered before the silent push goes.
/// Reading three conversations in a row is one push, not three. Short, because
/// the point is that the tray clears while the user is still looking.
pub const DISMISS_COALESCE: std::time::Duration = std::time::Duration::from_secs(2);

/// Handles per silent push. 100 × 22 characters stays far below the 4 KiB
/// APNs/FCM payload cap; more than that is split across pushes.
const DISMISS_HANDLES_PER_PUSH: usize = 100;

/// How long a silent dismissal may wait at the provider for an offline device.
/// Past the handle TTL the notification can no longer be matched anyway.
const DISMISS_TTL_SECS: i64 = PUSH_HANDLE_TTL_DAYS * 24 * 60 * 60;

/// The silent push that clears notifications. Built from the token, the
/// platform and the handles only, so what leaves the process can be asserted on
/// directly: no title, no body (so no OS ever shows it, and an app that does
/// not understand `d` ignores it — expo-notifications drops a data-only
/// message it has no task for, on every version we have shipped), and `data`
/// is `{ d }` and nothing else.
fn expo_dismiss_message(
    token: &str,
    platform: Option<&str>,
    handles: &[String],
) -> serde_json::Value {
    let mut msg = serde_json::json!({
        "to": token,
        "data": { "d": handles },
        // Normal: a dismissal is not worth waking a dozing phone for, and iOS
        // requires priority 5 for a background (content-available) push.
        "priority": "normal",
        "ttl": DISMISS_TTL_SECS,
    });
    if platform == Some("ios") {
        // Wakes the app in the background to run the dismissal task. Both
        // spellings: `contentAvailable` is the current field, `_contentAvailable`
        // the older one; Expo prefers the former when both are set.
        msg["contentAvailable"] = serde_json::Value::Bool(true);
        msg["_contentAvailable"] = serde_json::Value::Bool(true);
    }
    msg
}

/// Stamp the user's outstanding handles for `entries` dismissed and return
/// them. Only handles minted at or before each read (`now - read_secs_ago`),
/// within the TTL, and not already dismissed.
///
/// Best-effort like the rest of push: a database that predates migration
/// 000040 has no `dismissed_at` column, and that reads as "nothing to dismiss".
pub async fn mark_dismissed(
    conn: &Connection,
    user_id: &str,
    entries: &[DismissEntry],
    now_unix: i64,
) -> Vec<String> {
    // One cutoff per conversation: the LATEST read wins if a conversation is
    // named twice.
    let mut cutoffs: BTreeMap<&str, i64> = BTreeMap::new();
    for e in entries {
        if e.conversation_id.is_empty() || e.conversation_id.len() > 128 {
            continue;
        }
        let at = now_unix - i64::from(e.read_secs_ago);
        let slot = cutoffs.entry(e.conversation_id.as_str()).or_insert(at);
        if at > *slot {
            *slot = at;
        }
    }
    let mut handles = Vec::new();
    for (conversation_id, cutoff) in cutoffs {
        let rows = conn
            .query(
                "SELECT handle FROM push_handle \
                 WHERE user_id = ?1 AND conversation_id = ?2 AND dismissed_at IS NULL \
                   AND created_at <= datetime(?3, 'unixepoch') \
                   AND created_at >= datetime('now', ?4) \
                 ORDER BY created_at",
                libsql::params![
                    user_id.to_string(),
                    conversation_id.to_string(),
                    cutoff,
                    format!("-{PUSH_HANDLE_TTL_DAYS} days")
                ],
            )
            .await;
        let mut rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("push dismiss lookup skipped: {e}");
                return Vec::new();
            }
        };
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(h) = row.get::<String>(0) {
                handles.push(h);
            }
        }
    }
    let mut stamped = Vec::with_capacity(handles.len());
    for chunk in handles.chunks(BIND_CHUNK) {
        let ph = placeholders(chunk.len(), 2);
        let sql = format!(
            "UPDATE push_handle SET dismissed_at = datetime('now') \
             WHERE user_id = ?1 AND dismissed_at IS NULL AND handle IN ({ph})"
        );
        let mut params: Vec<libsql::Value> = vec![user_id.to_string().into()];
        params.extend(chunk.iter().map(|h| h.clone().into()));
        match conn.execute(&sql, params).await {
            Ok(_) => stamped.extend_from_slice(chunk),
            // No user id, no handle: push logs name no one (#1423).
            Err(e) => tracing::warn!("push dismiss stamp failed: {e}"),
        }
    }
    stamped
}

/// The subset of `handles` that belong to `user_id` and were dismissed.
pub async fn dismissed_handles(
    conn: &Connection,
    user_id: &str,
    handles: &[String],
) -> Vec<String> {
    let mut out = Vec::new();
    for chunk in handles.chunks(BIND_CHUNK) {
        let ph = placeholders(chunk.len(), 2);
        let sql = format!(
            "SELECT handle FROM push_handle \
             WHERE user_id = ?1 AND dismissed_at IS NOT NULL AND handle IN ({ph})"
        );
        let mut params: Vec<libsql::Value> = vec![user_id.to_string().into()];
        params.extend(chunk.iter().map(|h| h.clone().into()));
        let mut rows = match conn.query(&sql, params).await {
            Ok(r) => r,
            Err(e) => {
                tracing::debug!("push dismissed lookup skipped: {e}");
                return Vec::new();
            }
        };
        while let Ok(Some(row)) = rows.next().await {
            if let Ok(h) = row.get::<String>(0) {
                out.push(h);
            }
        }
    }
    out
}

/// Send `user_id`'s devices the silent push that clears `handles`. Never fails:
/// like every push, an error is counted and logged by class.
pub async fn send_dismissals(
    conn: &Connection,
    expo_url: &str,
    user_id: &str,
    handles: &[String],
) {
    if handles.is_empty() {
        return;
    }
    let tokens = match push_tokens(conn, &[user_id.to_string()]).await {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("push dismiss: token read failed: {e}");
            return;
        }
    };
    let mut messages = Vec::new();
    let mut message_tokens = Vec::new();
    for (token, platform, _) in &tokens {
        for chunk in handles.chunks(DISMISS_HANDLES_PER_PUSH) {
            messages.push(expo_dismiss_message(token, platform.as_deref(), chunk));
            message_tokens.push(token.clone());
        }
    }
    let auth = expo_token();
    for (chunk, chunk_tokens) in messages
        .chunks(EXPO_BATCH)
        .zip(message_tokens.chunks(EXPO_BATCH))
    {
        send_batch(conn, expo_url, auth, chunk, chunk_tokens, stats()).await;
    }
}

/// Handles waiting for their user's coalesced dismissal push. A user is in the
/// map exactly while one send is scheduled for them.
fn pending_dismissals() -> &'static Mutex<HashMap<String, Vec<String>>> {
    static PENDING: std::sync::OnceLock<Mutex<HashMap<String, Vec<String>>>> =
        std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Queue `handles` for `user_id`'s next dismissal push, scheduling it if none
/// is pending. Returns whether this call scheduled the send (the first in a
/// window), so a test can see the coalescing.
pub fn queue_dismissals(
    db: std::sync::Arc<crate::db::Db>,
    expo_url: String,
    delay: std::time::Duration,
    user_id: &str,
    handles: Vec<String>,
) -> bool {
    if handles.is_empty() {
        return false;
    }
    let schedule = {
        let mut pending = pending_dismissals()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        match pending.get_mut(user_id) {
            Some(list) => {
                list.extend(handles);
                false
            }
            None => {
                pending.insert(user_id.to_string(), handles);
                true
            }
        }
    };
    if schedule {
        let user = user_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let handles = pending_dismissals()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&user)
                .unwrap_or_default();
            let conn = match db.conn().await {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("push dismiss: conn failed: {e}");
                    return;
                }
            };
            send_dismissals(&conn, &expo_url, &user, &handles).await;
        });
    }
    schedule
}

/// POST `/v1/push/dismiss` — the caller read these conversations; clear their
/// notifications on every device of theirs (#1508).
///
/// Answers how many notifications were newly dismissed. The push itself goes
/// out after the response, coalesced per user ([`queue_dismissals`]).
pub async fn dismiss_push(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: DismissPushBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(crate::writes::bad_request("invalid body")),
    };
    if parsed.conversations.len() > DISMISS_MAX_CONVERSATIONS {
        return Ok(crate::writes::bad_request("too many conversations"));
    }
    let who = match authed_user(&state, &req, Some(&parsed.user_id)).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let handles = mark_dismissed(&conn, &who, &parsed.conversations, now).await;
    let dismissed = u32::try_from(handles.len()).unwrap_or(u32::MAX);
    queue_dismissals(
        state.db.clone(),
        EXPO_PUSH_URL.to_string(),
        DISMISS_COALESCE,
        &who,
        handles,
    );
    Ok(crate::writes::ok_response::<DismissPushBody>(
        DismissPushResponse { dismissed },
    ))
}

/// POST `/v1/push/dismissed` — which of these tray notifications were read
/// since (#1508)? The foreground catch-up for a device that missed the silent
/// push. Another user's handle is simply absent from the answer, exactly like
/// one that was never minted.
pub async fn read_dismissed_push_handles(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: DismissedPushHandlesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(crate::writes::bad_request("invalid body")),
    };
    if parsed.handles.len() > DISMISSED_QUERY_MAX_HANDLES {
        return Ok(crate::writes::bad_request("too many handles"));
    }
    let who = match authed_user(&state, &req, Some(&parsed.user_id)).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let dismissed = dismissed_handles(&conn, &who, &parsed.handles).await;
    Ok(crate::writes::ok_response::<DismissedPushHandlesBody>(
        DismissedPushHandlesResponse { dismissed },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Authorization` header the fan-out would actually put on the wire.
    fn authorization(token: Option<&str>) -> Option<String> {
        let empty: [serde_json::Value; 0] = [];
        let req = expo_request(EXPO_PUSH_URL, token, &empty[..])
            .build()
            .expect("request builds");
        req.headers()
            .get(reqwest::header::AUTHORIZATION)
            .map(|v| v.to_str().expect("header is ascii").to_string())
    }

    #[test]
    fn a_configured_token_is_sent_as_a_bearer() {
        assert_eq!(
            authorization(Some("expo-secret")).as_deref(),
            Some("Bearer expo-secret")
        );
    }

    /// Without a token the header must be ABSENT rather than empty — an empty
    /// bearer is a credential Expo would reject once enforcement is on, and it
    /// would read as "authenticated" to anyone auditing the call.
    #[test]
    fn no_authorization_header_without_a_token() {
        assert_eq!(authorization(None), None);
    }

    /// #1157: the payload carries the handle and nothing else.
    ///
    /// Asserted on the SERIALIZED `data` blob rather than on fields, because
    /// that string is exactly what Expo, APNs and FCM receive. A test that
    /// checked named keys would pass while some future field quietly reappeared
    /// beside them.
    #[test]
    fn the_payload_data_carries_only_the_handle() {
        let handle = "Zm9vYmFyMTIzNDU2Nzg5".to_string();
        let msg = expo_message("ExponentPushToken[abc]", Some("ios"), Some(&handle));

        let data = msg.get("data").expect("data blob");
        let obj = data.as_object().expect("data is an object");
        assert_eq!(
            obj.keys().collect::<Vec<_>>(),
            vec!["h"],
            "data must contain exactly one key; got {obj:?}"
        );
        assert_eq!(obj.get("h").unwrap().as_str(), Some(handle.as_str()));

        // And nothing anywhere in the message names a conversation or a kind.
        let wire = serde_json::to_string(&msg).unwrap();
        for forbidden in ["conversationId", "conversation_id", "kind", "senderId"] {
            assert!(
                !wire.contains(forbidden),
                "`{forbidden}` must not appear in a push payload: {wire}"
            );
        }
    }

    /// A recipient whose handle could not be minted still gets the buzz, with
    /// no routing — never a conversation id as a consolation prize.
    #[test]
    fn a_missing_handle_degrades_to_no_routing() {
        let msg = expo_message("ExponentPushToken[abc]", None, None);
        assert!(msg["data"]["h"].is_null());
        assert_eq!(msg["title"].as_str(), Some("New message"));
        let wire = serde_json::to_string(&msg).unwrap();
        assert!(!wire.contains("conversationId"));
    }

    /// The Android channel is still set, and only for Android.
    #[test]
    fn the_android_channel_survives_the_payload_change() {
        let h = "aGFuZGxl".to_string();
        assert_eq!(
            expo_message("t", Some("android"), Some(&h))["channelId"].as_str(),
            Some("default")
        );
        assert!(expo_message("t", Some("ios"), Some(&h)).get("channelId").is_none());
    }

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn
    }

    /// A handle resolves only for the user it was minted for, and only to what
    /// it was minted for.
    #[tokio::test]
    async fn a_handle_resolves_for_its_owner() {
        let c = conn().await;
        let handles =
            mint_push_handles(&c, &["alice".to_string()], "conv-1", "dm").await;
        let h = handles.get("alice").expect("alice got a handle");

        assert_eq!(
            lookup_push_handle(&c, "alice", h).await.unwrap(),
            Some(("conv-1".to_string(), "dm".to_string()))
        );
    }

    /// Someone else's handle is indistinguishable from one that never existed.
    ///
    /// The point of the opaque handle is that possessing it proves nothing, so
    /// the resolve must not confirm it is real to a caller it does not belong
    /// to — same answer for "not yours" and "not a handle".
    #[tokio::test]
    async fn another_users_handle_is_indistinguishable_from_a_missing_one() {
        let c = conn().await;
        let handles =
            mint_push_handles(&c, &["alice".to_string()], "conv-1", "dm").await;
        let h = handles.get("alice").unwrap();

        assert_eq!(
            lookup_push_handle(&c, "bob", h).await.unwrap(),
            None,
            "bob must not be able to resolve alice's handle"
        );
        assert_eq!(
            lookup_push_handle(&c, "bob", "never-minted").await.unwrap(),
            None
        );
    }

    /// Every notification gets a fresh handle.
    ///
    /// This is the whole reason the design is a random handle rather than an
    /// HMAC of the conversation id: a stable per-conversation pseudonym would
    /// let Expo/APNs/FCM COUNT a conversation without naming it, which is most
    /// of what the metadata was worth.
    #[tokio::test]
    async fn handles_are_never_reused_across_notifications() {
        let c = conn().await;
        let users = vec!["alice".to_string()];
        let mut seen = std::collections::HashSet::new();
        for _ in 0..16 {
            let h = mint_push_handles(&c, &users, "conv-1", "dm")
                .await
                .remove("alice")
                .expect("minted");
            assert!(
                seen.insert(h),
                "the same conversation must not produce a repeated handle"
            );
        }
        assert_eq!(seen.len(), 16);
    }

    /// One handle per recipient user, and users do not share.
    #[tokio::test]
    async fn each_recipient_gets_their_own_handle() {
        let c = conn().await;
        let handles = mint_push_handles(
            &c,
            &["alice".to_string(), "bob".to_string()],
            "conv-1",
            "channel",
        )
        .await;
        let a = handles.get("alice").unwrap();
        let b = handles.get("bob").unwrap();
        assert_ne!(a, b, "two recipients must not share a handle");
        // Each resolves for its owner and for nobody else.
        assert!(lookup_push_handle(&c, "alice", a).await.unwrap().is_some());
        assert!(lookup_push_handle(&c, "bob", b).await.unwrap().is_some());
        assert_eq!(lookup_push_handle(&c, "alice", b).await.unwrap(), None);
        assert_eq!(lookup_push_handle(&c, "bob", a).await.unwrap(), None);
    }

    /// Past the TTL a handle stops resolving, and the sweep removes it.
    ///
    /// The TTL is the mitigation for this table's one cost — a durable record
    /// that a notification for conversation X went to user Y — so "it expires"
    /// has to be a property, not a comment.
    #[tokio::test]
    async fn an_expired_handle_stops_resolving_and_is_swept() {
        let c = conn().await;
        let handles =
            mint_push_handles(&c, &["alice".to_string()], "conv-1", "dm").await;
        let h = handles.get("alice").unwrap().clone();

        // Age the row past the TTL.
        c.execute(
            "UPDATE push_handle SET created_at = datetime('now', ?1)",
            libsql::params![format!("-{} days", PUSH_HANDLE_TTL_DAYS + 1)],
        )
        .await
        .unwrap();

        assert_eq!(
            lookup_push_handle(&c, "alice", &h).await.unwrap(),
            None,
            "an expired handle must not resolve"
        );
        assert_eq!(sweep_push_handles(&c).await.unwrap(), 1);

        // And a fresh one survives the sweep, so it is not deleting everything.
        let fresh = mint_push_handles(&c, &["alice".to_string()], "conv-2", "dm").await;
        assert_eq!(sweep_push_handles(&c).await.unwrap(), 0);
        assert!(lookup_push_handle(&c, "alice", fresh.get("alice").unwrap())
            .await
            .unwrap()
            .is_some());
    }

    // ── #1423: tickets, receipts, pruning ────────────────────────────────────

    /// A stand-in for Expo: answers every POST with `status` and `body`, and
    /// keeps what it was sent. A real socket and the real client, so the
    /// request the DS builds is the one that goes out.
    async fn fake_expo(
        status: u16,
        body: serde_json::Value,
    ) -> (String, std::sync::Arc<Mutex<Vec<serde_json::Value>>>) {
        use axum::routing::post;
        let seen = std::sync::Arc::new(Mutex::new(Vec::new()));
        let seen2 = seen.clone();
        let app = axum::Router::new().route(
            "/",
            post(move |Json(sent): Json<serde_json::Value>| {
                let seen = seen2.clone();
                let body = body.clone();
                async move {
                    seen.lock().unwrap().push(sent);
                    (StatusCode::from_u16(status).unwrap(), Json(body))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}/"), seen)
    }

    async fn add_token(c: &Connection, token: &str, user: &str) {
        c.execute(
            "INSERT INTO push_token (token, user_id, platform, updated_at) \
             VALUES (?1, ?2, 'android', '2026-10-24T00:00:00Z')",
            libsql::params![token.to_string(), user.to_string()],
        )
        .await
        .unwrap();
    }

    async fn tokens_left(c: &Connection) -> Vec<String> {
        let mut rows = c
            .query("SELECT token FROM push_token ORDER BY token", ())
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(r.get::<String>(0).unwrap());
        }
        out
    }

    async fn tickets_left(c: &Connection) -> Vec<(String, String)> {
        let mut rows = c
            .query("SELECT ticket_id, token FROM push_ticket ORDER BY ticket_id", ())
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push((r.get::<String>(0).unwrap(), r.get::<String>(1).unwrap()));
        }
        out
    }

    fn counts(stats: &PushStats) -> serde_json::Value {
        stats.snapshot()
    }

    #[test]
    fn tickets_are_classified_and_unknown_errors_are_other() {
        let body = serde_json::json!({ "data": [
            { "status": "ok", "id": "t1" },
            { "status": "error", "message": "\"ExponentPushToken[x]\" is not registered",
              "details": { "error": "DeviceNotRegistered", "expoPushToken": "ExponentPushToken[x]" } },
            { "status": "error", "details": { "error": "MessageRateExceeded" } },
            { "status": "error", "details": { "error": "ExponentPushToken[leak]" } },
            { "status": "error" },
            "garbage",
        ]});
        assert_eq!(
            parse_tickets(&body).unwrap(),
            vec![
                PushOutcome::Ok { id: Some("t1".into()) },
                PushOutcome::Error(PushErrorClass::DeviceNotRegistered),
                PushOutcome::Error(PushErrorClass::MessageRateExceeded),
                PushOutcome::Error(PushErrorClass::Other),
                PushOutcome::Error(PushErrorClass::Other),
                PushOutcome::Error(PushErrorClass::Other),
            ]
        );
        // A request-level error has no tickets to read.
        assert_eq!(parse_tickets(&serde_json::json!({ "errors": [] })), None);
    }

    /// Only Expo-shaped codes survive into a log line or the counts.
    #[test]
    fn request_error_codes_cannot_carry_a_token() {
        let body = serde_json::json!({ "errors": [
            { "code": "PUSH_TOO_MANY_EXPERIENCE_IDS", "details": { "x": ["ExponentPushToken[a]"] } },
            { "code": "ExponentPushToken[a]" },
            { "code": "user@example.com" },
        ]});
        assert_eq!(request_error_codes(&body), vec!["PUSH_TOO_MANY_EXPERIENCE_IDS"]);
    }

    /// The bug: a 200 with error tickets used to be read as success. Now each
    /// ticket is acted on: `DeviceNotRegistered` prunes its token, an accepted
    /// ticket is kept for its receipt, other errors are only counted.
    #[tokio::test]
    async fn tickets_prune_dead_tokens_and_keep_ids_for_receipts() {
        let c = conn().await;
        add_token(&c, "ExponentPushToken[live]", "alice").await;
        add_token(&c, "ExponentPushToken[dead]", "bob").await;
        add_token(&c, "ExponentPushToken[busy]", "carol").await;
        let (url, seen) = fake_expo(
            200,
            serde_json::json!({ "data": [
                { "status": "ok", "id": "ticket-live" },
                { "status": "error", "details": { "error": "DeviceNotRegistered" } },
                { "status": "error", "details": { "error": "MessageRateExceeded" } },
            ]}),
        )
        .await;
        let tokens = vec![
            "ExponentPushToken[live]".to_string(),
            "ExponentPushToken[dead]".to_string(),
            "ExponentPushToken[busy]".to_string(),
        ];
        let messages: Vec<_> = tokens
            .iter()
            .map(|t| expo_message(t, Some("android"), None))
            .collect();
        let stats = PushStats::default();

        send_batch(&c, &url, None, &messages, &tokens, &stats).await;

        assert_eq!(seen.lock().unwrap().len(), 1, "one request for the batch");
        assert_eq!(
            tokens_left(&c).await,
            vec!["ExponentPushToken[busy]", "ExponentPushToken[live]"],
            "only the DeviceNotRegistered token is pruned"
        );
        assert_eq!(
            tickets_left(&c).await,
            vec![("ticket-live".to_string(), "ExponentPushToken[live]".to_string())]
        );
        let snap = counts(&stats);
        assert_eq!(snap["messages_sent"], 3);
        assert_eq!(snap["tickets_ok"], 1);
        assert_eq!(snap["ticket_errors"]["DeviceNotRegistered"], 1);
        assert_eq!(snap["ticket_errors"]["MessageRateExceeded"], 1);
        assert_eq!(snap["tokens_pruned"], 1);
        // The counts name no token and no user.
        let wire = snap.to_string();
        assert!(!wire.contains("ExponentPushToken"), "{wire}");
        assert!(!wire.contains("alice") && !wire.contains("bob"), "{wire}");
    }

    /// A request-level failure is counted and touches nothing.
    #[tokio::test]
    async fn a_failed_request_is_counted_and_prunes_nothing() {
        let c = conn().await;
        add_token(&c, "ExponentPushToken[a]", "alice").await;
        let (url, _) = fake_expo(
            400,
            serde_json::json!({ "errors": [{ "code": "PUSH_TOO_MANY_EXPERIENCE_IDS" }] }),
        )
        .await;
        let tokens = vec!["ExponentPushToken[a]".to_string()];
        let messages = vec![expo_message(&tokens[0], None, None)];
        let stats = PushStats::default();

        send_batch(&c, &url, None, &messages, &tokens, &stats).await;

        assert_eq!(tokens_left(&c).await, vec!["ExponentPushToken[a]"]);
        assert!(tickets_left(&c).await.is_empty());
        let snap = counts(&stats);
        assert_eq!(snap["send_request_failures"], 1);
        assert_eq!(snap["send_request_errors"]["PUSH_TOO_MANY_EXPERIENCE_IDS"], 1);
        assert_eq!(snap["messages_sent"], 0);
    }

    /// An unreachable Expo is a counted failure, not an error or a panic: the
    /// send that triggered it has already been answered.
    #[tokio::test]
    async fn an_unreachable_expo_is_counted_not_raised() {
        let c = conn().await;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/", listener.local_addr().unwrap());
        drop(listener);
        let tokens = vec!["ExponentPushToken[a]".to_string()];
        let messages = vec![expo_message(&tokens[0], None, None)];
        let stats = PushStats::default();

        send_batch(&c, &url, None, &messages, &tokens, &stats).await;

        assert_eq!(counts(&stats)["send_request_failures"], 1);
    }

    /// Receipts: a `DeviceNotRegistered` receipt prunes the token, an answered
    /// ticket is deleted either way, an unanswered one waits, and one past 24
    /// hours is dropped without being asked about.
    #[tokio::test]
    async fn receipts_prune_dead_tokens_and_purge_answered_and_expired_tickets() {
        let c = conn().await;
        add_token(&c, "ExponentPushToken[ok]", "alice").await;
        add_token(&c, "ExponentPushToken[dead]", "bob").await;
        add_token(&c, "ExponentPushToken[wait]", "carol").await;
        add_token(&c, "ExponentPushToken[old]", "dave").await;
        let now = crate::util::now_unix() as i64;
        for (id, token, at) in [
            ("r-ok", "ExponentPushToken[ok]", now),
            ("r-dead", "ExponentPushToken[dead]", now),
            ("r-wait", "ExponentPushToken[wait]", now),
            ("r-old", "ExponentPushToken[old]", now - PUSH_TICKET_MAX_AGE_SECS - 60),
        ] {
            c.execute(
                "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES (?1, ?2, ?3)",
                libsql::params![id, token, at],
            )
            .await
            .unwrap();
        }
        let (url, seen) = fake_expo(
            200,
            serde_json::json!({ "data": {
                "r-ok": { "status": "ok" },
                "r-dead": { "status": "error", "details": { "error": "DeviceNotRegistered" } },
                "not-ours": { "status": "error", "details": { "error": "DeviceNotRegistered" } },
            }}),
        )
        .await;
        let stats = PushStats::default();

        let sweep = check_receipts_at(&c, &url, None, &stats).await.unwrap();

        assert_eq!(sweep.expired, 1);
        assert_eq!(sweep.checked, 2);
        assert_eq!(sweep.errors, 1);
        let asked = seen.lock().unwrap()[0]["ids"].clone();
        let mut asked: Vec<String> = serde_json::from_value(asked).unwrap();
        asked.sort();
        assert_eq!(asked, vec!["r-dead", "r-ok", "r-wait"], "the expired ticket is not asked about");
        assert_eq!(
            tickets_left(&c).await,
            vec![("r-wait".to_string(), "ExponentPushToken[wait]".to_string())]
        );
        assert_eq!(
            tokens_left(&c).await,
            vec!["ExponentPushToken[ok]", "ExponentPushToken[old]", "ExponentPushToken[wait]"]
        );
        let snap = counts(&stats);
        assert_eq!(snap["receipts_ok"], 1);
        assert_eq!(snap["receipt_errors"]["DeviceNotRegistered"], 1);
        assert_eq!(snap["tickets_expired"], 1);
        assert_eq!(snap["tokens_pruned"], 1);
    }

    /// A failed receipts request keeps every ticket for the next sweep.
    #[tokio::test]
    async fn a_failed_receipts_request_keeps_the_tickets() {
        let c = conn().await;
        let now = crate::util::now_unix() as i64;
        c.execute(
            "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES ('r1', 'ExponentPushToken[a]', ?1)",
            libsql::params![now],
        )
        .await
        .unwrap();
        let (url, _) = fake_expo(503, serde_json::json!({})).await;
        let stats = PushStats::default();

        let sweep = check_receipts_at(&c, &url, None, &stats).await.unwrap();

        assert_eq!(sweep.checked, 0);
        assert_eq!(tickets_left(&c).await.len(), 1);
        assert_eq!(counts(&stats)["receipt_request_failures"], 1);
    }

    /// The table bounds itself: a ticket older than a day cannot outlive the
    /// next insert, even if the sweep never runs.
    #[tokio::test]
    async fn the_ticket_table_drops_day_old_rows_on_insert() {
        let c = conn().await;
        let now = crate::util::now_unix() as i64;
        c.execute(
            "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES ('old', 't', ?1)",
            libsql::params![now - PUSH_TICKET_MAX_AGE_SECS - 1],
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO push_ticket (ticket_id, token, created_at) VALUES ('new', 't', ?1)",
            libsql::params![now],
        )
        .await
        .unwrap();
        assert_eq!(
            tickets_left(&c).await,
            vec![("new".to_string(), "t".to_string())]
        );
    }

    // ── #1431: channel pushes by default, per-channel mute ────────────────────

    /// A group with one channel: alice (the sender), bob (who muted the
    /// channel) and carol, each with one push token.
    async fn muted_channel_fixture() -> Connection {
        let c = conn().await;
        for sql in [
            "INSERT INTO conversation (id, kind) VALUES ('g1', 'group')",
            "INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice')",
            "INSERT INTO conversation (id, kind) VALUES ('ch1', 'channel')",
            "INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general')",
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
            "INSERT INTO channel_mute (user_id, channel_id) VALUES ('bob', 'ch1')",
        ] {
            c.execute(sql, ()).await.unwrap();
        }
        add_token(&c, "ExponentPushToken[alice]", "alice").await;
        add_token(&c, "ExponentPushToken[bob]", "bob").await;
        add_token(&c, "ExponentPushToken[carol]", "carol").await;
        c
    }

    /// Which tokens one send woke, by running the real fan-out against a fake
    /// Expo and reading back what it was sent.
    async fn woken(c: &Connection, audience: &PushAudience) -> Vec<String> {
        let (url, seen) = fake_expo(200, serde_json::json!({ "data": [] })).await;
        notify_new_message_at(c, &url, "ch1", "alice", audience)
            .await
            .unwrap();
        let mut out: Vec<String> = seen
            .lock()
            .unwrap()
            .iter()
            .flat_map(|batch| batch.as_array().cloned().unwrap_or_default())
            .map(|m| m["to"].as_str().unwrap().to_string())
            .collect();
        out.sort();
        out
    }

    /// The issue's acceptance test: a user who muted a channel gets no push
    /// for ordinary chatter or for `@all`, but does for a direct `@username`.
    /// Everyone else in the channel is pushed for all three.
    #[tokio::test]
    async fn a_muted_member_is_pushed_only_when_mentioned_by_name() {
        let c = muted_channel_fixture().await;
        let carol_only = vec!["ExponentPushToken[carol]".to_string()];
        let both = vec![
            "ExponentPushToken[bob]".to_string(),
            "ExponentPushToken[carol]".to_string(),
        ];

        // Chatter and `@all` look the same on the wire from a current client:
        // everyone, nobody named.
        let chatter = PushAudience::from_send(Some(vec![]), Some(vec![])).unwrap();
        assert_eq!(woken(&c, &chatter).await, carol_only, "chatter skips the muted member");
        assert_eq!(woken(&c, &chatter).await, carol_only, "@all skips the muted member");

        // `@bob`: everyone is still woken, and bob gets through his mute.
        let at_bob =
            PushAudience::from_send(Some(vec![]), Some(vec!["bob".to_string()])).unwrap();
        assert_eq!(woken(&c, &at_bob).await, both, "a direct mention reaches a muted member");

        // The sender is never pushed their own message.
        assert!(!woken(&c, &at_bob).await.contains(&"ExponentPushToken[alice]".to_string()));
    }

    /// The oldest store build keeps working against a mute-aware DS: its
    /// chatter (`push_to` absent) still pushes nobody, its `@all` respects the
    /// mute, and its `@username` (a non-empty `push_to`, no `push_mentions`)
    /// still reaches exactly the members named, through a mute.
    #[tokio::test]
    async fn an_old_client_send_keeps_its_meaning() {
        let c = muted_channel_fixture().await;

        assert_eq!(PushAudience::from_send(None, None), None, "old chatter: no push");

        let old_all = PushAudience::from_send(Some(vec![]), None).unwrap();
        assert_eq!(woken(&c, &old_all).await, vec!["ExponentPushToken[carol]".to_string()]);

        let old_at_bob = PushAudience::from_send(Some(vec!["bob".to_string()]), None).unwrap();
        assert_eq!(woken(&c, &old_at_bob).await, vec!["ExponentPushToken[bob]".to_string()]);
    }

    /// A mention list cannot widen the audience past the conversation: naming
    /// someone who is not a member wakes nobody extra.
    #[tokio::test]
    async fn a_mention_cannot_reach_outside_the_channel() {
        let c = muted_channel_fixture().await;
        add_token(&c, "ExponentPushToken[mallory]", "mallory").await;
        let audience =
            PushAudience::from_send(Some(vec![]), Some(vec!["mallory".to_string()])).unwrap();
        assert_eq!(woken(&c, &audience).await, vec!["ExponentPushToken[carol]".to_string()]);
    }

    // ── #1508: a read clears its notifications on every device ───────────────

    fn entry(conversation_id: &str, read_secs_ago: u32) -> DismissEntry {
        DismissEntry {
            conversation_id: conversation_id.to_string(),
            read_secs_ago,
        }
    }

    fn now() -> i64 {
        crate::util::now_unix() as i64
    }

    /// Android tags the notification with its handle, so the app can find a
    /// system-posted notification in the tray again; iOS needs no tag (the
    /// handle is readable from a delivered notification's payload there).
    #[test]
    fn an_android_notification_is_tagged_with_its_handle() {
        let h = "aGFuZGxlLXRhZw".to_string();
        let android = expo_message("t", Some("android"), Some(&h));
        assert_eq!(android["tag"].as_str(), Some(h.as_str()));
        assert!(expo_message("t", Some("ios"), Some(&h)).get("tag").is_none());
        // No handle, no tag — never a conversation-derived stand-in.
        assert!(expo_message("t", Some("android"), None).get("tag").is_none());
    }

    /// The silent push: `data` is `{ d }` and nothing else, no title or body
    /// (so no OS shows it and an older app ignores it), and iOS asks to be
    /// woken in the background.
    #[test]
    fn the_dismissal_push_is_silent_and_carries_only_handles() {
        let handles = vec!["aaaa".to_string(), "bbbb".to_string()];
        for platform in [Some("android"), Some("ios"), None] {
            let msg = expo_dismiss_message("ExponentPushToken[x]", platform, &handles);
            let data = msg["data"].as_object().expect("data object");
            assert_eq!(data.keys().collect::<Vec<_>>(), vec!["d"]);
            assert_eq!(msg["data"]["d"], serde_json::json!(["aaaa", "bbbb"]));
            assert!(msg.get("title").is_none() && msg.get("body").is_none(), "{msg}");
            assert_eq!(msg["priority"].as_str(), Some("normal"));
            let wire = serde_json::to_string(&msg).unwrap();
            for forbidden in ["conversationId", "conversation_id", "kind", "channelId"] {
                assert!(!wire.contains(forbidden), "`{forbidden}` in {wire}");
            }
        }
        let ios = expo_dismiss_message("t", Some("ios"), &handles);
        assert_eq!(ios["contentAvailable"], serde_json::json!(true));
        let android = expo_dismiss_message("t", Some("android"), &handles);
        assert!(android.get("contentAvailable").is_none());
    }

    /// The core rule: only the reader's own handles, only for the conversations
    /// read, only ones minted before the read — and only once.
    #[tokio::test]
    async fn a_read_dismisses_only_the_readers_earlier_notifications_once() {
        let c = conn().await;
        let alice = vec!["alice".to_string()];
        let a1 = mint_push_handles(&c, &alice, "conv-1", "dm").await["alice"].clone();
        let a2 = mint_push_handles(&c, &alice, "conv-1", "dm").await["alice"].clone();
        let other_conv = mint_push_handles(&c, &alice, "conv-2", "dm").await["alice"].clone();
        let bob = mint_push_handles(&c, &["bob".to_string()], "conv-1", "dm").await["bob"].clone();

        // A read that happened a minute before these were minted clears none
        // of them: those messages arrived after it.
        assert!(mark_dismissed(&c, "alice", &[entry("conv-1", 60)], now()).await.is_empty());

        // A read now (one second of margin past the mint) clears alice's two
        // conv-1 notifications and nothing else.
        let mut got = mark_dismissed(&c, "alice", &[entry("conv-1", 0)], now() + 1).await;
        got.sort();
        let mut want = vec![a1.clone(), a2.clone()];
        want.sort();
        assert_eq!(got, want);

        // Re-reading is free: nothing outstanding, nothing to push.
        assert!(mark_dismissed(&c, "alice", &[entry("conv-1", 0)], now() + 1).await.is_empty());

        // The catch-up query answers for alice's dismissed handles only.
        let asked = vec![a1.clone(), a2.clone(), other_conv.clone(), bob.clone(), "nope".into()];
        let mut dismissed = dismissed_handles(&c, "alice", &asked).await;
        dismissed.sort();
        assert_eq!(dismissed, want);
        // Bob's own handle is not dismissed, and alice's are not his to see.
        assert!(dismissed_handles(&c, "bob", &asked).await.is_empty());

        // Naming someone else's conversation does not reach their handles.
        assert!(mark_dismissed(&c, "mallory", &[entry("conv-1", 0)], now() + 1).await.is_empty());
        assert!(dismissed_handles(&c, "bob", &[bob]).await.is_empty());
    }

    /// An expired handle is past matching; a malformed id is skipped.
    #[tokio::test]
    async fn expired_handles_and_bad_ids_are_ignored() {
        let c = conn().await;
        mint_push_handles(&c, &["alice".to_string()], "conv-1", "dm").await;
        c.execute(
            "UPDATE push_handle SET created_at = datetime('now', ?1)",
            libsql::params![format!("-{} days", PUSH_HANDLE_TTL_DAYS + 1)],
        )
        .await
        .unwrap();
        assert!(mark_dismissed(&c, "alice", &[entry("conv-1", 0)], now() + 1).await.is_empty());
        let long = "x".repeat(129);
        assert!(mark_dismissed(&c, "alice", &[entry("", 0), entry(&long, 0)], now()).await.is_empty());
    }

    /// A database from before migration 000040 has no `dismissed_at`: the
    /// endpoints answer "nothing" rather than failing.
    #[tokio::test]
    async fn a_database_without_the_column_dismisses_nothing() {
        let c = conn().await;
        // The real schema, minus what 000040 added.
        c.execute("ALTER TABLE push_handle DROP COLUMN dismissed_at", ())
            .await
            .unwrap();
        c.execute(
            "INSERT INTO push_handle (handle, user_id, conversation_id, kind) \
             VALUES ('h1', 'alice', 'conv-1', 'dm')",
            (),
        )
        .await
        .unwrap();
        assert!(mark_dismissed(&c, "alice", &[entry("conv-1", 0)], now() + 1).await.is_empty());
        assert!(dismissed_handles(&c, "alice", &["h1".to_string()]).await.is_empty());
    }

    /// The silent push reaches every device of the reader and nobody else's,
    /// and splits a long list rather than overflowing the payload.
    #[tokio::test]
    async fn dismissals_reach_every_device_of_the_reader_only() {
        let c = conn().await;
        add_token(&c, "ExponentPushToken[alice-phone]", "alice").await;
        c.execute(
            "INSERT INTO push_token (token, user_id, platform, updated_at) \
             VALUES ('ExponentPushToken[alice-ipad]', 'alice', 'ios', '2026-10-24T00:00:00Z')",
            (),
        )
        .await
        .unwrap();
        add_token(&c, "ExponentPushToken[bob]", "bob").await;
        let (url, seen) = fake_expo(200, serde_json::json!({ "data": [] })).await;
        let handles: Vec<String> = (0..150).map(|i| format!("h{i:03}")).collect();

        send_dismissals(&c, &url, "alice", &handles).await;

        let sent: Vec<serde_json::Value> = seen
            .lock()
            .unwrap()
            .iter()
            .flat_map(|b| b.as_array().cloned().unwrap_or_default())
            .collect();
        // Two devices × two chunks (100 + 50).
        assert_eq!(sent.len(), 4, "{sent:?}");
        let mut to: Vec<&str> = sent.iter().map(|m| m["to"].as_str().unwrap()).collect();
        to.sort();
        to.dedup();
        assert_eq!(to, vec!["ExponentPushToken[alice-ipad]", "ExponentPushToken[alice-phone]"]);
        let total: usize = sent.iter().map(|m| m["data"]["d"].as_array().unwrap().len()).sum();
        assert_eq!(total, 300);
        for m in &sent {
            let ios = m["to"].as_str().unwrap().contains("ipad");
            assert_eq!(m.get("contentAvailable").is_some(), ios, "{m}");
        }
    }

    /// Several reads inside the coalescing window are one push per device.
    #[tokio::test]
    async fn dismissals_within_the_window_are_coalesced_into_one_push() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("db.db");
        let db = std::sync::Arc::new(
            crate::db::Db::connect_local(path.to_str().unwrap()).await.unwrap(),
        );
        {
            let c = db.conn().await.unwrap();
            pollis_schema::apply::single_db(&c).await.unwrap();
            add_token(&c, "ExponentPushToken[carol]", "carol-1508").await;
        }
        let (url, seen) = fake_expo(200, serde_json::json!({ "data": [] })).await;
        let window = std::time::Duration::from_millis(200);

        assert!(queue_dismissals(db.clone(), url.clone(), window, "carol-1508", vec!["h1".into()]));
        assert!(!queue_dismissals(db.clone(), url.clone(), window, "carol-1508", vec!["h2".into()]));
        assert!(!queue_dismissals(db.clone(), url.clone(), window, "carol-1508", vec![]));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;

        let batches = seen.lock().unwrap().clone();
        assert_eq!(batches.len(), 1, "one request: {batches:?}");
        assert_eq!(batches[0][0]["data"]["d"], serde_json::json!(["h1", "h2"]));

        // The window closed, so the next read schedules a fresh push.
        assert!(queue_dismissals(db.clone(), url, window, "carol-1508", vec!["h3".into()]));
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}
