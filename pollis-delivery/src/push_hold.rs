//! No phone push while the user is at a desktop (#2056).
//!
//! A desktop reports itself active (focused, visible, not idle) and inactive
//! through `POST /v1/push/activity`. While any device of a user is active, a
//! message push for that user is HELD in `push_held` (000056) instead of sent:
//! one row per conversation, the newest message. Then:
//!
//!   * a read of the conversation drops the row ([`drop_read`], from the dismiss path);
//!   * the user going inactive, or the last active report lapsing after
//!     [`ACTIVITY_TTL_SECS`], sends one push per row still held ([`release`]);
//!   * boot sends everything held ([`release_all`]): activity lives only in
//!     memory, so after a restart nobody is active.
//!
//! The Slack model: the desktop alerts while you are at it, your phone is told
//! only what you did not see there.
//!
//! Activity is kept in memory only, keyed by user and device, never written
//! down or logged. Every failure falls back to sending the push: a held push
//! that cannot be stored is pushed at once, never dropped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use libsql::Connection;
use pollis_api::devices::PushActivityBody;
pub use pollis_api::devices::ACTIVITY_TTL_SECS;

use crate::error::AppError;
use crate::push::{PushMessage, ReadCutoff};
use crate::writes::RawRequest;
use crate::AppState;

/// Soft cap on tracked users; only bounds memory in a pathological case.
const MAX_USERS: usize = 50_000;

/// Most devices tracked per user.
const MAX_DEVICES: usize = 16;

/// `user -> device -> active until`.
type Registry = HashMap<String, HashMap<String, Instant>>;

fn registry() -> &'static Mutex<Registry> {
    static REG: std::sync::OnceLock<Mutex<Registry>> = std::sync::OnceLock::new();
    REG.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record one device's report. Returns whether the user is active afterwards.
pub fn report(user_id: &str, device_id: &str, active: bool, ttl: Duration) -> bool {
    let now = Instant::now();
    let mut reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    if active && !reg.contains_key(user_id) && reg.len() >= MAX_USERS {
        reg.retain(|_, devices| {
            devices.retain(|_, until| *until > now);
            !devices.is_empty()
        });
        if reg.len() >= MAX_USERS {
            // Full: this user simply is not held, which only means pushes go out.
            return false;
        }
    }
    let devices = reg.entry(user_id.to_string()).or_default();
    devices.retain(|_, until| *until > now);
    if active {
        if devices.contains_key(device_id) || devices.len() < MAX_DEVICES {
            devices.insert(device_id.to_string(), now + ttl);
        }
    } else {
        devices.remove(device_id);
    }
    let still = !devices.is_empty();
    if !still {
        reg.remove(user_id);
    }
    still
}

/// Is any device of `user_id` active now?
pub fn is_active(user_id: &str) -> bool {
    let now = Instant::now();
    let reg = registry().lock().unwrap_or_else(|e| e.into_inner());
    reg.get(user_id)
        .is_some_and(|devices| devices.values().any(|until| *until > now))
}

/// Hold the push for every active user in `user_ids`; returns the users to push
/// now. A user whose hold could not be stored is pushed, never dropped.
///
/// Only a push with a message is held: without one, a read could not clear it
/// by position, and nothing on the current send path lacks one.
pub async fn hold_active(
    conn: &Connection,
    expo_url: &str,
    user_ids: &[String],
    conversation_id: &str,
    kind: &str,
    message: Option<&PushMessage>,
) -> Vec<String> {
    let Some(message) = message else {
        return user_ids.to_vec();
    };
    let mut now = Vec::with_capacity(user_ids.len());
    for user in user_ids {
        if !is_active(user) {
            now.push(user.clone());
            continue;
        }
        if !store_hold(conn, user, conversation_id, kind, message).await {
            now.push(user.clone());
            continue;
        }
        // The user may have gone inactive between the check and the write; the
        // release for that has already run, so run another.
        if !is_active(user) {
            release(conn, expo_url, user).await;
        }
    }
    now
}

/// Keep the newest message per (user, conversation). False when the write failed.
async fn store_hold(
    conn: &Connection,
    user_id: &str,
    conversation_id: &str,
    kind: &str,
    message: &PushMessage,
) -> bool {
    let wrote = conn
        .execute(
            "INSERT INTO push_held \
             (user_id, conversation_id, kind, message_id, message_sent_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
             ON CONFLICT(user_id, conversation_id) DO UPDATE SET \
               kind = excluded.kind, \
               message_id = excluded.message_id, \
               message_sent_at = excluded.message_sent_at, \
               created_at = excluded.created_at \
             WHERE (excluded.message_sent_at, excluded.message_id) \
                   > (push_held.message_sent_at, push_held.message_id)",
            libsql::params![
                user_id.to_string(),
                conversation_id.to_string(),
                kind.to_string(),
                message.id.clone(),
                message.sent_at.clone()
            ],
        )
        .await;
    match wrote {
        Ok(_) => true,
        Err(e) => {
            // No user or conversation id: push logs name no one (#1423).
            tracing::warn!("push hold write failed, pushing instead: {e}");
            false
        }
    }
}

/// Drop `user_id`'s held push for `conversation_id` if the read covers it.
pub async fn drop_read(conn: &Connection, user_id: &str, conversation_id: &str, cutoff: &ReadCutoff) {
    let (through_at, through_id) = cutoff.through_params();
    let dropped = conn
        .execute(
            "DELETE FROM push_held \
             WHERE user_id = ?1 AND conversation_id = ?2 \
               AND (created_at <= datetime(?3, 'unixepoch') \
                    OR (?4 IS NOT NULL AND (message_sent_at, message_id) <= (?4, ?5)))",
            libsql::params![
                user_id.to_string(),
                conversation_id.to_string(),
                cutoff.at_unix,
                through_at,
                through_id
            ],
        )
        .await;
    if let Err(e) = dropped {
        tracing::debug!("push hold drop skipped: {e}");
    }
}

/// The conversations held for `user_id`, as `(conversation, kind, message)`.
async fn held_for(conn: &Connection, user_id: &str) -> Vec<(String, String, PushMessage)> {
    let mut out = Vec::new();
    let rows = conn
        .query(
            "SELECT conversation_id, kind, message_id, message_sent_at FROM push_held \
             WHERE user_id = ?1 ORDER BY message_sent_at",
            libsql::params![user_id.to_string()],
        )
        .await;
    let mut rows = match rows {
        Ok(r) => r,
        Err(e) => {
            tracing::debug!("push hold read skipped: {e}");
            return out;
        }
    };
    while let Ok(Some(row)) = rows.next().await {
        let (Ok(c), Ok(k), Ok(id), Ok(at)) = (
            row.get::<String>(0),
            row.get::<String>(1),
            row.get::<String>(2),
            row.get::<String>(3),
        ) else {
            continue;
        };
        out.push((c, k, PushMessage { id, sent_at: at }));
    }
    out
}

/// Send `user_id` one push per conversation still held, unless they are active
/// again. Each row is claimed (deleted, only if it still names the same
/// message) before its push, so two releases never push it twice and a newer
/// hold that lands meanwhile stays held for the next release.
pub async fn release(conn: &Connection, expo_url: &str, user_id: &str) -> usize {
    if is_active(user_id) {
        return 0;
    }
    let mut sent = 0;
    for (conversation_id, kind, message) in held_for(conn, user_id).await {
        let claimed = conn
            .execute(
                "DELETE FROM push_held \
                 WHERE user_id = ?1 AND conversation_id = ?2 AND message_id = ?3",
                libsql::params![
                    user_id.to_string(),
                    conversation_id.clone(),
                    message.id.clone()
                ],
            )
            .await;
        if !matches!(claimed, Ok(n) if n > 0) {
            continue;
        }
        let users = [user_id.to_string()];
        match crate::push::fan_out(conn, expo_url, &conversation_id, &users, &kind, Some(&message))
            .await
        {
            Ok(()) => sent += 1,
            Err(e) => tracing::warn!("held push send failed: {e}"),
        }
    }
    sent
}

/// Release everything held, for every user: at boot, when nobody is active.
pub async fn release_all(conn: &Connection, expo_url: &str) -> usize {
    let mut users = Vec::new();
    match conn.query("SELECT DISTINCT user_id FROM push_held", ()).await {
        Ok(mut rows) => {
            while let Ok(Some(row)) = rows.next().await {
                if let Ok(u) = row.get::<String>(0) {
                    users.push(u);
                }
            }
        }
        Err(e) => {
            tracing::debug!("push hold boot release skipped: {e}");
            return 0;
        }
    }
    let mut sent = 0;
    for user in users {
        sent += release(conn, expo_url, &user).await;
    }
    sent
}

/// Spawn the boot release; see [`release_all`].
pub fn spawn_boot_release(db: std::sync::Arc<crate::db::Db>) {
    tokio::spawn(async move {
        let conn = match db.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("push hold boot release: conn failed: {e}");
                return;
            }
        };
        let n = release_all(&conn, crate::push::EXPO_PUSH_URL).await;
        if n > 0 {
            tracing::info!("push hold: released {n} held push(es) at boot");
        }
    });
}

/// After `delay`, release `user_id`'s holds if no device renewed in the meantime.
/// One-shot per report, not a poll.
pub fn schedule_lapse(db: std::sync::Arc<crate::db::Db>, expo_url: String, user_id: String, delay: Duration) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if is_active(&user_id) {
            return;
        }
        let conn = match db.conn().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("push hold lapse: conn failed: {e}");
                return;
            }
        };
        release(&conn, &expo_url, &user_id).await;
    });
}

/// Apply one device's report: record it, then either schedule the lapse check
/// (active) or release at once (the user's last active device went inactive).
pub async fn apply_report(
    db: std::sync::Arc<crate::db::Db>,
    expo_url: &str,
    user_id: &str,
    device_id: &str,
    active: bool,
    ttl: Duration,
) {
    let still = report(user_id, device_id, active, ttl);
    if active {
        // A hair past the TTL, so the check never lands just before the expiry.
        schedule_lapse(db, expo_url.to_string(), user_id.to_string(), ttl + Duration::from_millis(250));
        return;
    }
    if still {
        return;
    }
    match db.conn().await {
        Ok(conn) => {
            release(&conn, expo_url, user_id).await;
        }
        Err(e) => tracing::warn!("push hold release: conn failed: {e}"),
    }
}

/// POST `/v1/push/activity` — this device is, or is no longer, in use (#2056).
///
/// Device-signed and self-scoped: one device's report never touches another's.
/// With auth off (tests, local runs) the device is the body's user, one per user.
pub async fn push_activity(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: PushActivityBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(crate::writes::bad_request("invalid body")),
    };
    let (user_id, device_id) = if state.require_auth {
        match crate::writes::verify_signed_identity(&state, &req).await {
            Ok((user, device)) => {
                if !parsed.user_id.is_empty() && parsed.user_id != user {
                    return Ok(crate::error::AuthRejection::Forbidden.into_response());
                }
                (user, device)
            }
            Err(rej) => return Ok(rej.into_response()),
        }
    } else {
        if parsed.user_id.is_empty() {
            return Ok(crate::writes::bad_request("user_id required when auth is disabled"));
        }
        (parsed.user_id.clone(), parsed.user_id.clone())
    };
    apply_report(
        state.db.clone(),
        crate::push::EXPO_PUSH_URL,
        &user_id,
        &device_id,
        parsed.active,
        Duration::from_secs(ACTIVITY_TTL_SECS),
    )
    .await;
    Ok(crate::writes::ok_response::<PushActivityBody>(pollis_api::StatusOk::Ok))
}
