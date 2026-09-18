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
//! `{ conversationId, kind }` and nothing else — enough for the client to route
//! and re-ingest the (still-encrypted) message locally, never the plaintext, the
//! sender, or any content. Expo/APNs/FCM therefore learn no more than the DS
//! already knows: that a conversation had activity. This is the same approach
//! Signal and WhatsApp take when the server cannot decrypt.
//!
//! Best-effort throughout: a push that fails to send must never fail a send that
//! already landed.

use std::collections::HashMap;

use axum::extract::State;
use axum::response::Response;
use libsql::Connection;
use pollis_api::devices::{ResolvePushHandleBody, ResolvePushHandleResponse};
use rand::rngs::OsRng;
use rand::RngCore as _;

use crate::error::AppError;
use crate::reads::authed_user;
use crate::writes::RawRequest;
use crate::AppState;
use crate::util::{BIND_CHUNK, placeholders};

/// Expo's push service endpoint. Accepts a JSON array of up to 100 messages.
const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";

/// Expo accepts up to 100 messages per request.
const EXPO_BATCH: usize = 100;

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
fn expo_request(token: Option<&str>, chunk: &[serde_json::Value]) -> reqwest::RequestBuilder {
    let req = crate::util::http_post(crate::util::Upstream::ExpoPush, EXPO_PUSH_URL).json(chunk);
    match token {
        Some(token) => req.bearer_auth(token),
        None => req,
    }
}

/// Wake every other member of a conversation with a content-free notification.
///
/// `mention_only` narrows the recipients to a subset — used by per-user
/// `@username` mentions (#843), where a channel message must wake exactly the
/// people named and nobody else. `None` means every member, which is the DM and
/// `@all` behaviour. The subset is INTERSECTED with real membership rather than
/// trusted, so it can only ever REMOVE recipients: it cannot be used to push to
/// someone outside the conversation.
pub async fn notify_new_message(
    conn: &Connection,
    conversation_id: &str,
    sender_id: &str,
    mention_only: Option<&[String]>,
) -> anyhow::Result<()> {
    // Recipients = conversation members other than the sender. `is_member`'s
    // three-legged id namespace applies here too: the id may be a DM, a group or
    // a channel, and a channel's members live on its group.
    let mut user_ids = conversation_recipients(conn, conversation_id, sender_id).await?;
    if let Some(allowed) = mention_only {
        user_ids.retain(|u| allowed.contains(u));
    }
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
        .into_iter()
        .map(|(token, platform, user_id)| {
            // Generic, content-free alert — the data fields drive routing and a
            // local re-ingest; the body intentionally reveals nothing.
            let mut msg = serde_json::json!({
                "to": token,
                "title": "New message",
                "body": "You have a new message",
                "priority": "high",
                // `h` is the opaque handle (#1122); the client resolves it over
                // its authenticated channel. `conversationId` and `kind` stay
                // for ONE release so shipped clients keep routing taps — see
                // the rollout note on #1122. A follow-up drops them once
                // clients that prefer `h` are the floor.
                "data": {
                    "h": handles.get(&user_id),
                    "conversationId": conversation_id,
                    "kind": kind,
                },
            });
            // Android posts to the channel the client created at startup.
            if platform.as_deref() == Some("android") {
                msg["channelId"] = serde_json::Value::String("default".into());
            }
            msg
        })
        .collect();

    let token = expo_token();
    for chunk in messages.chunks(EXPO_BATCH) {
        // Through `util::http_post`, which forces a deadline (#913). A push
        // nobody is waiting on must never be the thing that pins a handler open.
        match expo_request(token, chunk).send().await {
            Ok(r) if !r.status().is_success() => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                tracing::warn!("expo push non-success {status}: {body}");
            }
            Err(e) => tracing::warn!("expo push send failed: {e}"),
            _ => {}
        }
    }
    Ok(())
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
            Err(e) => tracing::warn!("push handle insert failed for {user}: {e}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The `Authorization` header the fan-out would actually put on the wire.
    fn authorization(token: Option<&str>) -> Option<String> {
        let req = expo_request(token, &[]).build().expect("request builds");
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
        for _ in 0..8 {
            let h = mint_push_handles(&c, &users, "conv-1", "dm")
                .await
                .remove("alice")
                .expect("minted");
            assert!(
                seen.insert(h),
                "the same conversation must not produce a repeated handle"
            );
        }
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
}
