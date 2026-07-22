//! Push-notification backend (#344) — **always compiled, all targets**.
//!
//! Two halves:
//!   - `register_push_token` — a mobile client upserts its Expo push token
//!     (one row per device install) into the Turso `push_token` table.
//!   - `notify_new_message` — called from `send_message`'s background fanout
//!     to wake recipients' backgrounded/closed apps with a **content-free**
//!     notification.
//!
//! Privacy: a push carries ONLY `{ conversationId, kind }` — enough for the
//! client to route and re-ingest the (still-encrypted) message locally, never
//! the plaintext, sender, or any content. APNs/FCM/Expo therefore learn no
//! more than Turso already does (a conversation had activity). Foreground
//! delivery uses the LiveKit realtime path instead; this is strictly the
//! background/closed path.
//!
//! Desktop never registers a token, so its users have no rows and the fanout
//! is a cheap no-op — but desktop still RUNS the fanout, which is what lets a
//! message sent from desktop wake a recipient's phone.

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// Expo's push service endpoint. Accepts a JSON array of up to 100 messages.
const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";

/// Upsert a device's Expo push token. Keyed on the token (unique per device
/// install) so re-registering from the same device — e.g. after switching
/// accounts — reassigns ownership rather than creating a duplicate row.
pub async fn register_push_token(
    user_id: String,
    token: String,
    platform: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    // DS seam: route the owner-scoped upsert through the Delivery Service (the
    // write API).
    let body = serde_json::json!({
        "token": token,
        "platform": platform,
        "updated_at": now,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/push-tokens", &body).await?;
    Ok(())
}

/// Deliver a content-free push to every other member of a conversation. Best
/// effort: resolves recipients → their tokens → one batched Expo POST. Returns
/// `Ok(())` (after logging) on any relay failure; callers spawn this so it
/// never blocks or fails the send.
pub async fn notify_new_message(
    conversation_id: &str,
    mls_group_id: &str,
    is_channel: bool,
    sender_id: &str,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Recipients = conversation members other than the sender. For a channel
    // the membership lives on the group; for a DM, on the dm channel.
    let (member_sql, member_key) = if is_channel {
        (
            "SELECT user_id FROM group_member WHERE group_id = ?1 AND user_id <> ?2",
            mls_group_id,
        )
    } else {
        (
            "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id <> ?2",
            conversation_id,
        )
    };
    let mut rows = conn
        .query(
            member_sql,
            libsql::params![member_key.to_string(), sender_id.to_string()],
        )
        .await?;
    let mut user_ids: Vec<String> = Vec::new();
    while let Some(row) = rows.next().await? {
        user_ids.push(row.get::<String>(0)?);
    }
    if user_ids.is_empty() {
        return Ok(());
    }

    // Fetch every registered token for those recipients in one query.
    let placeholders = (0..user_ids.len())
        .map(|i| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(",");
    let token_sql = format!(
        "SELECT token, platform FROM push_token WHERE user_id IN ({placeholders})"
    );
    let token_params: Vec<libsql::Value> = user_ids
        .iter()
        .map(|u| libsql::Value::Text(u.clone()))
        .collect();
    let mut token_rows = conn.query(&token_sql, token_params).await?;

    // `kind` mirrors the values the mobile push router understands
    // (mobile/hooks/usePushNotifications.ts): "channel" | "dm".
    let kind = if is_channel { "channel" } else { "dm" };

    let mut messages: Vec<serde_json::Value> = Vec::new();
    while let Some(row) = token_rows.next().await? {
        let token: String = row.get(0)?;
        let platform: String = row.get(1).unwrap_or_default();
        // Generic, content-free alert — the data fields drive routing + a
        // local re-ingest; the body intentionally reveals nothing. This is
        // the same approach Signal/WhatsApp use when the server can't decrypt
        // ("New message" until the app pulls + decrypts locally).
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
        if platform == "android" {
            msg["channelId"] = serde_json::Value::String("default".into());
        }
        messages.push(msg);
    }
    if messages.is_empty() {
        return Ok(());
    }

    // Direct, NOT through the overlay: Expo (`exp.host`) is a non-first-party host
    // outside the closed allowlist, so a relay would refuse to forward it (§1.2,
    // §14.4). Longer term the DS should proxy push registration server-side so the
    // client stops talking to Expo directly at all.
    // Expo accepts up to 100 messages per request; chunk to stay under it.
    let client = reqwest::Client::new();
    for chunk in messages.chunks(100) {
        let resp = client.post(EXPO_PUSH_URL).json(chunk).send().await;
        match resp {
            Ok(r) if !r.status().is_success() => {
                let status = r.status();
                let body = r.text().await.unwrap_or_default();
                eprintln!("[push] expo push non-success {status}: {body}");
            }
            Err(e) => eprintln!("[push] expo push send failed: {e}"),
            _ => {}
        }
    }

    Ok(())
}
