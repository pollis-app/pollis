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

use libsql::Connection;

/// Expo's push service endpoint. Accepts a JSON array of up to 100 messages.
const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";

/// Expo accepts up to 100 messages per request.
const EXPO_BATCH: usize = 100;

const BIND_CHUNK: usize = 100;

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

    let messages: Vec<serde_json::Value> = tokens
        .into_iter()
        .map(|(token, platform)| {
            // Generic, content-free alert — the data fields drive routing and a
            // local re-ingest; the body intentionally reveals nothing.
            let mut msg = serde_json::json!({
                "to": token,
                "title": "New message",
                "body": "You have a new message",
                "priority": "high",
                "data": {
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

async fn push_tokens(
    conn: &Connection,
    user_ids: &[String],
) -> anyhow::Result<Vec<(String, Option<String>)>> {
    let mut out = Vec::new();
    for chunk in user_ids.chunks(BIND_CHUNK) {
        let placeholders = (0..chunk.len())
            .map(|i| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql =
            format!("SELECT token, platform FROM push_token WHERE user_id IN ({placeholders})");
        let params: Vec<libsql::Value> = chunk.iter().map(|u| u.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push((row.get(0)?, row.get(1)?));
        }
    }
    Ok(out)
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
}
