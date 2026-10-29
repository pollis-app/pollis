//! `user_device.last_seen`: bumped from `auth::verify_request_inner` on a
//! request whose device signature verified. Nothing else is recorded (no IP,
//! user agent or endpoint).
//!
//! Throttled to one write per device per [`LAST_SEEN_BUMP_INTERVAL_SECS`] with
//! an in-process map, which is sound because the DS is one process (the same
//! assumption [`crate::auth::DeviceKeyCache`] rests on); if that broke, N
//! instances would only bump N times per interval. The write is spawned off the
//! request path; a failure releases the throttle slot so the next request
//! retries. The UPDATE is keyed on both halves of the verified identity and
//! skips revoked rows, which also covers a revoke racing the spawned write.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use libsql::Connection;

use crate::db::Db;

/// Minimum spacing between two `last_seen` writes for one device, in seconds.
pub const LAST_SEEN_BUMP_INTERVAL_SECS: i64 = 15 * 60;

/// Soft cap on tracked devices: a pathological case costs extra writes, not
/// unbounded memory.
const MAX_ENTRIES: usize = 10_000;

#[derive(Default)]
struct Inner {
    /// `(user_id, device_id)` -> unix seconds of the last bump claimed.
    map: HashMap<(String, String), i64>,
    /// Writes spawned since boot, for tests.
    writes: u64,
}

/// The in-process `last_seen` throttle. `Clone` shares the map.
#[derive(Clone, Default)]
pub struct LastSeenThrottle {
    inner: Arc<Mutex<Inner>>,
}

impl LastSeenThrottle {
    /// Claim a bump slot if due. A stamp in the future (clock went backwards)
    /// counts as due, so a clock jump can never freeze `last_seen`.
    fn claim(&self, user_id: &str, device_id: &str, now: i64) -> bool {
        let mut guard = self.inner.lock().expect("last_seen throttle mutex poisoned");
        let key = (user_id.to_string(), device_id.to_string());
        if let Some(&at) = guard.map.get(&key) {
            if (0..LAST_SEEN_BUMP_INTERVAL_SECS).contains(&(now - at)) {
                return false;
            }
        }
        if guard.map.len() >= MAX_ENTRIES && !guard.map.contains_key(&key) {
            guard
                .map
                .retain(|_, at| (0..LAST_SEEN_BUMP_INTERVAL_SECS).contains(&(now - *at)));
            if guard.map.len() >= MAX_ENTRIES {
                guard.map.clear();
            }
        }
        guard.map.insert(key, now);
        guard.writes += 1;
        true
    }

    /// Undo a failed write's claim, unless a later claim already replaced it.
    fn release(&self, user_id: &str, device_id: &str, claimed_at: i64) {
        let mut guard = self.inner.lock().expect("last_seen throttle mutex poisoned");
        let key = (user_id.to_string(), device_id.to_string());
        if guard.map.get(&key) == Some(&claimed_at) {
            guard.map.remove(&key);
        }
    }

    /// Record that a just-verified identity was seen at `now`, spawning the
    /// write when due. Must be called within a Tokio runtime.
    pub fn note(&self, db: &Db, user_id: &str, device_id: &str, now: i64) {
        if !self.claim(user_id, device_id, now) {
            return;
        }
        let db = db.clone();
        let throttle = self.clone();
        let user_id = user_id.to_string();
        let device_id = device_id.to_string();
        tokio::spawn(async move {
            let result = async {
                let conn = db.conn().await?;
                touch_last_seen(&conn, &user_id, &device_id).await
            }
            .await;
            if let Err(e) = result {
                crate::error::note_transient(&e);
                tracing::warn!("last_seen bump failed: {e:#}");
                throttle.release(&user_id, &device_id, now);
            }
        });
    }

    /// `last_seen` writes spawned since boot (test observability).
    pub fn writes(&self) -> u64 {
        self.inner.lock().expect("last_seen throttle mutex poisoned").writes
    }

    /// Forget `user_id`'s throttle stamps. Test seam; never called on the
    /// request path.
    pub fn forget_user(&self, user_id: &str) {
        self.inner
            .lock()
            .expect("last_seen throttle mutex poisoned")
            .map
            .retain(|(u, _), _| u != user_id);
    }
}

/// Set `last_seen = now` on the caller's own live device row. Returns rows
/// changed (0 or 1).
pub async fn touch_last_seen(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<u64> {
    let changed = conn
        .execute(
            "UPDATE user_device SET last_seen = datetime('now') \
             WHERE device_id = ?1 AND user_id = ?2 AND revoked_at IS NULL",
            libsql::params![device_id, user_id],
        )
        .await?;
    Ok(changed)
}
