use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::{Mutex, Notify};

use crate::config::Config;
use crate::db::local::LocalDb;
use crate::keystore::{self, Keystore};
use crate::commands::pin::UnlockState;
#[cfg(feature = "media")]
use crate::commands::camera::CameraState;
#[cfg(feature = "media")]
use crate::commands::screenshare::ScreenShareState;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use crate::commands::terminal::PtySession;
#[cfg(feature = "media")]
use crate::commands::voice::VoiceState;
#[cfg(feature = "media")]
use crate::commands::voice_test::VoiceTestState;
use crate::realtime::LiveKitState;

#[derive(Clone)]
pub struct OtpEntry {
    pub hash: String,
    pub expires_at: u64,
}

pub struct AppState {
    pub config: Config,
    /// None until a user logs in. Opened per-user as pollis_{user_id}.db; the
    /// only database this process opens (#987).
    pub local_db: Arc<Mutex<Option<LocalDb>>>,
    /// Pluggable secret store: [`OsKeystore`] in production, one
    /// [`InMemoryKeystore`] per simulated client in tests.
    pub keystore: Arc<dyn Keystore>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    /// #836 identity translations, keyed `(logical_room, wire_identity)` and
    /// valued `(internal_identity, display_name)`: a memo of the DS lookups that
    /// open LiveKit's per-room pseudonyms.
    ///
    /// Its own lock so voice and realtime paths never nest two locks. Holds the
    /// finished identity because the local participant's entry is pinned at
    /// join, not looked up. Never invalidated: a pseudonym is deterministic.
    #[cfg(feature = "media")]
    pub livekit_identities: Arc<Mutex<crate::commands::livekit_identity::IdentityCache>>,
    #[cfg(feature = "media")]
    pub voice: Arc<Mutex<VoiceState>>,
    #[cfg(feature = "media")]
    pub voice_test: Arc<Mutex<VoiceTestState>>,
    #[cfg(feature = "media")]
    pub screenshare: Arc<Mutex<ScreenShareState>>,
    #[cfg(feature = "media")]
    pub camera: Arc<Mutex<CameraState>>,
    pub update_required: Arc<AtomicBool>,
    /// The device the DS answered `device_revoked` for (#1338), bound to the
    /// `(user_id, device_id)` that signed the refused request, so a late
    /// response can never act on another account. Cleared by that logout.
    pub revoked_device: Arc<std::sync::Mutex<Option<RevokedDevice>>>,
    /// Per-device ULID, set during login; kept in the keystore so it survives
    /// local DB wipes.
    pub device_id: Arc<Mutex<Option<String>>>,
    /// Ephemeral X25519 private keys for pending enrollment requests, by
    /// request id. Memory only: a restart mid-enrollment starts over.
    pub enrollment_ephemeral_keys: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// QR device links this device created (#1207): link_id → link token.
    /// Memory only; the token is the QR's secret.
    pub device_link_tokens: Arc<Mutex<HashMap<String, zeroize::Zeroizing<Vec<u8>>>>>,
    /// The QR device link this (new) device claimed (#1207), read by
    /// `start_device_enrollment` to tag its request.
    pub device_link_pending: Arc<Mutex<Option<crate::commands::device_link::PendingDeviceLink>>>,
    /// The code this NEW device is showing (#1361): the poll secret and the
    /// ephemeral private key its QR does not carry. Memory only.
    pub device_offer: Arc<Mutex<Option<crate::commands::device_link::DeviceOfferInProgress>>>,
    /// Codes this signed-in device accepted (#1361): link_id → the ephemeral
    /// public key the QR showed. Approval wraps only to a request with that key.
    pub device_offer_accepts: Arc<Mutex<HashMap<String, [u8; 32]>>>,
    /// In-memory PIN unlock state. `Some` once the user has entered a valid PIN
    /// (or just set one via `set_pin`); dropped by `lock`.
    pub unlock: Arc<Mutex<Option<UnlockState>>>,
    /// The OTP-session bearer from `verify-otp` on first-device signup, held
    /// until the first device-cert publish spends it.
    /// See `docs/otp-server-bootstrap-design.md`.
    pub bootstrap_session: Arc<Mutex<Option<String>>>,
    /// The OTP-session bearer from `verify-otp` on a subsequent-device sign-in,
    /// for the pre-credential writes (`register-device`, the enrollment
    /// request). Separate from [`bootstrap_session`]: the subsequent-device
    /// cert publish must not consume a session, since approval can outlast the
    /// TTL (`docs/otp-server-bootstrap-design.md` §5).
    pub enrollment_session: Arc<Mutex<Option<String>>>,
    /// Bound port of the loopback media server; `None` where none was started.
    pub media_server_port: Arc<Mutex<Option<u16>>>,
    /// Per-session token for the loopback media server, rotated on unlock and
    /// cleared on logout; other local users could otherwise read media.
    pub media_server_token: Arc<Mutex<Option<String>>>,
    /// Live terminal sessions by `terminal_open` id; dropping one kills and
    /// reaps its shell. Desktop only.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub terminals: Arc<Mutex<HashMap<String, PtySession>>>,
    /// Tells long-lived background tasks (the media server) to stop, via
    /// [`AppState::shutdown`], so they never pin the runtime and hang exit (#335).
    pub shutdown_signal: Arc<Notify>,
    /// Per-conversation locks serializing MLS group mutations in this process.
    /// Without them two callers can both external-join and commit at the same
    /// epoch, forking the group. Cross-device races are caught by
    /// `UNIQUE(conversation_id, epoch)` on `mls_commit_log`.
    pub mls_group_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Per-message outbox locks (#1335), so a send, a retry and a drain never
    /// post the same message concurrently.
    pub outbox_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Held while topping up the KeyPackage pool (#1412). Taken with `try_lock`,
    /// so a running top-up fills the whole shortfall alone.
    pub key_package_refill: Arc<Mutex<()>>,
    /// Held for an outbox drain, carrying its report: `flush_outbox` is
    /// single-flight, and a caller that finds it held reuses the report (#1335).
    pub outbox_drain: Arc<Mutex<Option<crate::commands::messages::OutboxReport>>>,
    /// Set by a `flush_outbox` that found a drain running, so the trigger is
    /// not lost (#1335).
    pub outbox_rerun: Arc<AtomicBool>,
    /// Fan-out of decoded remote screenshare frames (packed I420,
    /// `pack_frame_bytes`) to the media server's WebSocket route.
    /// Latest-frame-wins: lagged receivers drop old frames.
    pub screenshare_frame_tx: tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>,
    /// The running overlay relay shim, or `None` when the overlay is off.
    /// Dropping it aborts the shim. Swapped live by
    /// `commands::overlay::set_overlay_mode`; a `std::sync::Mutex` so the
    /// hot-path read never holds a lock across an await.
    pub overlay: std::sync::Mutex<Option<Arc<pollis_relay::OverlayHandle>>>,
}

impl AppState {
    /// Builds the state; the shell then calls
    /// `commands::overlay::apply_overlay_mode`, the same path a settings toggle
    /// uses (design §14).
    pub async fn new(config: Config) -> crate::error::Result<Self> {
        Ok(Self::new_with_parts(
            config,
            keystore::default_os_keystore(),
        ))
    }

    /// Build AppState from pre-constructed parts; tests inject an
    /// [`InMemoryKeystore`].
    pub fn new_with_parts(
        config: Config,
        keystore: Arc<dyn Keystore>,
    ) -> Self {
        Self {
            config,
            local_db: Arc::new(Mutex::new(None)),
            keystore,
            otp_store: Arc::new(Mutex::new(HashMap::new())),
            livekit: Arc::new(Mutex::new(LiveKitState::new())),
            #[cfg(feature = "media")]
            livekit_identities: Arc::new(Mutex::new(Default::default())),
            #[cfg(feature = "media")]
            voice: Arc::new(Mutex::new(VoiceState::new())),
            #[cfg(feature = "media")]
            voice_test: Arc::new(Mutex::new(VoiceTestState::new())),
            #[cfg(feature = "media")]
            screenshare: Arc::new(Mutex::new(ScreenShareState::new())),
            #[cfg(feature = "media")]
            camera: Arc::new(Mutex::new(CameraState::new())),
            update_required: Arc::new(AtomicBool::new(false)),
            revoked_device: Arc::new(std::sync::Mutex::new(None)),
            device_id: Arc::new(Mutex::new(None)),
            enrollment_ephemeral_keys: Arc::new(Mutex::new(HashMap::new())),
            device_link_tokens: Arc::new(Mutex::new(HashMap::new())),
            device_link_pending: Arc::new(Mutex::new(None)),
            device_offer: Arc::new(Mutex::new(None)),
            device_offer_accepts: Arc::new(Mutex::new(HashMap::new())),
            unlock: Arc::new(Mutex::new(None)),
            bootstrap_session: Arc::new(Mutex::new(None)),
            enrollment_session: Arc::new(Mutex::new(None)),
            media_server_port: Arc::new(Mutex::new(None)),
            media_server_token: Arc::new(Mutex::new(None)),
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            terminals: Arc::new(Mutex::new(HashMap::new())),
            shutdown_signal: Arc::new(Notify::new()),
            mls_group_locks: Arc::new(Mutex::new(HashMap::new())),
            outbox_locks: Arc::new(Mutex::new(HashMap::new())),
            key_package_refill: Arc::new(Mutex::new(())),
            outbox_drain: Arc::new(Mutex::new(None)),
            outbox_rerun: Arc::new(AtomicBool::new(false)),
            screenshare_frame_tx: tokio::sync::broadcast::channel(8).0,
            // Off until `apply_overlay_mode` starts a shim.
            overlay: std::sync::Mutex::new(None),
        }
    }

    /// The overlay shim handle in force, or `None` when the overlay is off. Every
    /// control-plane HTTP caller reads it, so a mode switch applies immediately.
    pub fn overlay_handle(&self) -> Option<Arc<pollis_relay::OverlayHandle>> {
        self.overlay.lock().unwrap().clone()
    }

    /// Acquire the per-conversation MLS lock (keyed by the MLS group id). Hold
    /// it across the whole read-modify-write of the group's local state plus its
    /// `mls_commit_log` insert. Non-reentrant: holders call the `*_locked`
    /// helper variants.
    pub async fn mls_group_lock(
        &self,
        conversation_id: &str,
    ) -> tokio::sync::OwnedMutexGuard<()> {
        let entry = {
            let mut map = self.mls_group_locks.lock().await;
            map.entry(conversation_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        entry.lock_owned().await
    }

    /// Tear down long-lived background tasks so the host process can exit.
    /// Idempotent: a quit and an update-relaunch can both reach it.
    ///
    /// Signals `shutdown_signal` (the media server stops accepting), then closes
    /// every LiveKit room and the voice room, aborting their tasks. Terminal
    /// sessions die with their shells at exit.
    pub async fn shutdown(&self) {
        self.shutdown_signal.notify_waiters();

        #[cfg(feature = "media")]
        {
            let mut guard = self.livekit.lock().await;
            let rooms = std::mem::take(&mut guard.rooms);
            drop(guard);
            for (room_id, (room, handle)) in rooms {
                if let Err(e) = room.close().await {
                    crate::diag!("[shutdown] livekit room {room_id} close failed: {e}");
                }
                handle.abort();
            }

            // The voice room lives in `self.voice.room`, not `livekit.rooms`.
            // Leaving it open keeps a ghost participant until the SFU times us
            // out. Tasks are aborted first so the Disconnected handler cannot
            // race, and the close is bounded so a dead connection can't stall
            // the quit.
            let room = {
                let mut voice = self.voice.lock().await;
                if let Some(t) = voice.frame_task.take() {
                    t.abort();
                }
                if let Some(t) = voice.room_task.take() {
                    t.abort();
                }
                voice.room.take()
            };
            if let Some(room) = room {
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(3),
                    room.close(),
                )
                .await;
            }
        }
    }

    /// Open the per-user database under a caller-supplied DB key (from
    /// `set_pin` or `unlock`); never reads the keystore.
    ///
    /// Returns whether `mls_kv` was empty (a fresh or wiped DB). The caller then
    /// runs `reset_welcome_delivery`, after `ensure_device_cert`, so the signed
    /// reset authenticates and the check is not fooled by the cert's own write.
    pub async fn load_user_db_with_key(
        &self,
        user_id: &str,
        db_key: &[u8],
    ) -> crate::error::Result<bool> {
        let db = LocalDb::open_for_user(user_id, db_key)?;

        let mls_empty: bool = db.conn()
            .query_row("SELECT COUNT(*) FROM mls_kv", [], |r| r.get::<_, i64>(0))
            .map(|c| c == 0)
            .unwrap_or(true);

        // Best-effort: a failed retention sweep must never block login.
        if let Err(e) = crate::db::local::evict_old_messages(db.conn()) {
            crate::diag!("[state] startup message eviction failed (non-fatal): {e}");
        }
        crate::commands::messages::loss::prune_known_envelopes(db.conn());

        *self.local_db.lock().await = Some(db);
        // Spawned and chunked (#850): it is the one open-path cost that scales
        // with the corpus.
        spawn_search_index_maintenance(self.local_db.clone());
        // Scope the media cache to this user: two dev clients on one machine
        // otherwise share it under different db_keys.
        crate::commands::r2::set_cache_user(Some(user_id));
        Ok(mls_empty)
    }

    /// Close the current user's database (called on logout).
    pub async fn unload_user_db(&self) {
        *self.local_db.lock().await = None;
        crate::commands::r2::set_cache_user(None);
        // The corpus count is per-account.
        crate::commands::messages::invalidate_corpus_cache();
    }

    /// Whether `(user_id, device_id)` is the device the DS called revoked.
    pub fn is_revoked_device(&self, user_id: &str, device_id: &str) -> bool {
        self.revoked_device
            .lock()
            .map(|g| {
                g.as_ref()
                    .is_some_and(|r| r.user_id == user_id && r.device_id == device_id)
            })
            .unwrap_or(false)
    }

    /// Record that the DS refused `(user_id, device_id)` as revoked. Callers
    /// pass the identity that SIGNED the refused request, and must only call
    /// this while that device is still this process's device.
    pub fn mark_revoked_device(&self, user_id: &str, device_id: &str) {
        if let Ok(mut g) = self.revoked_device.lock() {
            *g = Some(RevokedDevice {
                user_id: user_id.to_string(),
                device_id: device_id.to_string(),
            });
        }
    }

    /// The account whose device was revoked, if any.
    pub fn revoked_user(&self) -> Option<String> {
        self.revoked_device
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|r| r.user_id.clone()))
    }

    pub fn clear_revoked_device(&self) {
        if let Ok(mut g) = self.revoked_device.lock() {
            *g = None;
        }
    }

    pub fn check_not_outdated(&self) -> crate::error::Result<()> {
        if self.update_required.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::error::Error::ClientOutdated);
        }
        Ok(())
    }
}

/// Proof, at the type level, that its holder has the per-conversation MLS lock.
///
/// #1079: a post that seals an envelope must serialize behind any committer on
/// this device, or it picks an epoch the commit log has not decided.
/// [`crate::commands::messages::seal`]'s locked sealer demands one of these, and
/// only [`MlsLockHeld::from_guard`] makes one, so a post site that forgets the
/// lock does not compile. `receipts::emit_receipt` deliberately uses the
/// unlocked sealer (taking the lock there would deadlock catch-up).
///
/// Borrows the guard, so it cannot extend the lock's lifetime.
pub struct MlsLockHeld<'a> {
    _guard: &'a tokio::sync::OwnedMutexGuard<()>,
}

impl<'a> MlsLockHeld<'a> {
    pub fn from_guard(guard: &'a tokio::sync::OwnedMutexGuard<()>) -> Self {
        Self { _guard: guard }
    }
}

/// Bring the local full-text search index up to date, off the sign-in path (#850).
///
/// 1. **Auto-repair**: rebuild if `integrity-check` fails; a contentless index
///    drifts if `pollis_search_text` ever changes its output. Silent by design.
/// 2. **Backfill** older history in chunks, yielding between them so the
///    local-DB mutex never sits in front of a send.
///
/// Best-effort: degraded search must never cost a login or a message.
fn spawn_search_index_maintenance(local_db: Arc<Mutex<Option<LocalDb>>>) {
    tokio::spawn(async move {
        {
            let guard = local_db.lock().await;
            let Some(db) = guard.as_ref() else {
                return;
            };
            if !crate::db::local::search_index_is_healthy(db.conn()) {
                crate::diag!("[search] FTS index failed integrity-check; rebuilding");
                if let Err(e) = crate::db::local::rebuild_search_index(db.conn()) {
                    crate::diag!("[search] index rebuild failed (non-fatal): {e}");
                }
                return;
            }
        }

        loop {
            let indexed = {
                let guard = local_db.lock().await;
                let Some(db) = guard.as_ref() else {
                    return;
                };
                match crate::db::local::search_backfill_pending(db.conn()) {
                    Ok(false) => return,
                    Ok(true) => {}
                    Err(e) => {
                        crate::diag!("[search] backfill state unreadable (non-fatal): {e}");
                        return;
                    }
                }
                match crate::db::local::backfill_search_index_chunk(db.conn()) {
                    Ok(0) => {
                        if let Err(e) = crate::db::local::finish_search_backfill(db.conn()) {
                            crate::diag!("[search] backfill finalisation failed (non-fatal): {e}");
                        }
                        return;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        crate::diag!("[search] backfill chunk failed (non-fatal): {e}");
                        return;
                    }
                }
            };
            debug_assert!(indexed > 0);
            tokio::task::yield_now().await;
        }
    });
}

#[cfg(test)]
mod guard_lifetime_tests {
    use std::path::{Path, PathBuf};

    /// In edition 2021 a temporary in an `if let` / `match` / `while let`
    /// scrutinee lives until the end of the whole statement, so
    ///
    /// ```ignore
    /// if let Some(id) = state.device_id.lock().await.clone() {
    ///     some_network_round_trip(id).await;   // <- lock STILL HELD
    /// }
    /// ```
    ///
    /// holds the mutex across the await (#875), one refactor from a
    /// self-deadlock. Bind it with a `let` statement instead. A source-shape
    /// check, so it cannot see a guard held on purpose.
    #[test]
    fn no_lock_guard_in_a_scrutinee_spans_an_await() {
        let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&src_root, &mut files);
        assert!(
            files.len() > 50,
            "walked only {} files under {} — the walk is broken, not clean",
            files.len(),
            src_root.display()
        );

        let mut offenders = Vec::new();
        for file in &files {
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            let lines: Vec<&str> = src.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") || !line.contains(".lock().await") {
                    continue;
                }
                if !is_scrutinee(t) {
                    continue;
                }
                if body_awaits(&lines, i) {
                    offenders.push(format!(
                        "{}:{}\n      {}",
                        file.strip_prefix(&src_root).unwrap_or(file).display(),
                        i + 1,
                        t
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "a lock guard taken in a `if let`/`match`/`while let` scrutinee lives until the end \
             of the statement in edition 2021, so these hold it across an await. Bind it to a \
             `let` statement first:\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Does this line open an `if let` / `match` / `while let` whose scrutinee is
    /// the locking expression (including `let x = match …`)?
    fn is_scrutinee(trimmed: &str) -> bool {
        let rest = trimmed
            .strip_prefix('}')
            .map(str::trim_start)
            .unwrap_or(trimmed);
        let rest = match rest.find('=') {
            Some(eq) if rest.starts_with("let ") => rest[eq + 1..].trim_start(),
            _ => rest,
        };
        rest.starts_with("if let ") || rest.starts_with("match ") || rest.starts_with("while let ")
    }

    /// Whether the braced body opened at or after `start` contains an `.await`.
    fn body_awaits(lines: &[&str], start: usize) -> bool {
        let mut depth: i32 = 0;
        let mut opened = false;
        for (n, line) in lines.iter().enumerate().skip(start).take(400) {
            for ch in line.chars() {
                match ch {
                    '{' => {
                        depth += 1;
                        opened = true;
                    }
                    '}' => depth -= 1,
                    _ => {}
                }
            }
            if opened && n > start && line.contains(".await") {
                return true;
            }
            if opened && depth <= 0 {
                return false;
            }
        }
        false
    }

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
}

/// The device the DS answered `device_revoked` for (#1338).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedDevice {
    pub user_id: String,
    pub device_id: String,
}
