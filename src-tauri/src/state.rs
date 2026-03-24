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
    pub local_db: Arc<Mutex<LocalDb>>,
    pub remote_db: Arc<RemoteDb>,
    pub otp_store: Arc<Mutex<HashMap<String, OtpEntry>>>,
    pub livekit: Arc<Mutex<LiveKitState>>,
    pub update_required: Arc<AtomicBool>,
}

impl AppState {
    pub async fn new(config: Config) -> crate::error::Result<Self> {
        // Load or generate the local DB encryption key from the OS keystore.
        let db_key = match keystore::load("local_db_key").await? {
            Some(k) => k,
            None => {
                let key: Vec<u8> = (0..32).map(|_| rand::random::<u8>()).collect();
                keystore::store("local_db_key", &key).await?;
                key
            }
        };
        let local_db = LocalDb::open(&db_key)?;
        let remote_db = RemoteDb::connect(&config.turso_url, &config.turso_token).await?;

        Ok(Self {
            config,
            local_db: Arc::new(Mutex::new(local_db)),
            remote_db: Arc::new(remote_db),
            otp_store: Arc::new(Mutex::new(HashMap::new())),
            livekit: Arc::new(Mutex::new(LiveKitState::new())),
            update_required: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn check_not_outdated(&self) -> crate::error::Result<()> {
        if self.update_required.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(crate::error::Error::ClientOutdated);
        }
        Ok(())
    }
}
