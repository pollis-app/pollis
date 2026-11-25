//! The iOS share extension's own session (#2100).
//!
//! The extension is a separate process with no app running in it, so it builds a headless
//! state the way the Notification Service Extension's fetch does (#1599): the App Group data
//! dir, the shared keychain group, no realtime, LiveKit or media server, and the database opened
//! with [`LocalDb::open_existing_for_user_in`], which never creates, migrates or wipes.
//!
//! Unlike the notification fetch it never reads the notification escrow: the user unlocks it
//! with the PIN (through [`pin::check_pin`], the app's own counter and lockout) or with
//! biometrics (the app's device unlock key, which gates on the same counter and never writes
//! it). Every MLS step takes its share of the cross-process `state.lock` per section, like the
//! app's, so the app and the extension never advance a group's state over each other.
//!
//! The host must not be suspended holding the database or the lock (`0xdead10cc`): when the
//! share sheet's host app leaves the foreground it calls [`suspend`], and [`resume`] when it
//! comes back; [`close`] before the extension finishes.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use super::send::{QueuedShare, ShareOutcome, ShareRequest};
use super::targets::ShareTarget;
use crate::commands::pin::{self, PinCheck};
use crate::config::Config;
use crate::db::local::LocalDb;
use crate::error::{Error, Result};
use crate::keystore::Keystore;
use crate::state::AppState;

/// The per-user keystore slot `commands::auth` keeps the device id in.
const DEVICE_ID_SLOT: &str = "device_id";

/// What a host hands [`open`].
pub struct Params {
    pub keystore: Arc<dyn Keystore>,
    pub config: Config,
    /// The core's data dir (iOS: the App Group container + `/pollis`).
    pub data_dir: PathBuf,
}

/// Where the extension stands before any unlock. Serialized as the C ABI's `state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Status {
    /// Nobody is signed in on this device (or the app has never run on this build).
    SignedOut,
    /// Signed in, but the PIN was locked out: only the app's recovery can continue.
    LockedOut,
    /// Signed in and locked. `biometric`: the app's biometric unlock is on.
    Locked {
        biometric: bool,
        attempts_remaining: u32,
        /// Whose session it is: the host reads the biometric key item named after it.
        user_id: String,
    },
}

/// What one unlock attempt came to. Serialized as the C ABI's `result`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Unlock {
    Unlocked,
    /// A wrong PIN, counted against the app's own attempts.
    Incorrect { attempts_remaining: u32 },
    /// The attempt that reached the threshold, or one after it: the keys are gone.
    LockedOut,
    /// The biometric key did not unlock (re-enrolled, turned off): the keypad, nothing spent.
    BiometricFailed,
    /// The database would not open here (the app has not finished an update yet).
    OpenApp,
    /// No session: [`open`] was not called, or found nobody signed in.
    NoSession,
}

/// The factor the user unlocked with.
pub enum Factor {
    Pin(String),
    /// The device unlock key the host read from the biometric-bound keychain item, base64.
    DeviceKey(String),
}

struct Session {
    state: Arc<AppState>,
    user_id: String,
    data_dir: PathBuf,
    /// The database was closed by [`suspend`] and should reopen on [`resume`].
    parked: bool,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn session_state() -> Option<(Arc<AppState>, String, PathBuf)> {
    SESSION
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .as_ref()
        .map(|s| (Arc::clone(&s.state), s.user_id.clone(), s.data_dir.clone()))
}

async fn unlocked_state() -> Result<Arc<AppState>> {
    let (state, _, _) = session_state().ok_or_else(|| Error::Other(anyhow::anyhow!("no share session")))?;
    // A resume that could not reopen the database (it was busy) leaves it to the next call.
    if !crate::notify::lock::suspend::is_suspending() {
        reopen_if_parked();
    }
    if state.unlock.lock().await.is_none() || state.local_db.lock().await.is_none() {
        return Err(Error::Other(anyhow::anyhow!("the share session is locked")));
    }
    Ok(state)
}

/// Read who is signed in and start a locked session for them. Replaces any earlier session.
pub async fn open(p: Params) -> Status {
    close();
    // iOS reuses an extension process: a gate an earlier share closed must not outlive it.
    crate::notify::lock::suspend::end();
    let Some(user_id) = crate::accounts::read_accounts_index()
        .ok()
        .and_then(|i| i.last_active_user)
        .filter(|u| !u.is_empty())
    else {
        return Status::SignedOut;
    };
    let device_id = match p.keystore.load_for_user(DEVICE_ID_SLOT, &user_id).await {
        Ok(Some(bytes)) => match String::from_utf8(bytes) {
            Ok(id) if !id.is_empty() => id,
            _ => return Status::SignedOut,
        },
        _ => return Status::SignedOut,
    };
    let attempts_remaining = match pin::attempts_remaining(p.keystore.as_ref(), &user_id).await {
        Ok(Some(n)) => n,
        // No PIN slot: a lockout already wiped it, or sign-in never finished.
        Ok(None) => return Status::SignedOut,
        Err(e) => {
            crate::diag!("[share] keystore unreadable: {e}");
            return Status::SignedOut;
        }
    };
    if attempts_remaining == 0 {
        return Status::LockedOut;
    }
    let has_db = crate::db::local::user_db_path(&p.data_dir, &user_id).is_ok_and(|path| path.is_file());
    if !has_db {
        return Status::SignedOut;
    }
    let biometric = crate::commands::device_unlock::is_enabled(p.keystore.as_ref(), &user_id).await;

    let state = AppState::new_with_parts(p.config, p.keystore);
    let _ = state.local_dir.set(p.data_dir.clone());
    let state = Arc::new(state);
    *state.device_id.lock().await = Some(device_id);
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(Session {
        state,
        user_id: user_id.clone(),
        data_dir: p.data_dir,
        parked: false,
    });
    Status::Locked {
        biometric,
        attempts_remaining,
        user_id,
    }
}

/// Unlock the session: verify the factor, then open the database.
pub async fn unlock(factor: Factor) -> Unlock {
    let Some((state, user_id, data_dir)) = session_state() else {
        return Unlock::NoSession;
    };
    let keystore = state.keystore.as_ref();
    let unlocked = match factor {
        Factor::Pin(pin) => match pin::check_pin(keystore, &user_id, &pin, Some(&data_dir)).await {
            Ok(PinCheck::Unlocked(u)) => u,
            Ok(PinCheck::Incorrect { attempts_remaining }) => {
                return Unlock::Incorrect { attempts_remaining }
            }
            Ok(PinCheck::LockedOut) => return Unlock::LockedOut,
            Err(e) => {
                crate::diag!("[share] PIN check failed: {e}");
                // No PIN slot left: a lockout (here or in the app) took it.
                return match pin::attempts_remaining(keystore, &user_id).await {
                    Ok(None) => Unlock::LockedOut,
                    _ => Unlock::NoSession,
                };
            }
        },
        Factor::DeviceKey(key) => {
            match crate::commands::device_unlock::unwrap_with_device_key(keystore, &user_id, &key).await {
                Ok(u) => u,
                Err(e) => {
                    let locked_out = matches!(pin::attempts_remaining(keystore, &user_id).await, Ok(None | Some(0)));
                    crate::diag!("[share] biometric unlock refused: {e}");
                    return if locked_out { Unlock::LockedOut } else { Unlock::BiometricFailed };
                }
            }
        }
    };
    let db = match LocalDb::open_existing_for_user_in(&data_dir, &user_id, &unlocked.db_key) {
        Ok(db) => db,
        Err(e) => {
            crate::diag!("[share] local database would not open: {e}");
            return Unlock::OpenApp;
        }
    };
    *state.local_db.lock().await = Some(db);
    *state.unlock.lock().await = Some(unlocked);
    Unlock::Unlocked
}

/// The picker's rows (see [`super::targets`]).
pub async fn targets(query: Option<String>, limit: Option<u32>) -> Result<Vec<ShareTarget>> {
    let state = unlocked_state().await?;
    super::targets::share_targets(&state, query, limit).await
}

/// Store the share, report `stored`, then deliver it, reporting each target as it ends.
pub async fn send<S, P>(req: ShareRequest, on_stored: S, progress: P) -> Result<Vec<ShareOutcome>>
where
    S: FnOnce(&[QueuedShare]),
    P: Fn(&ShareOutcome) + Send + Sync,
{
    let state = unlocked_state().await?;
    let queued = super::send::enqueue_share(&state, &req).await?;
    on_stored(&queued);
    Ok(super::send::deliver_share(&state, &queued, progress).await)
}

/// The host app is leaving the foreground: let the sends in flight finish for up to ¾ of
/// `timeout`, then take no new hold of `state.lock`, wait for the open ones, and close the
/// database, marking anything still unsent `failed` for the app to finish. True when nothing is
/// held afterwards.
pub fn suspend(timeout: std::time::Duration) -> bool {
    let ticket = crate::notify::lock::suspend::ticket();
    let entered = crate::notify::lock::suspend::enter(ticket, timeout);
    let mut guard = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    // [`resume`] ends the suspension under this same mutex: if it came first, the host is back
    // in the foreground and the database stays open.
    if !crate::notify::lock::suspend::is_current(ticket) {
        return true;
    }
    let Some(session) = guard.as_mut() else {
        return entered.drained;
    };
    release_claims(&session.state);
    match crate::bridge::close_db_bounded(&session.state, std::time::Duration::from_secs(2)) {
        Ok(Some(_)) => {
            session.parked = true;
            entered.drained
        }
        Ok(None) => entered.drained,
        Err(()) => {
            crate::diag!("[share] the database stayed busy; not closed before suspension");
            false
        }
    }
}

/// Hand this process's outbox claims back before it stops (#2100), so the app delivers what is
/// left at once instead of waiting out the leases.
fn release_claims(state: &Arc<AppState>) {
    let Ok(guard) = state.local_db.try_lock() else {
        return;
    };
    if let Some(db) = guard.as_ref() {
        if let Err(e) = crate::commands::messages::release_outbox_claims_on(db.conn()) {
            crate::diag!("[share] could not release the outbox claims: {e}");
        }
    }
}

/// Back in the foreground: holds may start again, and a database [`suspend`] closed reopens.
pub fn resume() {
    let _session = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    // Under the session mutex, so a [`suspend`] still letting sends finish sees it and does not
    // close the database after this.
    crate::notify::lock::suspend::end();
    drop(_session);
    reopen_if_parked();
}

/// Reopen a database [`suspend`] closed. Left parked when the keys or the slot are busy; the
/// next [`targets`] or [`send`] tries again.
fn reopen_if_parked() {
    let mut guard = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    let Some(session) = guard.as_mut() else {
        return;
    };
    if !session.parked {
        return;
    }
    let key = match session.state.unlock.try_lock() {
        Ok(u) => match u.as_ref() {
            Some(u) => u.db_key.clone(),
            None => return,
        },
        Err(_) => return,
    };
    let Ok(mut slot) = session.state.local_db.try_lock() else {
        return;
    };
    match LocalDb::open_existing_for_user_in(&session.data_dir, &session.user_id, &key) {
        Ok(db) => {
            *slot = Some(db);
            session.parked = false;
        }
        Err(e) => crate::diag!("[share] database not reopened after resume: {e}"),
    }
}

/// End the session: release the lock and close the database. Call before the extension
/// completes its request. Sends still unfinished stay in the outbox for the app.
pub fn close() {
    let session = SESSION.lock().unwrap_or_else(|p| p.into_inner()).take();
    let Some(session) = session else {
        return;
    };
    // As the app's suspension does: no new hold of `state.lock`, and the open ones end (or run
    // out of time) before it is released, so nothing keeps working on MLS state without it.
    let ticket = crate::notify::lock::suspend::ticket();
    if !crate::notify::lock::suspend::begin(ticket, std::time::Duration::from_secs(2)) {
        crate::diag!("[share] closing with work still holding the lock");
    }
    release_claims(&session.state);
    session.state.state_lock.force_release();
    let _ = crate::bridge::close_db_bounded(&session.state, std::time::Duration::from_secs(2));
    // The session is gone and its database closed: anything left of it finds nothing to work
    // on, so the gate need not stay shut (a host that is not a separate process, such as the
    // flows harness, keeps running work of its own).
    crate::notify::lock::suspend::end();
    // The keys go with the state; nothing of the session outlives it.
    let cleared = session.state.unlock.try_lock().map(|mut u| *u = None);
    drop(cleared);
}
