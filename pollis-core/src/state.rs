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
    /// None until a user logs in. Opened per-user as pollis_{user_id}.db.
    ///
    /// The only database this process opens (#987); remote reads go through the DS.
    pub local_db: Arc<Mutex<Option<LocalDb>>>,
    /// Test-harness only (#1524): a per-device data dir, so sibling devices don't share one DB. Set before sign-in.
    #[cfg(any(test, feature = "test-harness"))]
    pub local_dir: std::sync::OnceLock<std::path::PathBuf>,
    /// Pluggable secret store; tests inject an [`InMemoryKeystore`] per simulated client.
    pub keystore: Arc<dyn Keystore>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    /// #836 identity translations, keyed `(logical_room, wire_identity)` and
    /// valued `(internal_identity, display_name)`.
    ///
    /// Memo of the DS's `/v1/livekit/identities` answers; pseudonyms only the DS can open.
    /// Its own lock so voice/realtime paths never nest two locks.
    /// Stores the internal identity because the LOCAL participant's entry is pinned at join, not derived.
    /// Never invalidated: a pseudonym is deterministic, so entries cannot go stale.
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
    /// The minimum-version policy's last verdict for this build (#1611); a blocked one makes every
    /// DS request fail locally with [`crate::error::Error::UpgradeRequired`].
    pub client_policy: Arc<std::sync::Mutex<crate::client_policy::PolicyRuntime>>,
    /// The device the DS answered `device_revoked` for (#1338), bound to the signing `(user_id, device_id)` so a late response cannot act on another account.
    pub revoked_device: Arc<std::sync::Mutex<Option<RevokedDevice>>>,
    /// Per-device ULID, kept in the OS keystore so it survives local DB wipes.
    pub device_id: Arc<Mutex<Option<String>>>,
    /// Ephemeral X25519 private keys for pending enrollment requests, by request id.
    ///
    /// Memory only; the 10-minute request TTL bounds the exposure.
    pub enrollment_ephemeral_keys: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// QR device links this device created (#1207): link_id → link token. Memory only.
    pub device_link_tokens: Arc<Mutex<HashMap<String, zeroize::Zeroizing<Vec<u8>>>>>,
    /// The QR device link this (new) device claimed (#1207), read by
    /// `start_device_enrollment` to tag its request.
    pub device_link_pending: Arc<Mutex<Option<crate::commands::device_link::PendingDeviceLink>>>,
    /// The code this NEW device is showing (#1361): the poll secret and the
    /// ephemeral private key its QR does not carry. Memory only.
    pub device_offer: Arc<Mutex<Option<crate::commands::device_link::DeviceOfferInProgress>>>,
    /// Codes this device scanned and accepted (#1361): link_id → the QR's ephemeral public key.
    pub device_offer_accepts: Arc<Mutex<HashMap<String, [u8; 32]>>>,
    /// PIN unlock state. `Some` once a valid PIN was entered or set; dropped by `lock`.
    pub unlock: Arc<Mutex<Option<UnlockState>>>,
    /// OTP-session bearer from first-device signup, held until the first cert publish spends it.
    pub bootstrap_session: Arc<Mutex<Option<String>>>,
    /// OTP-session bearer for a subsequent-device sign-in, for pre-credential writes. Separate from [`bootstrap_session`] (§5).
    pub enrollment_session: Arc<Mutex<Option<String>>>,
    /// Bound port of the loopback media server; `None` where it is not started.
    pub media_server_port: Arc<Mutex<Option<u16>>>,
    /// Per-session token for the media server; rotated on unlock, cleared on logout. Other local users must not read media.
    pub media_server_token: Arc<Mutex<Option<String>>>,
    /// Live in-app terminal sessions by id; dropping one kills its shell. Desktop only.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub terminals: Arc<Mutex<HashMap<String, PtySession>>>,
    /// Tells long-lived background tasks (the media server) to shut down; fired by `shutdown()`.
    ///
    /// Without it the accept loop pins the runtime and the process hangs on quit (#335).
    pub shutdown_signal: Arc<Notify>,
    /// Per-conversation locks serializing local MLS mutations, so two callers can't both external-join and fork the group. Cross-device races hit `UNIQUE(conversation_id, epoch)`.
    pub mls_group_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Per-group single-flight for the ingesting catch-up (#1629), so overlapping triggers never fetch and open the same envelopes twice.
    pub catch_up_flights: crate::commands::messages::ingest_guard::CatchUpFlights,
    /// Per-message outbox locks (#1335): never post the same message twice at once.
    pub outbox_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Held during a KeyPackage top-up (#1412); `try_lock` so a drain never publishes a second batch.
    pub key_package_refill: Arc<Mutex<()>>,
    /// Last pre-seal missing-device check per conversation (#1571), so a burst of sends checks once.
    pub pre_seal_checked: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Single-flight outbox drain (#1335); a waiting caller answers with the last report.
    pub outbox_drain: Arc<Mutex<Option<crate::commands::messages::OutboxReport>>>,
    /// A `flush_outbox` that found a drain running asks it to go round again (#1335).
    pub outbox_rerun: Arc<AtomicBool>,
    /// Decoded screenshare frames to the media server's WebSocket route. Small capacity; lagged receivers drop (latest-frame-wins).
    pub screenshare_frame_tx: tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>,
    /// The running overlay shim, or `None` when off; control-plane HTTP reads it via [`overlay_handle`](AppState::overlay_handle).
    ///
    /// Swapped live by `set_overlay_mode`; a `std::sync::Mutex` so the hot read holds no `.await`.
    pub overlay: std::sync::Mutex<Option<Arc<pollis_relay::OverlayHandle>>>,
    /// The cross-process state lock (#1599), taken by every MLS section alongside its in-process
    /// mutex. A headless fetch's state is built with [`LockPolicy::HeldByCaller`](crate::notify::lock::LockPolicy).
    pub state_lock: Arc<crate::notify::lock::SharedStateLock>,
}

impl AppState {
    /// `async` for its callers; the shell applies the overlay afterwards, via the same path as a settings toggle.
    pub async fn new(config: Config) -> crate::error::Result<Self> {
        Ok(Self::new_with_parts(
            config,
            keystore::default_os_keystore(),
        ))
    }

    /// Build AppState from parts; tests inject an [`InMemoryKeystore`].
    pub fn new_with_parts(
        config: Config,
        keystore: Arc<dyn Keystore>,
    ) -> Self {
        Self {
            config,
            local_db: Arc::new(Mutex::new(None)),
            #[cfg(any(test, feature = "test-harness"))]
            local_dir: std::sync::OnceLock::new(),
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
            client_policy: Arc::default(),
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
            catch_up_flights: Arc::default(),
            outbox_locks: Arc::new(Mutex::new(HashMap::new())),
            key_package_refill: Arc::new(Mutex::new(())),
            pre_seal_checked: Arc::new(Mutex::new(HashMap::new())),
            outbox_drain: Arc::new(Mutex::new(None)),
            outbox_rerun: Arc::new(AtomicBool::new(false)),
            screenshare_frame_tx: tokio::sync::broadcast::channel(8).0,
            overlay: std::sync::Mutex::new(None),
            state_lock: crate::notify::lock::SharedStateLock::new(
                crate::notify::lock::LockPolicy::PerSection,
            ),
        }
    }

    /// The overlay shim in force, or `None`. Cheap: no `.await` held.
    pub fn overlay_handle(&self) -> Option<Arc<pollis_relay::OverlayHandle>> {
        self.overlay.lock().unwrap().clone()
    }

    /// Hold for the whole read-modify-write of local MLS state plus its `mls_commit_log` INSERT. Non-reentrant: use the `*_locked` variants.
    ///
    /// Also takes the cross-process [`state lock`](crate::notify::lock) (#1599), after the
    /// in-process mutex; fails only if another process held it past
    /// [`APP_WAIT`](crate::notify::lock::APP_WAIT), so the step is retried later instead of racing it.
    pub async fn mls_group_lock(
        &self,
        conversation_id: &str,
    ) -> crate::error::Result<MlsGroupGuard> {
        let entry = {
            let mut map = self.mls_group_locks.lock().await;
            map.entry(conversation_id.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let mutex = entry.lock_owned().await;
        let cross_process = self.cross_process_lock().await?;
        Ok(MlsGroupGuard {
            _mutex: mutex,
            _cross_process: cross_process,
        })
    }

    /// A share of the cross-process state lock (#1599) for an MLS-mutating step that has no
    /// per-conversation lock (welcome processing, KeyPackage generation).
    pub async fn cross_process_lock(
        &self,
    ) -> crate::error::Result<crate::notify::lock::SharedStateGuard> {
        Ok(self
            .state_lock
            .acquire(&self.local_data_dir(), crate::notify::lock::APP_WAIT)
            .await?)
    }


    /// Tear down long-lived tasks so the host process can exit. Idempotent.
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

            // The voice room lives in `self.voice.room`; close it too or we linger as a "ghost card" until LiveKit times out.
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

    /// This state's per-user DB directory ([`AppState::local_dir`] under the harness).
    pub fn local_data_dir(&self) -> std::path::PathBuf {
        #[cfg(any(test, feature = "test-harness"))]
        if let Some(dir) = self.local_dir.get() {
            return dir.clone();
        }
        crate::db::local::dirs_path()
    }

    /// Open the per-user database under a caller-supplied DB key.
    ///
    /// Used by `set_pin` and `unlock`; does not touch the keystore.
    ///
    /// Returns whether `mls_kv` was empty; the caller then runs `reset_welcome_delivery` AFTER `ensure_device_cert`.
    pub async fn load_user_db_with_key(
        &self,
        user_id: &str,
        db_key: &[u8],
    ) -> crate::error::Result<bool> {
        let db = LocalDb::open_for_user_in(&self.local_data_dir(), user_id, db_key)?;

        let mls_empty: bool = db.conn()
            .query_row("SELECT COUNT(*) FROM mls_kv", [], |r| r.get::<_, i64>(0))
            .map(|c| c == 0)
            .unwrap_or(true);

        // Best-effort: a failed sweep must never block login.
        if let Err(e) = crate::db::local::evict_old_messages(db.conn()) {
            crate::diag!("[state] startup message eviction failed (non-fatal): {e}");
        }
        crate::commands::messages::loss::prune_known_envelopes(db.conn());

        *self.local_db.lock().await = Some(db);
        // Spawned (#850): indexing scales with the corpus and must not delay sign-in.
        spawn_search_index_maintenance(self.local_db.clone());
        // Per-user media cache: each client's db_key only decrypts its own entries.
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

    /// Record that the DS refused the device that SIGNED the request, while it is still ours.
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
/// #1079: a sealing post must serialize behind any committer on this device, or it picks an undecided epoch.
///
/// Only [`MlsLockHeld::from_guard`] makes one, so a post site that forgets the lock does not compile.
///
/// Borrows the guard so it can't outlive the lock.
pub struct MlsLockHeld<'a> {
    _guard: &'a MlsGroupGuard,
}

impl<'a> MlsLockHeld<'a> {
    pub fn from_guard(guard: &'a MlsGroupGuard) -> Self {
        Self { _guard: guard }
    }
}

/// The per-conversation MLS mutex together with this process's share of the cross-process
/// state lock (#1599). Both release on drop.
pub struct MlsGroupGuard {
    _mutex: tokio::sync::OwnedMutexGuard<()>,
    _cross_process: crate::notify::lock::SharedStateGuard,
}

/// Bring the local full-text search index up to date, off the sign-in path (#850).
///
/// 1. **Auto-repair**: rebuild if FTS `integrity-check` fails (contentless delete needs stable output).
/// 2. **Backfill** in chunks, yielding so the local-DB mutex never blocks a send.
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

    /// The edition-2021 temporary-lifetime footgun, made unrepresentable.
    ///
    /// In edition 2021 a temporary in an `if let`/`match`/`while let` scrutinee lives to the end of the statement, so
    ///
    /// ```ignore
    /// if let Some(id) = state.device_id.lock().await.clone() {
    ///     some_network_round_trip(id).await;   // <- lock STILL HELD
    /// }
    /// ```
    ///
    /// holds the mutex across the await (#875); with a signed `ds_post` that is a self-deadlock.
    ///
    /// Use a `let` statement, whose guard drops at the `;`.
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

    /// Does this line open a scrutinee (or `let x = match …`) on the locking expression?
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

    /// Whether the braced body from `start` contains an `.await`. Brace-counted.
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
