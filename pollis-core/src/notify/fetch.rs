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

/// The DS keeps a push handle for days, and a message the app ingested a moment ago should
/// still be shown for its own push: an inbound message stored this recently counts as "new".
const RECENT_SECS: i64 = 120;

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
    #[cfg(feature = "test-harness")]
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

/// Run one fetch. Never panics on its own account; every failure is a [`Reason`].
pub async fn run(p: Params) -> Outcome {
    let started = Instant::now();
    let deadline = started + p.budget;

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
    #[cfg(feature = "test-harness")]
    if let Some(dir) = p.local_dir {
        let _ = state.local_dir.set(dir);
    }
    let state = Arc::new(state);
    *state.device_id.lock().await = Some(device_id);
    let data_dir = state.local_data_dir();

    // 3. The cross-process lock, for the whole run.
    let lock_deadline = started + p.budget.mul_f64(LOCK_SHARE);
    let _lock = match StateLock::try_acquire_until_in(&data_dir, lock_deadline).await {
        Ok(l) => l,
        Err(LockError::Busy) => return Outcome::Failed(Reason::LockBusy),
        Err(e) => {
            crate::diag!("[notify] state lock: {e}");
            return Outcome::Failed(Reason::Internal);
        }
    };

    // 4. The app's database, opened without any of the app's open-time housekeeping.
    let db = match LocalDb::open_existing_for_user_in(&data_dir, &user_id, &db_key) {
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
    let mode = escrow::read_mode(db.conn());
    let floor: i64 = db
        .conn()
        .query_row("SELECT COALESCE(MAX(rowid), 0) FROM message", [], |r| r.get(0))
        .unwrap_or(0);
    *state.local_db.lock().await = Some(db);

    // 5. Fetch, decrypt and persist through the app's own path.
    let ingest = ingest_for_handle(&state, &user_id, p.handle.as_deref());
    let target = match tokio::time::timeout_at(deadline.into(), ingest).await {
        Ok(Ok(target)) => Some(target),
        Ok(Err(reason)) => {
            // Whatever did persist is still shown below; only an empty result fails.
            crate::diag!("[notify] ingest failed: {reason:?}");
            return finish(&state, &user_id, mode, floor, None, deadline, Some(reason)).await;
        }
        Err(_) => {
            return finish(&state, &user_id, mode, floor, None, deadline, Some(Reason::Timeout))
                .await;
        }
    };
    finish(&state, &user_id, mode, floor, target.flatten(), deadline, None).await
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
        // A channel mute is overridden by a direct @mention, exactly as the DS decides whom to wake.
        let muted = !is_dm
            && mutes.contains(&row.conversation_id)
            && crate::commands::messages::direct_mentions(
                &row.content,
                &[(user_id.to_string(), my_name.clone())],
            )
            .is_empty();
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
        });
    }
    Outcome::Ok { mode, items }
}

/// Inbound, real, live messages: stored by this run, or (for the handle's conversation) stored
/// in the last [`RECENT_SECS`] by anyone, so a push whose message the app or a sibling fetch
/// already ingested still shows it. Oldest first, at most [`MAX_ITEMS`].
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
    let conv = target.map(|t| t.conversation_id.as_str());
    let mut stmt = db.conn().prepare(
        "SELECT id, conversation_id, sender_id, content FROM message
         WHERE sender_id != ?1
           AND content IS NOT NULL AND undecryptable IS NULL
           AND deleted_at IS NULL AND send_state IS NULL
           AND (?2 IS NULL OR conversation_id = ?2)
           AND (rowid > ?3
                OR (?2 IS NOT NULL AND received_at >= datetime('now', ?4)))
         ORDER BY rowid DESC LIMIT ?5",
    )?;
    let window = format!("-{RECENT_SECS} seconds");
    let mut rows: Vec<Row> = stmt
        .query_map(
            rusqlite::params![user_id, conv, floor, window, MAX_ITEMS],
            |r| {
                Ok(Row {
                    id: r.get(0)?,
                    conversation_id: r.get(1)?,
                    sender_id: r.get(2)?,
                    content: r.get(3)?,
                })
            },
        )?
        .filter_map(|r| r.ok())
        .collect();
    rows.reverse();
    Ok(rows)
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
