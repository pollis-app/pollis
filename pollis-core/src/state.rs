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
    /// The ONLY database this process opens (#987). `remote_db` and `log_db`
    /// used to sit beside it, each holding a whole-database Turso token baked
    /// into the shipped binary; both are gone, along with `RemoteDb` and the
    /// `libsql` dependency itself. Every remote read is now a signed
    /// `POST /v1/read/...` on the Delivery Service, on the same transport as the
    /// writes — see `commands/ds_reads.rs`.
    pub local_db: Arc<Mutex<Option<LocalDb>>>,
    /// Pluggable secret store. Production wires an [`OsKeystore`]; integration
    /// tests inject an [`InMemoryKeystore`] per simulated client so multiple
    /// users coexist in one test process without sharing session tokens or
    /// account identity keys.
    pub keystore: Arc<dyn Keystore>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    /// #836 identity translations, keyed `(logical_room, wire_identity)` and
    /// valued `(internal_identity, display_name)`.
    ///
    /// LiveKit participant identities are opaque per-room pseudonyms that only
    /// the DS can open, so every peer seen on the SDK event stream costs one
    /// `/v1/livekit/identities` round trip — once. This is that memo.
    ///
    /// It lives in its own lock rather than on `LiveKitState` or `VoiceState` so
    /// the voice and realtime paths, which already hold one of those, never have
    /// to nest two locks to attribute a participant.
    ///
    /// It stores the finished internal identity rather than the DS's answer,
    /// because one entry is not derived at all: the LOCAL participant's, which is
    /// pinned at join to the identity the renderer independently builds from
    /// `get_device_id`. LiveKit reports the local participant's own events (every
    /// push-to-talk `TrackMuted`) under its wire identity, and those have to land
    /// on the local tile.
    ///
    /// Never invalidated: a pseudonym is a deterministic function of the room,
    /// the user and the device, so an entry cannot go stale — it can only stop
    /// being asked about. Bounded by the number of distinct participants seen
    /// this process.
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
    /// Per-device ULID, set during login. Each physical device gets a stable ID
    /// stored in the OS keystore so it survives local DB wipes.
    pub device_id: Arc<Mutex<Option<String>>>,
    /// In-memory ephemeral X25519 private keys for pending device-enrollment
    /// requests. Keyed by enrollment request id. Populated by
    /// `start_device_enrollment` and consumed by `poll_enrollment_status`
    /// when the approver has written back the wrapped account key.
    ///
    /// Stored in memory only — if the app restarts mid-enrollment the user
    /// starts over. The 10-minute request TTL bounds the exposure.
    pub enrollment_ephemeral_keys: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    /// In-memory PIN unlock state. `Some` once the user has entered a
    /// valid PIN (or just set one via `set_pin`); dropped by `lock`.
    /// Not yet load-bearing — stage 6 flips the app over to reading the
    /// unwrapped keys from here instead of the legacy keystore slots.
    pub unlock: Arc<Mutex<Option<UnlockState>>>,
    /// The short-lived OTP-session bearer token minted by the DS `verify-otp`
    /// during first-device signup, held across the bootstrap sequence so the
    /// session-gated `publish-device-cert` (driven later by `set_pin` →
    /// `ensure_device_cert`) can present it. `Some` only between `verify_otp` and
    /// the first cert publish; cleared once the cert lands (the token is then
    /// spent server-side too). Always `None` on the direct (no-DS) path and for
    /// re-login / subsequent-device cert publishes, which stay on the direct
    /// Turso write path. See `docs/otp-server-bootstrap-design.md`.
    pub bootstrap_session: Arc<Mutex<Option<String>>>,
    /// The OTP-session bearer token minted by the DS `verify-otp` on a
    /// **re-login / subsequent-device** (`has_identity`) sign-in, held so the
    /// session-gated writes a NEW device performs *before* it has a signing
    /// credential — `register-device` and the enrollment **request** — can
    /// present it. Deliberately SEPARATE from [`bootstrap_session`]: the
    /// subsequent-device cert publish must NOT consume a session (sibling
    /// approval can outlast the TTL), so it is gated by cert-validity ALONE.
    /// `Some` only between `verify_otp` and the enrollment request; `None` on the
    /// direct (no-DS) path. See `docs/otp-server-bootstrap-design.md` §5.
    pub enrollment_session: Arc<Mutex<Option<String>>>,
    /// Bound port of the loopback media HTTP server, set once during
    /// startup. `None` in test/headless contexts that don't spin up the
    /// server. The port plus `media_server_token` produce the URLs the
    /// frontend embeds in `<img>/<audio>/<video>` `src` attributes.
    pub media_server_port: Arc<Mutex<Option<u16>>>,
    /// Per-session 32-byte hex token for the loopback media server.
    /// Rotated on `unlock` / `set_pin`, cleared on `logout`. Without
    /// this token the server returns 403 — even loopback access is
    /// gated since other local users could otherwise read decrypted
    /// media in flight.
    pub media_server_token: Arc<Mutex<Option<String>>>,
    /// Live in-app terminal sessions, keyed by the id returned from
    /// `terminal_open`. Spawned on first activation, kept for the app's
    /// lifetime; dropping an entry kills + reaps its child shell.
    /// Desktop only — the terminal pane is gated out on mobile targets.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub terminals: Arc<Mutex<HashMap<String, PtySession>>>,
    /// Broadcasts "tear down" to every long-lived background task that
    /// has wired itself up to wait on it. Today the media-server task
    /// listens via `axum::serve(...).with_graceful_shutdown(...)`. A
    /// shell calls `shutdown_signal.notify_waiters()` (via the
    /// `shutdown()` method on AppState) when the process is exiting — the
    /// only caller today is the Tauri shell's quit path in
    /// `src-tauri/src/lib.rs`.
    ///
    /// Without this, the axum task's accept loop pins the Tokio runtime
    /// alive and the host process hangs instead of exiting — the
    /// "Relaunching…" hang #335 fixes, whose symptom was first hit under
    /// the (since-reverted, #386/#389) Electron shell's auto-updater.
    pub shutdown_signal: Arc<Notify>,
    /// Per-conversation locks serializing MLS group mutations within this
    /// process. The MLS sync path (`process_pending_commits`,
    /// `external_join_group`, reconcile) has several independent callers —
    /// `send_message`, channel + DM ingest, the realtime inbox handler, and
    /// device-enrollment finalize. Without serialization two of them can both
    /// observe "no local group", both external-join, and post two distinct
    /// commits at the same epoch, forking the group (prod incident: group
    /// `01KQYX89…`). One lock per `conversation_id` makes the
    /// read-modify-write of an MLS group's local state + its commit-log INSERT
    /// atomic on this device. Cross-device races are caught instead by the
    /// `UNIQUE(conversation_id, epoch)` constraint on `mls_commit_log`.
    pub mls_group_locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
    /// Fan-out of decoded remote screenshare frames (packed I420, the same
    /// `pack_frame_bytes` wire format the legacy Tauri `Channel` carried) to
    /// the loopback media server's `/screenshare/<token>` WebSocket route.
    ///
    /// This is the Electron→Tauri revival path (spike/tauri-revival): the POC
    /// in `actuallydan/rustwebrtc` proved that pushing raw frames over a local
    /// WebSocket into a `<canvas>` WebGL shader sustains 1080p60+ inside
    /// WebKitGTK, where the per-frame Tauri IPC `Channel` (#305 Phase 1) stalled
    /// on V8 GC. `remote_video.rs` publishes here; each WS subscriber gets its
    /// own receiver. Capacity is small and lagged receivers drop old frames —
    /// latest-frame-wins, never back-pressure the decoder.
    pub screenshare_frame_tx: tokio::sync::broadcast::Sender<std::sync::Arc<Vec<u8>>>,
    /// The running closed-overlay relay shim (design §14), or `None` when the
    /// overlay is off (`POLLIS_OVERLAY` unset/`off`). `Some` owns the loopback
    /// SOCKS5 shim task; dropping it (process exit, or a runtime switch to Off)
    /// aborts the accept loop. Control-plane HTTP + the libsql connector consult
    /// [`overlay_handle`](AppState::overlay_handle) to route through the shim;
    /// when `None` every path is byte-for-byte the pre-overlay direct behavior.
    ///
    /// Interior-mutable (and `Arc`-wrapped inside) so the overlay can be started,
    /// stopped, and replaced **at runtime** — `commands::overlay::set_overlay_mode`
    /// swaps this handle live. A `std::sync::Mutex` (not tokio) so the hot-path
    /// read is a cheap uncontended lock + `Arc` clone with no `.await` held;
    /// swaps happen only on a mode change. Off-by-default: `None` at boot until
    /// `apply_overlay_mode` starts a shim for a non-off `POLLIS_OVERLAY`.
    pub overlay: std::sync::Mutex<Option<Arc<pollis_relay::OverlayHandle>>>,
}

impl AppState {
    /// Nothing to connect to any more (#987) — this stays `async` because its
    /// callers await it, and because the overlay is applied AFTERWARDS, by the
    /// shell calling `commands::overlay::apply_overlay_mode(&state,
    /// config.overlay_mode)` on the `Arc`-wrapped state. That keeps boot on the
    /// SAME runtime code path a settings toggle uses (design §14). Off by
    /// default: with the overlay off no shim is ever started.
    pub async fn new(config: Config) -> crate::error::Result<Self> {
        Ok(Self::new_with_parts(
            config,
            keystore::default_os_keystore(),
        ))
    }

    /// Build AppState from pre-constructed parts. Production should use
    /// [`AppState::new`]; tests use this to inject an [`InMemoryKeystore`] so
    /// several simulated clients can coexist in one process.
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
            device_id: Arc::new(Mutex::new(None)),
            enrollment_ephemeral_keys: Arc::new(Mutex::new(HashMap::new())),
            unlock: Arc::new(Mutex::new(None)),
            bootstrap_session: Arc::new(Mutex::new(None)),
            enrollment_session: Arc::new(Mutex::new(None)),
            media_server_port: Arc::new(Mutex::new(None)),
            media_server_token: Arc::new(Mutex::new(None)),
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            terminals: Arc::new(Mutex::new(HashMap::new())),
            shutdown_signal: Arc::new(Notify::new()),
            mls_group_locks: Arc::new(Mutex::new(HashMap::new())),
            // Receiver dropped immediately; subscribers are created per
            // WebSocket connection via `screenshare_frame_tx.subscribe()`.
            screenshare_frame_tx: tokio::sync::broadcast::channel(8).0,
            // Overlay off until `apply_overlay_mode` starts a shim (boot honoring
            // `POLLIS_OVERLAY`, or a runtime `set_overlay_mode`). Test harness /
            // `new_with_parts` callers stay direct unless a test applies a mode.
            overlay: std::sync::Mutex::new(None),
        }
    }

    /// The overlay shim handle currently in force, or `None` when the overlay is
    /// off. Cheap hot-path read: an uncontended lock plus an `Arc` clone (no
    /// `.await` held). Every control-plane HTTP caller and the libsql connector
    /// go through this so a runtime `set_overlay_mode` is picked up immediately.
    pub fn overlay_handle(&self) -> Option<Arc<pollis_relay::OverlayHandle>> {
        self.overlay.lock().unwrap().clone()
    }

    /// Acquire the per-conversation MLS lock. The returned guard must be held
    /// for the full read-modify-write of the group's local MLS state plus its
    /// `mls_commit_log` INSERT, so concurrent callers on this device can't race
    /// into a commit fork. The lock is keyed by `conversation_id` (group_id for
    /// channels, conversation_id for DMs — the same id used as the MLS group
    /// id). Non-reentrant: a caller already holding this lock must call the
    /// `*_locked` helper variants rather than re-acquiring.
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

    /// Tear down long-lived background tasks so the host process can
    /// exit cleanly. Idempotent — safe to call more than once and from
    /// more than one exit path, because a quit and an update-relaunch can
    /// both funnel through the same teardown.
    ///
    /// Steps, in order:
    /// 1. Signal `shutdown_signal` so the axum media server hits its
    ///    graceful-shutdown branch and stops accepting new connections.
    /// 2. Drain LiveKit room handles — close each room cleanly so the
    ///    SFU sees a normal disconnect, then abort the per-room task so
    ///    its event-loop holder is released.
    ///
    /// Voice / screenshare / terminals are NOT touched here:
    /// * Voice cleanup happens via `leave_voice_channel`, which the
    ///   `UpdateScreen` renderer already invokes before triggering an
    ///   install (and is a no-op when no voice session is active).
    /// * Terminal PtySessions die naturally when their child shell
    ///   receives SIGTERM during process exit; nothing extra to do.
    pub async fn shutdown(&self) {
        self.shutdown_signal.notify_waiters();

        #[cfg(feature = "media")]
        {
            let mut guard = self.livekit.lock().await;
            let rooms = std::mem::take(&mut guard.rooms);
            drop(guard);
            for (room_id, (room, handle)) in rooms {
                if let Err(e) = room.close().await {
                    eprintln!("[shutdown] livekit room {room_id} close failed: {e}");
                }
                handle.abort();
            }

            // The voice room is held separately in `self.voice.room`, not in the
            // `livekit.rooms` map above — so close it here too. Without this, a
            // graceful quit (Cmd+Q / window close → pollis-node shutdown) leaves
            // our participant in the room until LiveKit's server-side RTC timeout
            // evicts us, which is the lingering "ghost card" other members see.
            // Mirror `leave_voice_channel`: abort the room/frame tasks (so the
            // Disconnected handler can't race the teardown), take the room, then
            // close it outside the lock with a timeout so a dead connection can't
            // stall the quit.
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

    /// Open the per-user database under a caller-supplied DB key.
    ///
    /// Used by `set_pin` (after wrapping freshly generated bytes) and
    /// `unlock` (after unwrapping the PIN-protected blob). Does not touch
    /// the keystore — the caller is responsible for sourcing `db_key`
    /// from `AppState.unlock`.
    ///
    /// Returns whether `mls_kv` was empty (freshly-created or wiped local DB).
    /// The caller runs `reset_welcome_delivery` on `true`, AFTER
    /// `ensure_device_cert` republishes this device's key — both so the signed
    /// DS reset authenticates and so the check isn't fooled by
    /// `ensure_device_cert` itself writing the device signer into `mls_kv`.
    pub async fn load_user_db_with_key(
        &self,
        user_id: &str,
        db_key: &[u8],
    ) -> crate::error::Result<bool> {
        let db = LocalDb::open_for_user(user_id, db_key)?;

        // mls_kv empty ⇒ the local DB was freshly created or wiped (e.g. a
        // schema-version bump deleted and recreated the file). Reported to the
        // caller, which re-arms welcome delivery (reset_welcome_delivery) after
        // ensure_device_cert so poll_mls_welcomes restores group memberships.
        let mls_empty: bool = db.conn()
            .query_row("SELECT COUNT(*) FROM mls_kv", [], |r| r.get::<_, i64>(0))
            .map(|c| c == 0)
            .unwrap_or(true);

        // Bounded local history: sweep messages older than the device-local
        // retention window now that the DB is ready. Best-effort — a failed
        // sweep must never block login.
        if let Err(e) = crate::db::local::evict_old_messages(db.conn()) {
            eprintln!("[state] startup message eviction failed (non-fatal): {e}");
        }

        *self.local_db.lock().await = Some(db);
        // Scope the media cache to this user. Two clients on the same machine
        // (dev workflow) otherwise share `app_data_dir/media-cache` but each
        // has its own db_key, so client B can't decrypt client A's cache
        // entries and the media server returns 500.
        crate::commands::r2::set_cache_user(Some(user_id));
        Ok(mls_empty)
    }

    /// Close the current user's database (called on logout).
    pub async fn unload_user_db(&self) {
        *self.local_db.lock().await = None;
        crate::commands::r2::set_cache_user(None);
    }

    pub fn check_not_outdated(&self) -> crate::error::Result<()> {
        if self.update_required.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::error::Error::ClientOutdated);
        }
        Ok(())
    }
}

#[cfg(test)]
mod guard_lifetime_tests {
    use std::path::{Path, PathBuf};

    /// The edition-2021 temporary-lifetime footgun, made unrepresentable.
    ///
    /// In edition 2021 a temporary created in the SCRUTINEE of `if let` /
    /// `match` / `while let` — or on the right-hand side of a `let` — lives
    /// until the end of the whole statement, body included. So
    ///
    /// ```ignore
    /// if let Some(id) = state.device_id.lock().await.clone() {
    ///     some_network_round_trip(id).await;   // <- lock STILL HELD
    /// }
    /// ```
    ///
    /// holds the mutex across the await even though `.clone()` reads as though
    /// it released it. #875 found this on both `pin::set_pin` and `pin::unlock`,
    /// pinning `state.device_id` — one of the hottest shared mutexes in the
    /// crate — across a Turso read, an OS-keystore signature and a DS POST.
    /// Nothing deadlocked, because the callee happened to use the sign-free
    /// bootstrap posts; the signed `ds_client::ds_post` takes that same
    /// non-reentrant `tokio::sync::Mutex`, so the shape was one ordinary
    /// refactor away from a self-deadlock with no compiler diagnostic.
    ///
    /// The correct shape is a `let` STATEMENT — `let id = ...lock().await.clone();`
    /// — whose guard drops at the `;`. Every other lock read in the crate
    /// already uses it. This test is the invariant: the scrutinee form is
    /// allowed only when nothing in the body awaits.
    ///
    /// A guard, not a proof: it is a source-shape check, so it cannot see a
    /// guard bound to a `let` and then held deliberately. It closes the one
    /// case where the source LOOKS correct and is not.
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
    /// the locking expression? Also matches the `let x = match …` form, whose
    /// temporary lives just as long.
    fn is_scrutinee(trimmed: &str) -> bool {
        let rest = trimmed
            .strip_prefix('}')
            .map(str::trim_start)
            .unwrap_or(trimmed);
        let rest = match rest.find('=') {
            // `let x = match …` / `let x = if let …`
            Some(eq) if rest.starts_with("let ") => rest[eq + 1..].trim_start(),
            _ => rest,
        };
        rest.starts_with("if let ") || rest.starts_with("match ") || rest.starts_with("while let ")
    }

    /// Whether the braced body opened at or after `start` contains an `.await`.
    /// Brace-counted rather than parsed — good enough because the only thing it
    /// has to get right is "where does this statement end".
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
