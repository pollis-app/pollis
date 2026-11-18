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
use super::scope::{self, Scope, TailSet};
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
/// Never a row copied from a sibling device (#1735): history, not news.
const LIVE_INBOUND: &str = "sender_id != ?1 AND content IS NOT NULL AND undecryptable IS NULL \
    AND deleted_at IS NULL AND send_state IS NULL AND origin IS NULL";

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
    /// What the item is (additive, #1734): [`KIND_MESSAGE`], or [`KIND_DM_REQUEST`] for a
    /// pending DM request, where `sender_name` is the requester's display name, `body` is
    /// empty and `message_id` is empty. Hosts show "Message request" and the name, never a
    /// preview, and a tap opens the request list.
    pub kind: &'static str,
}

/// [`Item::kind`] of a message.
pub const KIND_MESSAGE: &str = "message";
/// [`Item::kind`] of a pending DM request (#1734); also the push handle's kind.
pub const KIND_DM_REQUEST: &str = pollis_api::devices::PUSH_KIND_DM_REQUEST;

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
        // The app lives on in this process: its watermark and receipt posts run after the
        // answer, and the host's `settle` waits for them (#1804).
        let tail = Arc::new(TailSet::default());
        let result = tokio::time::timeout_at(
            ingest_deadline.into(),
            ingest_for_handle(app, &user_id, handle, Some(Arc::clone(&tail))),
        )
        .await;
        let pending = tail.take();
        if !pending.is_empty() {
            scope::finish_after(pending, async {});
        }
        Some(match result {
            Ok(Ok(target)) => finish(app, &user_id, mode, floor, target, deadline, None).await,
            Ok(Err(reason)) => {
                crate::diag!("[notify] in-app ingest failed: {reason:?}");
                finish(app, &user_id, mode, floor, None, deadline, Some(reason)).await
            }
            Err(_) => {
                crate::diag!("[notify] in-app ingest ran out of time");
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
    let (outcome, deferred) =
        run_locked(&state, &user_id, &db_key, p.handle.as_deref(), started, p.budget).await;

    // Close the database before returning (#1599): an extension or FCM process must not keep the
    // App Group file (and its WAL/SHM locks) open after its work is done. Work the ingest
    // deferred (#1804) still needs both, so then they close when it ends.
    match deferred {
        None => {
            let db = state.local_db.lock().await.take();
            drop(db);
        }
        Some((pending, lock)) => {
            let state = Arc::clone(&state);
            scope::finish_after(pending, async move {
                let db = state.local_db.lock().await.take();
                drop(db);
                drop(lock);
            });
        }
    }
    outcome
}

/// Whether the headless fetch may finish its watermark and receipt posts after it answers
/// (#1804). Not on iOS: the extension can be suspended as soon as it hands the notification
/// over, and a suspended process holding the App Group lock is killed (`0xdead10cc`). The
/// extension has about 30 s, so it keeps the posts inside the fetch, as before.
const DEFER_HEADLESS: bool = cfg!(not(target_os = "ios"));

/// The headless fetch's deferred posts, and the lock they run under.
type Deferred = Option<(Vec<tokio::task::JoinHandle<()>>, StateLock)>;

/// Steps 3 to 6 of [`run`], on the headless state.
async fn run_locked(
    state: &Arc<AppState>,
    user_id: &str,
    db_key: &[u8],
    handle: Option<&str>,
    started: Instant,
    budget: Duration,
) -> (Outcome, Deferred) {
    let deadline = started + budget;
    let data_dir = state.local_data_dir();

    // 3. The cross-process lock, for the ingest only.
    let lock_deadline = started + budget.mul_f64(LOCK_SHARE);
    let lock = match StateLock::try_acquire_until_in(&data_dir, lock_deadline).await {
        Ok(l) => l,
        Err(LockError::Busy) => return (Outcome::Failed(Reason::LockBusy), None),
        Err(e) => {
            crate::diag!("[notify] state lock: {e}");
            return (Outcome::Failed(Reason::Internal), None);
        }
    };

    // 4. The app's database, opened without any of the app's open-time housekeeping.
    let db = match LocalDb::open_existing_for_user_in(&data_dir, user_id, db_key) {
        Ok(db) => db,
        Err(crate::error::Error::Database(rusqlite::Error::SqliteFailure(f, _)))
            if f.code == rusqlite::ErrorCode::NotADatabase =>
        {
            // A stale escrow. Never wipe; the app re-escrows on its next unlock.
            return (Outcome::Failed(Reason::Decrypt), None);
        }
        Err(e) => {
            crate::diag!("[notify] local database would not open: {e}");
            return (Outcome::Failed(Reason::NotLoggedIn), None);
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
        return (
            Outcome::Ok {
                mode,
                items: Vec::new(),
            },
            None,
        );
    }
    let floor: i64 = db
        .conn()
        .query_row("SELECT COALESCE(MAX(rowid), 0) FROM message", [], |r| r.get(0))
        .unwrap_or(0);
    *state.local_db.lock().await = Some(db);

    // 5. Fetch, decrypt and persist through the app's own path.
    let tail = DEFER_HEADLESS.then(|| Arc::new(TailSet::default()));
    // A spent budget starts nothing: `timeout_at` polls the ingest before it looks at the
    // deadline, and on every wake after, so on a fast link an ingest begun late can still run
    // to the end. `None` is the timeout.
    let result = if Instant::now() < deadline {
        let ingest = ingest_for_handle(state, user_id, handle, tail.clone());
        tokio::time::timeout_at(deadline.into(), ingest).await.ok()
    } else {
        None
    };

    // 6. The state is written; let the app have the lock back before any more round trips
    // (names, mutes), so a foreground app's own ingest is never stalled behind previews.
    // Deferred posts (#1804) seal under it, so they keep it until they end.
    let pending = tail.map(|t| t.take()).unwrap_or_default();
    let deferred = if pending.is_empty() {
        drop(lock);
        None
    } else {
        Some((pending, lock))
    };

    let outcome = match result {
        Some(Ok(target)) => finish(state, user_id, mode, floor, target, deadline, None).await,
        Some(Err(reason)) => {
            // Whatever did persist is still shown; only an empty result fails.
            crate::diag!("[notify] ingest failed: {reason:?}");
            finish(state, user_id, mode, floor, None, deadline, Some(reason)).await
        }
        None => {
            crate::diag!("[notify] ingest ran out of time");
            finish(state, user_id, mode, floor, None, deadline, Some(Reason::Timeout)).await
        }
    };
    (outcome, deferred)
}

/// The conversation a resolved handle names.
#[derive(Clone)]
struct Target {
    conversation_id: String,
    is_dm: bool,
    /// A pending DM request (#1734): nothing is ingested or previewed for it.
    request: bool,
    /// The fetch's scope, so a DM request's lookups read the same prefetch (#1804).
    scope: Arc<Scope>,
}

/// First pause before retrying a fetch that failed on the network, doubled up to [`RETRY_MAX`].
const RETRY_FIRST: Duration = Duration::from_millis(250);
const RETRY_MAX: Duration = Duration::from_millis(1500);

/// [`ingest_once`], retried while it fails on the network. Android lets a backgrounded app's
/// sockets through only some time after a high-priority push wakes it; until then every lookup
/// fails with "No address associated with hostname" and pooled connections are aborted, so a
/// single attempt posted the generic text for a message the app then showed fine. The ingest is
/// idempotent, and the caller's deadline bounds the retries.
async fn ingest_for_handle(
    state: &Arc<AppState>,
    user_id: &str,
    handle: Option<&str>,
    tail: Option<Arc<TailSet>>,
) -> Result<Option<Target>, Reason> {
    retry_on_network(|| ingest_once(state, user_id, handle, tail.clone())).await
}

/// `POST /v1/push/fetch` (#1804): the handle resolve, Welcome read, catch-up and DM peer keys
/// in one round trip. `Ok(None)` when this Delivery Service does not answer it (it predates
/// the endpoint): the fetch then makes the separate requests, as before.
async fn push_fetch(
    state: &Arc<AppState>,
    user_id: &str,
    handle: &str,
) -> Result<Option<pollis_api::devices::PushFetchResponse>, Reason> {
    let body = pollis_api::devices::PushFetchBody {
        handle: handle.to_string(),
        user_id: Some(user_id.to_string()),
        device_id: state.device_id.lock().await.clone(),
        count_key_packages_in_suite: Some(crate::commands::mls::ds_reads::welcome_drain_suite()),
    };
    match crate::commands::mls::ds_client::ds_post_json(state, &body).await {
        Ok(resp) => Ok(Some(resp)),
        Err(e) => match classify(e) {
            Reason::Network => Err(Reason::Network),
            _ => Ok(None),
        },
    }
}

async fn retry_on_network<T, F, Fut>(mut attempt: F) -> Result<T, Reason>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, Reason>>,
{
    let mut pause = RETRY_FIRST;
    let mut retries = 0u32;
    loop {
        match attempt().await {
            Err(Reason::Network) => {
                retries += 1;
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(RETRY_MAX);
            }
            result => {
                if retries > 0 {
                    crate::diag!("[notify] network came back after {retries} retr(ies)");
                }
                return result;
            }
        }
    }
}

async fn ingest_once(
    state: &Arc<AppState>,
    user_id: &str,
    handle: Option<&str>,
    tail: Option<Arc<TailSet>>,
) -> Result<Option<Target>, Reason> {
    let Some(handle) = handle.filter(|h| !h.is_empty()) else {
        return Ok(None);
    };
    let fetched = push_fetch(state, user_id, handle).await?;
    let resolved = match fetched.as_ref() {
        Some(r) => r.conversation_id.clone().zip(r.kind.clone()),
        None => crate::commands::push::resolve_push_handle(handle.to_string(), state)
            .await
            .map_err(classify)?,
    };
    let Some((conversation_id, kind)) = resolved else {
        // Unknown or expired: nothing to fetch for it.
        return Ok(None);
    };
    let had_fetch = fetched.is_some();
    let scope = Scope::new(fetched, tail);
    if had_fetch && !scope.has_prefetch() {
        crate::diag!("[notify] a Welcome is pending; reading it fresh under the lock");
    }
    let is_dm = match kind.as_str() {
        "dm" => true,
        "channel" => false,
        // #1734: a request the user has not accepted. Nothing in it is fetched or
        // decrypted here: no preview until accepted, and the app ingests it then.
        KIND_DM_REQUEST => {
            return Ok(Some(Target {
                conversation_id,
                is_dm: true,
                request: true,
                scope,
            }))
        }
        _ => return Ok(None),
    };
    let ingest = async {
        if is_dm {
            crate::commands::messages::ingest_dm_envelopes_inner(state, user_id, &conversation_id)
                .await
        } else {
            crate::commands::messages::ingest_channel_envelopes_inner(state, user_id, &conversation_id)
                .await
        }
    };
    scope::run(Arc::clone(&scope), ingest).await.map_err(classify)?;
    Ok(Some(Target {
        conversation_id,
        is_dm,
        request: false,
        scope,
    }))
}

/// The one item a DM request shows (#1734): the requester's display name, no message.
///
/// The requester is whoever added this user to the DM (`added_by`), else its creator, read
/// from the DS as a pending member may; the name is their `preferred_name`, else their
/// username, which the request list already shows. `Ok(None)` when this user is no longer in
/// the DM; the host then shows the generic text, as on any failure. A network failure is
/// retried like the ingest's (#1672), inside the caller's deadline.
async fn request_item(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
    deadline: Instant,
) -> Result<Option<Item>, Reason> {
    let attempt = retry_on_network(|| request_item_once(state, user_id, dm_channel_id));
    tokio::time::timeout_at(deadline.into(), attempt)
        .await
        .unwrap_or(Err(Reason::Timeout))
}

async fn request_item_once(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
) -> Result<Option<Item>, Reason> {
    let resp = crate::commands::ds_reads::catch_up(state, dm_channel_id, false, true)
        .await
        .map_err(classify)?;
    let Some(dm) = resp.dm.filter(|_| resp.authorized) else {
        return Ok(None);
    };
    let requester = dm
        .members
        .iter()
        .find(|m| m.user_id == user_id)
        .and_then(|m| m.added_by.clone())
        .filter(|a| !a.is_empty() && a != user_id)
        .unwrap_or_else(|| dm.created_by.clone());
    if requester == user_id {
        return Ok(None);
    }
    let username = dm
        .members
        .iter()
        .find(|m| m.user_id == requester)
        .and_then(|m| m.username.clone())
        .filter(|n| !n.is_empty());
    let users = crate::commands::ds_reads::users(state, vec![requester.clone()], None)
        .await
        .map_err(classify)?;
    let preferred = users.into_iter().find(|u| u.id == requester).and_then(|u| {
        u.preferred_name
            .filter(|n| !n.trim().is_empty())
            .or(Some(u.username).filter(|n| !n.is_empty()))
    });
    let Some(name) = preferred.or(username) else {
        return Ok(None);
    };
    Ok(Some(Item {
        conversation_id: dm_channel_id.to_string(),
        message_id: String::new(),
        sender_name: name,
        conversation_title: None,
        is_dm: true,
        body: String::new(),
        body_key: None,
        muted: false,
        mention: false,
        kind: KIND_DM_REQUEST,
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
    // #1734: a request shows who is asking and nothing they wrote, whatever is stored.
    if let Some(t) = target.as_ref().filter(|t| t.request) {
        let lookup = request_item(state, user_id, &t.conversation_id, deadline);
        return match scope::run(Arc::clone(&t.scope), lookup).await {
            Ok(item) => Outcome::Ok {
                mode,
                items: item.into_iter().collect(),
            },
            Err(reason) => {
                crate::diag!("[notify] request lookup failed: {reason:?}");
                Outcome::Failed(reason)
            }
        };
    }
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
    // #2003: inside a group the sender is named as that group shows them.
    let aliases = alias_names(state, user_id, &rows, deadline).await;
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
        let sender_name = aliases
            .iter()
            .find(|(conv, id, _)| *conv == row.conversation_id && *id == row.sender_id)
            .map(|(_, _, n)| n.clone())
            .or_else(|| {
                names
                    .iter()
                    .find(|(id, _)| *id == row.sender_id)
                    .map(|(_, n)| n.clone())
            })
            .unwrap_or_default();
        items.push(Item {
            conversation_id: row.conversation_id,
            message_id: row.id,
            sender_name,
            conversation_title: title,
            is_dm,
            body,
            body_key,
            muted,
            mention,
            kind: KIND_MESSAGE,
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

/// `(conversation_id, sender_id, alias)` for every channel row whose sender has an
/// alias name in that channel's group (#2003). Each group's aliases are read
/// once from the DS within the deadline, refreshing `group_alias_cache`; when
/// that read fails the cache answers, and a miss leaves the username.
async fn alias_names(
    state: &Arc<AppState>,
    user_id: &str,
    rows: &[Row],
    deadline: Instant,
) -> Vec<(String, String, String)> {
    let row_groups: Vec<(&Row, String)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Vec::new();
        };
        rows.iter()
            .filter_map(|r| {
                db.conn()
                    .query_row(
                        "SELECT group_id FROM conversation_cache
                         WHERE id = ?1 AND kind = 'channel' AND group_id IS NOT NULL",
                        rusqlite::params![r.conversation_id],
                        |q| q.get::<_, String>(0),
                    )
                    .ok()
                    .map(|g| (r, g))
            })
            .collect()
    };
    let mut groups: Vec<&str> = row_groups.iter().map(|(_, g)| g.as_str()).collect();
    groups.sort_unstable();
    groups.dedup();
    for group_id in groups {
        let read = crate::commands::ds_reads::group_aliases(state, user_id, group_id);
        if let Ok(Ok(resp)) = tokio::time::timeout_at(deadline.into(), read).await {
            if resp.authorized {
                let aliases: Vec<crate::commands::groups::GroupAlias> =
                    resp.aliases.into_iter().map(Into::into).collect();
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    let _ = crate::commands::groups::cache_group_aliases(db.conn(), group_id, &aliases);
                }
            }
        }
    }
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return Vec::new();
    };
    row_groups
        .into_iter()
        .filter_map(|(r, g)| {
            crate::commands::groups::cached_alias_name(db.conn(), &g, &r.sender_id)
                .map(|n| (r.conversation_id.clone(), r.sender_id.clone(), n))
        })
        .collect()
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

    /// #1735: a row copied from a sibling is history, not news — neither the
    /// rowid-floor query nor the recent-message fallback may preview it, even
    /// when it lands (an import) after the floor was read and inside the window.
    #[test]
    fn a_transferred_row_is_never_previewed() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        let floor: i64 = conn.query_row("SELECT COALESCE(MAX(rowid), 0) FROM message", [], |r| r.get(0)).unwrap();
        for (id, origin) in [("native", None), ("copied", Some("transfer"))] {
            conn.execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, origin)
                 VALUES (?1, 'c', 'bob', X'', 'hi', '2026-01-01T00:00:00Z', ?2)",
                rusqlite::params![id, origin],
            )
            .unwrap();
        }
        let ids = |sql: String| -> Vec<String> {
            let mut st = conn.prepare(&sql).unwrap();
            st.query_map(rusqlite::params!["me", "c", floor], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        };
        assert_eq!(ids(format!("SELECT id FROM message WHERE {LIVE_INBOUND} AND conversation_id = ?2 AND rowid > ?3")), ["native"]);
        assert_eq!(
            ids(format!(
                "SELECT id FROM message WHERE {LIVE_INBOUND} AND conversation_id = ?2 AND ?3 >= 0
                 AND received_at >= datetime('now', '-600 seconds')"
            )),
            ["native"]
        );
    }

    #[tokio::test]
    async fn a_network_failure_is_retried_until_it_clears() {
        let mut calls = 0;
        let started = Instant::now();
        let r = retry_on_network(|| {
            calls += 1;
            let n = calls;
            async move {
                if n < 3 {
                    Err(Reason::Network)
                } else {
                    Ok(n)
                }
            }
        })
        .await;
        assert_eq!(r, Ok(3));
        // 250 ms, then 500 ms.
        assert!(started.elapsed() >= Duration::from_millis(750));
    }

    #[tokio::test]
    async fn any_other_failure_is_not_retried() {
        let mut calls = 0;
        let r: Result<(), Reason> = retry_on_network(|| {
            calls += 1;
            async { Err(Reason::Internal) }
        })
        .await;
        assert_eq!(r, Err(Reason::Internal));
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn the_caller_deadline_bounds_the_retries() {
        let r = tokio::time::timeout(
            Duration::from_secs(1),
            retry_on_network(|| async { Err::<(), _>(Reason::Network) }),
        )
        .await;
        assert!(r.is_err());
    }

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
                kind: KIND_MESSAGE,
            }],
        };
        let v: serde_json::Value = serde_json::from_str(&ok.to_json()).unwrap();
        assert_eq!(v["ok"], true);
        assert_eq!(v["mode"], "full");
        assert_eq!(v["items"][0]["sender_name"], "alice");
        assert_eq!(v["items"][0]["kind"], "message");
        assert_eq!(KIND_DM_REQUEST, "dm_request");
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
