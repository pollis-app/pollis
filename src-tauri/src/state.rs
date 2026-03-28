use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::db::{local::LocalDb, remote::RemoteDb};
use crate::keystore;
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
    pub update_required: Arc<AtomicBool>,
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
            update_required: Arc::new(AtomicBool::new(false)),
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
