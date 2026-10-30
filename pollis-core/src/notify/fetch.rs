//! The headless fetch behind `pollis_notify_fetch` (#1599).
//!
//! Opens the signed-in session from the data dir without realtime, LiveKit or the media server,
//! holds the cross-process [`StateLock`] for the whole run, then drives the SAME ingest the app
//! runs (`ingest_dm_envelopes_inner` / `ingest_channel_envelopes_inner`), so envelopes are
//! fetched, decrypted, persisted and acknowledged exactly as the app would. Only then does it
//! read back what to show.


use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use super::escrow::{self, NotificationContent};
use super::lock::{LockError, LockPolicy, SharedStateLock, StateLock};
use super::preview;
use crate::config::Config;
use crate::db::local::LocalDb;
use crate::keystore::Keystore;
use crate::state::AppState;

/// How recent a message must be to stand in for a push whose message was already stored by
/// someone else (the app, or a fetch for a sibling channel). See [`select_rows`].
const FALLBACK_SECS: i64 = 600;

/// `ui_state` key: the last message offered by that fallback, so it is offered once.
const FALLBACK_SHOWN_KEY: &str = "notify_fallback_shown";

/// A message a notification may preview: someone else's, real, decrypted, not deleted.
const LIVE_INBOUND: &str = "sender_id != ?1 AND content IS NOT NULL AND undecryptable IS NULL \
    AND deleted_at IS NULL AND send_state IS NULL";

/// Most previews one fetch returns.
const MAX_ITEMS: i64 = 5;

/// Longest the fetch waits for the app to release the lock, as a share of the budget.
const LOCK_SHARE: f64 = 0.4;

/// The per-user keystore slot `commands::auth` keeps the device id in.
const DEVICE_ID_SLOT: &str = "device_id";

/// What the host asked for.
pub struct Params {
    pub keystore: Arc<dyn Keystore>,
    pub config: Config,
    pub handle: Option<String>,
    pub budget: Duration,
    /// The test harness's per-device directory; production uses `dirs_path()`.
    #[cfg(any(test, feature = "test-harness"))]
    pub local_dir: Option<std::path::PathBuf>,
}

/// One notification's worth. Field names are the C ABI contract.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Item {
    pub conversation_id: String,
    pub message_id: String,
    pub sender_name: String,
    pub conversation_title: Option<String>,
    pub is_dm: bool,
    pub body: String,
    pub body_key: Option<&'static str>,
    pub muted: bool,
    /// A channel message that @mentions this user by name (additive to the #1599 contract).
    pub mention: bool,
}

/// Why there is nothing to show; the host falls back to the generic text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    NotLoggedIn,
    LockBusy,
    Timeout,
    Network,
    Decrypt,
    Internal,
}

/// The whole answer, serialized as the contract's JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok {
        mode: NotificationContent,
        items: Vec<Item>,
    },
    Failed(Reason),
}

impl Outcome {
    pub fn to_json(&self) -> String {
        let v = match self {
            Outcome::Ok { mode, items } => serde_json::json!({
                "ok": true,
                "mode": mode.as_str(),
                "items": items,
            }),
            Outcome::Failed(reason) => serde_json::json!({ "ok": false, "reason": reason }),
        };
        v.to_string()
    }
}

/// The fetch for a push that arrives while the app itself runs in this process and is unlocked
/// (Android's FCM service lives in the app's process, #1610). `None` when it is not: the caller
/// then runs the headless [`run`].
///
/// It works on the app's own state: its open database, its MLS locks and its share of the
/// cross-process lock, the same as any of the app's ingests. The headless path would take the
/// EXCLUSIVE `state.lock` for its whole run instead, and the app's own ingest of the same
/// message, started by the realtime wake-up that arrives with the push, waited behind it for
/// seconds. Nothing is opened or closed here, and no escrowed key is read.
pub async fn run_in_app(app: &Arc<AppState>, handle: Option<&str>, budget: Duration) -> Option<Outcome> {
    let started = Instant::now();
    let deadline = started + budget;
    // The ingest stops short of the deadline so what it stored can still be read back.
    let ingest_deadline = started + budget.mul_f64(IN_APP_INGEST_SHARE);
    // Everything, the app's own locks included, runs inside the budget: a host thread waiting
    // on this must never wait longer than it asked to.
    let body = async {
        let user_id = app.unlock.lock().await.as_ref()?.user_id.clone();
        let (mode, floor) = {
            let guard = app.local_db.lock().await;
            let db = guard.as_ref()?;
            let floor: i64 = db
                .conn()
                .query_row("SELECT COALESCE(MAX(rowid), 0) FROM message", [], |r| r.get(0))
                .unwrap_or(0);
            (escrow::read_mode(db.conn()), floor)
        };
        // "Nothing" means nothing: no fetch, no decrypt, no name. The app's realtime path stores it.
        if mode == NotificationContent::None {
            return Some(Outcome::Ok {
                mode,
                items: Vec::new(),
            });
        }
        let result = tokio::time::timeout_at(
            ingest_deadline.into(),
            ingest_for_handle(app, &user_id, handle),
        )
        .await;
        Some(match result {
            Ok(Ok(target)) => finish(app, &user_id, mode, floor, target, deadline, None).await,
            Ok(Err(reason)) => {
                crate::diag!("[notify] in-app ingest failed: {reason:?}");
                finish(app, &user_id, mode, floor, None, deadline, Some(reason)).await
            }
            Err(_) => {
                finish(app, &user_id, mode, floor, None, deadline, Some(Reason::Timeout)).await
            }
        })
    };
    tokio::time::timeout_at(deadline.into(), body)
        .await
        .unwrap_or(Some(Outcome::Failed(Reason::Timeout)))
}

/// The in-app fetch's share of its budget for the ingest; the rest is for reading back.
const IN_APP_INGEST_SHARE: f64 = 0.8;

/// Run one fetch. Never panics on its own account; every failure is a [`Reason`].
pub async fn run(p: Params) -> Outcome {
    let started = Instant::now();

    // 1. Who is signed in, and is there a key this process may use?
    let Some(user_id) = crate::accounts::read_accounts_index()
        .ok()
        .and_then(|i| i.last_active_user)
        .filter(|u| !u.is_empty())
    else {
        return Outcome::Failed(Reason::NotLoggedIn);
    };
    let device_id = match p.keystore.load_for_user(DEVICE_ID_SLOT, &user_id).await {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(id) => id,
            Err(_) => return Outcome::Failed(Reason::Internal),
        },
        Ok(None) => return Outcome::Failed(Reason::NotLoggedIn),
        Err(e) => {
            crate::diag!("[notify] keystore unreadable: {e}");
            return Outcome::Failed(Reason::Internal);
        }
    };
    let db_key = match escrow::load_escrow(p.keystore.as_ref(), &user_id).await {
        Ok(Some(k)) => k,
        // No escrow: the setting is "Nothing", or this build has not been unlocked yet. Either
        // way there is nothing this process may decrypt; the host shows the generic text.
        Ok(None) => {
            return Outcome::Ok {
                mode: NotificationContent::None,
                items: Vec::new(),
            }
        }
        Err(e) => {
            crate::diag!("[notify] escrow unreadable: {e}");
            return Outcome::Failed(Reason::Internal);
        }
    };

    // 2. A state that never starts realtime, LiveKit or the media server.

    let mut state = AppState::new_with_parts(p.config, p.keystore.clone());
    state.state_lock = SharedStateLock::new(LockPolicy::HeldByCaller);
    #[cfg(any(test, feature = "test-harness"))]
    if let Some(dir) = p.local_dir {
        let _ = state.local_dir.set(dir);
    }
    let state = Arc::new(state);
    *state.device_id.lock().await = Some(device_id);
    let outcome = run_locked(&state, &user_id, &db_key, p.handle.as_deref(), started, p.budget).await;

    // Close the database before returning (#1599): an extension or FCM process must not keep the
    // App Group file (and its WAL/SHM locks) open after its work is done.
    let db = state.local_db.lock().await.take();
    drop(db);
    outcome
}

/// Steps 3 to 6 of [`run`], on the headless state.
async fn run_locked(
    state: &Arc<AppState>,
    user_id: &str,
    db_key: &[u8],
    handle: Option<&str>,
    started: Instant,
    budget: Duration,
) -> Outcome {
    let deadline = started + budget;
    let data_dir = state.local_data_dir();

    // 3. The cross-process lock, for the ingest only.
    let lock_deadline = started + budget.mul_f64(LOCK_SHARE);
    let lock = match StateLock::try_acquire_until_in(&data_dir, lock_deadline).await {
        Ok(l) => l,
        Err(LockError::Busy) => return Outcome::Failed(Reason::LockBusy),
        Err(e) => {
            crate::diag!("[notify] state lock: {e}");
            return Outcome::Failed(Reason::Internal);
        }
    };

    // 4. The app's database, opened without any of the app's open-time housekeeping.
    let db = match LocalDb::open_existing_for_user_in(&data_dir, user_id, db_key) {
        Ok(db) => db,
        Err(crate::error::Error::Database(rusqlite::Error::SqliteFailure(f, _)))
            if f.code == rusqlite::ErrorCode::NotADatabase =>
        {
            // A stale escrow. Never wipe; the app re-escrows on its next unlock.
            return Outcome::Failed(Reason::Decrypt);
        }
        Err(e) => {
            crate::diag!("[notify] local database would not open: {e}");
            return Outcome::Failed(Reason::NotLoggedIn);
        }
    };

    // "Nothing" means nothing: no fetch, no decrypt, no name. An escrow that outlived the
    // setting (a failed delete) is dropped here too.
    let mode = escrow::read_mode(db.conn());
    if mode == NotificationContent::None {
        drop(db);
        drop(lock);
        if let Err(e) = escrow::delete_escrow(state.keystore.as_ref(), user_id).await {
            crate::diag!("[notify] stale escrow not deleted: {e}");
        }
        return Outcome::Ok {
            mode,
            items: Vec::new(),
        };
    }
    let floor: i64 = db
        .conn()
        .query_row("SELECT COALESCE(MAX(rowid), 0) FROM message", [], |r| r.get(0))
        .unwrap_or(0);
    *state.local_db.lock().await = Some(db);

    // 5. Fetch, decrypt and persist through the app's own path.
    let ingest = ingest_for_handle(state, user_id, handle);
    let result = tokio::time::timeout_at(deadline.into(), ingest).await;

    // 6. The state is written; let the app have the lock back before any more round trips
    // (names, mutes), so a foreground app's own ingest is never stalled behind previews.
    drop(lock);

    match result {
        Ok(Ok(target)) => finish(state, user_id, mode, floor, target, deadline, None).await,
        Ok(Err(reason)) => {
            // Whatever did persist is still shown; only an empty result fails.
            crate::diag!("[notify] ingest failed: {reason:?}");
            finish(state, user_id, mode, floor, None, deadline, Some(reason)).await
        }
        Err(_) => finish(state, user_id, mode, floor, None, deadline, Some(Reason::Timeout)).await,
    }
}

/// The conversation a resolved handle names.
#[derive(Debug, Clone)]
struct Target {
    conversation_id: String,
    is_dm: bool,
}

async fn ingest_for_handle(
    state: &Arc<AppState>,
    user_id: &str,
    handle: Option<&str>,
) -> Result<Option<Target>, Reason> {
    let Some(handle) = handle.filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    let resolved = crate::commands::push::resolve_push_handle(handle.to_string(), state)
        .await
        .map_err(classify)?;
    let Some((conversation_id, kind)) = resolved else {
        // Unknown or expired: nothing to fetch for it.
        return Ok(None);
    };
    let is_dm = match kind.as_str() {
        "dm" => true,
        "channel" => false,
        _ => return Ok(None),
    };
    let result = if is_dm {
        crate::commands::messages::ingest_dm_envelopes_inner(state, user_id, &conversation_id).await
    } else {
        crate::commands::messages::ingest_channel_envelopes_inner(state, user_id, &conversation_id)
            .await
    };
    result.map_err(classify)?;
    Ok(Some(Target {
        conversation_id,
        is_dm,
    }))
}

fn classify(e: crate::error::Error) -> Reason {
    match e {
        crate::error::Error::Network(_) => Reason::Network,
        crate::error::Error::Database(_) => Reason::Internal,
        other => {
            let s = other.to_string();
            if s.contains("error sending request") || s.contains("connect") || s.contains("timed out") {
                Reason::Network
            } else {
                crate::diag!("[notify] fetch error: {s}");
                Reason::Internal
            }
        }
    }
}

/// One stored message worth previewing.
struct Row {
    id: String,
    conversation_id: String,
    sender_id: String,
    content: String,
}

/// Read back what to show and build the answer. A failed or timed-out ingest still shows
/// whatever it managed to persist; with nothing to show it reports `failure`.
async fn finish(
    state: &Arc<AppState>,
    user_id: &str,
    mode: NotificationContent,
    floor: i64,
    target: Option<Target>,
    deadline: Instant,
    failure: Option<Reason>,
) -> Outcome {
    let rows = match select_rows(state, user_id, floor, target.as_ref()).await {
        Ok(rows) => rows,
        Err(e) => {
            crate::diag!("[notify] reading previews failed: {e}");
            return Outcome::Failed(failure.unwrap_or(Reason::Internal));
        }
    };
    if rows.is_empty() {
        if let Some(reason) = failure {
            return Outcome::Failed(reason);
        }
        return Outcome::Ok {
            mode,
            items: Vec::new(),
        };
    }

    let names = sender_names(state, &rows, deadline).await;
    let my_name = crate::accounts::read_accounts_index()
        .ok()
        .and_then(|i| i.accounts.into_iter().find(|a| a.user_id == user_id))
        .map(|a| a.username)
        .unwrap_or_default();
    let mutes = channel_mutes(state, user_id, &rows, target.as_ref(), deadline).await;

    let mut items = Vec::with_capacity(rows.len());
    for row in rows {
        let (is_dm, title) = conversation_meta(state, &row.conversation_id, target.as_ref()).await;
        let (body, key) = preview::preview(&row.content);
        let (body, body_key) = match mode {
            NotificationContent::Full => (body, key.map(|k| k.as_str())),
            NotificationContent::Name | NotificationContent::None => (String::new(), None),
        };
        // A channel mute is overridden by a direct @username mention, exactly as the DS decides
        // whom to wake (`push_recipients` keeps a muted member only if `audience.mentioned`
        // names them, and that set is `send::direct_mentions`; `@all` does not pierce a mute).
        let mention = !is_dm
            && !crate::commands::messages::direct_mentions(
                &row.content,
                &[(user_id.to_string(), my_name.clone())],
            )
            .is_empty();
        let muted = !is_dm && !mention && mutes.contains(&row.conversation_id);
        items.push(Item {
            conversation_id: row.conversation_id,
            message_id: row.id,
            sender_name: names
                .iter()
                .find(|(id, _)| *id == row.sender_id)
                .map(|(_, n)| n.clone())
                .unwrap_or_default(),
            conversation_title: title,
            is_dm,
            body,
            body_key,
            muted,
            mention,
        });
    }
    Outcome::Ok { mode, items }
}

/// What to preview: the inbound, real, live messages THIS call stored (above `floor`), oldest
/// first, at most [`MAX_ITEMS`]. Each message is therefore returned by at most one fetch.
///
/// If this call stored nothing for the handle's conversation (the app, or a fetch for a sibling
/// channel, already had it), the newest inbound message there from the last
/// [`FALLBACK_SECS`] is returned instead, at most once ever: its id is remembered in
/// `ui_state` and never offered again. Hosts may also dedupe by `message_id`.
async fn select_rows(
    state: &Arc<AppState>,
    user_id: &str,
    floor: i64,
    target: Option<&Target>,
) -> crate::error::Result<Vec<Row>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("no database")))?;
    let conn = db.conn();
    let conv = target.map(|t| t.conversation_id.as_str());
    let read = |sql: &str, params: &[&dyn rusqlite::ToSql]| -> crate::error::Result<Vec<Row>> {
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt
            .query_map(params, |r| {
                Ok(Row {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    sender_id: r.get(2)?,
                    content: r.get(3)?,
                })
            })?
            .filter_map(|r| r.ok())
            .collect();
        Ok(rows)
    };
    let mut rows = read(
        &format!(
            "SELECT id, conversation_id, sender_id, content FROM message
             WHERE {LIVE_INBOUND}
               AND (?2 IS NULL OR conversation_id = ?2)
               AND rowid > ?3
             ORDER BY rowid DESC LIMIT ?4"
        ),
        &[&user_id, &conv, &floor, &MAX_ITEMS],
    )?;
    rows.reverse();
    let remember = |id: &str| {
        let _ = conn.execute(
            "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
            rusqlite::params![FALLBACK_SHOWN_KEY, id],
        );
    };
    if let Some(last) = rows.last() {
        // So the fallback below never offers this one again.
        remember(&last.id);
        return Ok(rows);
    }
    if conv.is_none() {
        return Ok(rows);
    }

    let window = format!("-{FALLBACK_SECS} seconds");
    let latest = read(
        &format!(
            "SELECT id, conversation_id, sender_id, content FROM message
             WHERE {LIVE_INBOUND}
               AND conversation_id = ?2
               AND received_at >= datetime('now', ?3)
             ORDER BY rowid DESC LIMIT 1"
        ),
        &[&user_id, &conv, &window],
    )?;
    let Some(row) = latest.into_iter().next() else {
        return Ok(Vec::new());
    };
    let shown: Option<String> = conn
        .query_row(
            "SELECT value FROM ui_state WHERE key = ?1",
            rusqlite::params![FALLBACK_SHOWN_KEY],
            |r| r.get(0),
        )
        .ok();
    if shown.as_deref() == Some(row.id.as_str()) {
        return Ok(Vec::new());
    }
    remember(&row.id);
    Ok(vec![row])
}

/// `(sender_id, username)`: the local cache first, then one DS lookup for the misses
/// (best-effort, cached for next time).
async fn sender_names(
    state: &Arc<AppState>,
    rows: &[Row],
    deadline: Instant,
) -> Vec<(String, String)> {
    let mut wanted: Vec<String> = Vec::new();
    for r in rows {
        if !wanted.contains(&r.sender_id) {
            wanted.push(r.sender_id.clone());
        }
    }
    let mut found: Vec<(String, String)> = Vec::new();
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            for id in &wanted {
                if let Ok(name) = db.conn().query_row(
                    "SELECT username FROM user_cache WHERE id = ?1",
                    rusqlite::params![id],
                    |r| r.get::<_, String>(0),
                ) {
                    found.push((id.clone(), name));
                }
            }
        }
    }
    let missing: Vec<String> = wanted
        .into_iter()
        .filter(|id| !found.iter().any(|(f, _)| f == id))
        .collect();
    if missing.is_empty() {
        return found;
    }
    let lookup = crate::commands::ds_reads::users(state, missing, None);
    if let Ok(Ok(users)) = tokio::time::timeout_at(deadline.into(), lookup).await {
        let guard = state.local_db.lock().await;
        for u in users {
            if let Some(db) = guard.as_ref() {
                let _ = db.conn().execute(
                    "INSERT INTO user_cache (id, username, updated_at) VALUES (?1, ?2, datetime('now'))
                     ON CONFLICT(id) DO UPDATE SET username = ?2, updated_at = datetime('now')",
                    rusqlite::params![u.id, u.username],
                );
            }
            found.push((u.id, u.username));
        }
    }
    found
}

/// The channels this user muted, asked only when a channel message is in the answer. A failed
/// read counts as nothing muted: the DS already skipped muted recipients when it sent the push.
async fn channel_mutes(
    state: &Arc<AppState>,
    user_id: &str,
    rows: &[Row],
    target: Option<&Target>,
    deadline: Instant,
) -> Vec<String> {
    let any_channel = match target {
        Some(t) if rows.iter().all(|r| r.conversation_id == t.conversation_id) => !t.is_dm,
        _ => true,
    };
    if !any_channel {
        return Vec::new();
    }
    let read = crate::commands::ds_reads::channel_mutes(state, user_id);
    match tokio::time::timeout_at(deadline.into(), read).await {
        Ok(Ok(resp)) => resp.channel_ids,
        _ => Vec::new(),
    }
}

/// `(is_dm, title)` for a conversation: the resolved handle's kind when it is that one, else
/// what the local caches know. A DM has no title (the sender is the title); a channel is
/// `#name`, prefixed with its group when known.
async fn conversation_meta(
    state: &Arc<AppState>,
    conversation_id: &str,
    target: Option<&Target>,
) -> (bool, Option<String>) {
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return (target.is_some_and(|t| t.is_dm), None);
    };
    let cached: Option<(String, Option<String>, Option<String>)> = db
        .conn()
        .query_row(
            "SELECT kind, name, group_name FROM conversation_cache WHERE id = ?1",
            rusqlite::params![conversation_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok();
    let is_dm = match target {
        Some(t) if t.conversation_id == conversation_id => t.is_dm,
        _ => match &cached {
            Some((kind, _, _)) => kind == "dm",
            None => db
                .conn()
                .query_row(
                    "SELECT 1 FROM dm_conversation WHERE id = ?1",
                    rusqlite::params![conversation_id],
                    |_| Ok(()),
                )
                .is_ok(),
        },
    };
    if is_dm {
        return (true, None);
    }
    let title = cached.and_then(|(_, name, group)| {
        let name = name.filter(|n| !n.is_empty())?;
        Some(match group.filter(|g| !g.is_empty()) {
            Some(g) => format!("{g} › #{name}"),
            None => format!("#{name}"),
        })
    });
    (false, title)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcomes_serialize_to_the_contract() {
        let ok = Outcome::Ok {
            mode: NotificationContent::Full,
            items: vec![Item {
                conversation_id: "c".into(),
                message_id: "m".into(),
                sender_name: "alice".into(),
                conversation_title: None,
                is_dm: true,
                body: "hi".into(),
                body_key: None,
                muted: false,
                mention: false,
            }],
        };
        let v: serde_json::Value = serde_json::from_str(&ok.to_json()).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["mode"], "full");
        assert_eq!(v["items"][0]["sender_name"], "alice");
        assert!(v["items"][0]["conversation_title"].is_null());
        assert!(v["items"][0]["body_key"].is_null());
        for (reason, s) in [
            (Reason::NotLoggedIn, "not_logged_in"),
            (Reason::LockBusy, "lock_busy"),
            (Reason::Timeout, "timeout"),
            (Reason::Network, "network"),
            (Reason::Decrypt, "decrypt"),
            (Reason::Internal, "internal"),
        ] {
            let v: serde_json::Value =
                serde_json::from_str(&Outcome::Failed(reason).to_json()).unwrap();
            assert_eq!(v, serde_json::json!({"ok": false, "reason": s}));
        }
    }
}
