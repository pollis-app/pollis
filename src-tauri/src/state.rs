use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::db::{local::LocalDb, remote::RemoteDb};
use crate::keystore;
use crate::commands::voice::VoiceState;
use crate::realtime::LiveKitState;

#[derive(Clone)]
pub struct OtpEntry {
    pub hash: String,
    pub expires_at: u64,
}

pub struct AppState {
    pub config: Config,
    /// None until a user logs in. Opened per-user as pollis_{user_id}.db.
    pub local_db: Arc<Mutex<Option<LocalDb>>>,
    pub remote_db: Arc<RemoteDb>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    pub voice: Arc<Mutex<VoiceState>>,
    pub update_required: Arc<AtomicBool>,
    /// Per-device ULID, set during login. Each physical device gets a stable ID
    /// stored in the OS keystore so it survives local DB wipes.
    pub device_id: Arc<Mutex<Option<String>>>,
}

impl AppState {
    pub async fn new(config: Config) -> crate::error::Result<Self> {
        let remote_db = RemoteDb::connect(&config.turso_url, &config.turso_token).await?;

        Ok(Self {
            config,
            local_db: Arc::new(Mutex::new(None)),
            remote_db: Arc::new(remote_db),
            otp_store: Arc::new(Mutex::new(HashMap::new())),
            livekit: Arc::new(Mutex::new(LiveKitState::new())),
            voice: Arc::new(Mutex::new(VoiceState::new())),
            update_required: Arc::new(AtomicBool::new(false)),
            device_id: Arc::new(Mutex::new(None)),
        })
    }

    /// Generate (or load) the per-user DB key and open their database.
    pub async fn load_user_db(&self, user_id: &str) -> crate::error::Result<()> {
        let db_key = match keystore::load_for_user("db_key", user_id).await? {
            Some(k) => k,
            None => {
                let key: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
                keystore::store_for_user("db_key", user_id, &key).await?;
                key
            }
        };
        let db = LocalDb::open_for_user(user_id, &db_key)?;

        // If mls_kv is empty the local DB was freshly created or wiped (e.g. a
        // schema-version bump deleted and recreated the file).  Reset welcome
        // delivery markers so poll_mls_welcomes (called from initialize_identity)
        // will re-process them and restore all MLS group memberships.
        let mls_empty: bool = db.conn()
            .query_row("SELECT COUNT(*) FROM mls_kv", [], |r| r.get::<_, i64>(0))
            .map(|c| c == 0)
            .unwrap_or(true);

        if mls_empty {
            // Load the device_id from keystore (if it exists yet) to scope the
            // reset to THIS device only.  Without scoping, resetting all of a
            // user's welcomes would cause other devices to re-process welcomes
            // and destroy their working MLS group state.
            let maybe_device_id = keystore::load_for_user("device_id", user_id).await
                .ok()
                .flatten()
                .and_then(|b| String::from_utf8(b).ok());

            match self.remote_db.conn().await {
                Ok(conn) => {
                    if let Some(ref did) = maybe_device_id {
                        let _ = conn.execute(
                            "UPDATE mls_welcome SET delivered = 0 \
                             WHERE recipient_id = ?1 AND (recipient_device_id = ?2 OR recipient_device_id IS NULL)",
                            libsql::params![user_id, did.clone()],
                        ).await;
                    } else {
                        // First login ever (no device_id yet) — safe to reset all
                        // since no other device can exist for this user yet.
                        let _ = conn.execute(
                            "UPDATE mls_welcome SET delivered = 0 WHERE recipient_id = ?1",
                            libsql::params![user_id],
                        ).await;
                    }
                }
                Err(e) => {
                    eprintln!("[state] load_user_db: failed to reset mls_welcome (non-fatal): {e}");
                }
            }
        }

        *self.local_db.lock().await = Some(db);
        Ok(())
    }

    /// Close the current user's database (called on logout).
    pub async fn unload_user_db(&self) {
        *self.local_db.lock().await = None;
    }

    pub fn check_not_outdated(&self) -> crate::error::Result<()> {
        if self.update_required.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::error::Error::ClientOutdated);
        }
        Ok(())
    }
}
