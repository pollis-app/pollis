//! `user_device.last_seen` — "when did this device last talk to the DS".
//!
//! The column is user-visible metadata about the user's OWN devices (Settings →
//! Devices, served by `account_reads::devices`). It used to be touched by the
//! client's direct remote write on every login; #987 moved every remote write
//! behind the DS and dropped that touch, which left `apply_register_device`
//! (hit only when a brand-new device registers) as the only writer, so every
//! device's "last seen" froze at its creation time.
//!
//! It is now bumped here, by the DS, from the one place a device proves it is
//! alive: a request whose device signature VERIFIED (`auth::verify_request_inner`,
//! the chokepoint every device-signed endpoint funnels through). Nothing else is
//! recorded — no IP, no user agent, no endpoint — and the precision is the
//! column's existing `datetime('now')` (seconds).
//!
//! ## Throttle
//!
//! A write per signed request would multiply the DS's Turso write volume, so a
//! device is bumped at most once per [`LAST_SEEN_BUMP_INTERVAL_SECS`]. The
//! throttle is an in-process map `(user_id, device_id) -> unix seconds of the
//! last bump`. That is sound because the DS serves every request from ONE
//! process (prod: `max_instances: 1` in `pollis-delivery/wrangler.prod.jsonc`;
//! dev's cap of 2 only tolerates an idle orphan, every request is routed to the
//! one `DS_SINGLETON_NAME` object) — the same assumption
//! [`crate::auth::DeviceKeyCache`] rests on. Unlike that cache, nothing here is
//! security-relevant if the assumption ever breaks: N instances would at worst
//! bump N times per interval, never write a wrong value. A restart forgets the
//! map, which costs one extra write per active device — harmless.
//!
//! The write is spawned off the request's critical path: it never delays the
//! request, and a failure is logged and never fails it. A failed write releases
//! its throttle slot so the device's next request retries.
//!
//! ## Binding
//!
//! The UPDATE is keyed on BOTH halves of the verified identity and skips revoked
//! rows (`WHERE device_id = ? AND user_id = ? AND revoked_at IS NULL`). Since the
//! identity comes from the verified signature, a caller can only ever bump its
//! own live device; the `revoked_at` guard also covers a revoke that lands
//! between the verification and the spawned write.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use libsql::Connection;

use crate::db::Db;

/// Minimum spacing between two `last_seen` writes for one device, in seconds.
/// 15 minutes: far finer than anything the Devices list displays meaningfully,
/// and coarse enough that a busy device costs a handful of writes a day.
pub const LAST_SEEN_BUMP_INTERVAL_SECS: i64 = 15 * 60;

/// Soft cap on tracked devices. Entries are only created for devices whose
/// signature verified, so the map is bounded by the live fleet; the cap only
/// turns a pathological case into extra writes rather than unbounded memory.
const MAX_ENTRIES: usize = 10_000;

#[derive(Default)]
struct Inner {
    /// `(user_id, device_id)` -> server unix seconds of the last bump claimed.
    map: HashMap<(String, String), i64>,
    /// Bumps claimed (i.e. writes spawned) since boot. Observability for tests.
    writes: u64,
}

/// The in-process `last_seen` throttle. `Clone` is shallow (shared `Arc`), so it
/// rides inside the `Clone` [`crate::auth::DeviceKeyCache`] and every request
/// sees the same map.
#[derive(Clone, Default)]
pub struct LastSeenThrottle {
    inner: Arc<Mutex<Inner>>,
}

impl LastSeenThrottle {
    /// Decide whether `(user_id, device_id)` is due a bump at `now`, and claim
    /// the slot if so. A stamp in the FUTURE (the clock went backwards) counts
    /// as due, so a clock jump can never freeze a device's `last_seen`.
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

    /// Undo a claim whose write failed, so the next request retries — unless a
    /// later claim has already replaced it.
    fn release(&self, user_id: &str, device_id: &str, claimed_at: i64) {
        let mut guard = self.inner.lock().expect("last_seen throttle mutex poisoned");
        let key = (user_id.to_string(), device_id.to_string());
        if guard.map.get(&key) == Some(&claimed_at) {
            guard.map.remove(&key);
        }
    }

    /// Record that `(user_id, device_id)` — an identity whose signature has just
    /// VERIFIED — was seen at `now`. Spawns the `last_seen` write when the
    /// device is due one; returns immediately either way. Must be called from
    /// within a Tokio runtime (every request handler is).
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

    /// How many `last_seen` writes have been claimed (spawned) since boot.
    /// Test/telemetry observability only.
    pub fn writes(&self) -> u64 {
        self.inner.lock().expect("last_seen throttle mutex poisoned").writes
    }

    /// Forget every throttle stamp of `user_id`, so their devices' next verified
    /// request bumps again. Test seam (the flows suite backdates rows and cannot
    /// wait the interval out); never called on the request path.
    pub fn forget_user(&self, user_id: &str) {
        self.inner
            .lock()
            .expect("last_seen throttle mutex poisoned")
            .map
            .retain(|(u, _), _| u != user_id);
    }
}

/// Set `last_seen = now` on the caller's OWN live device row. Bound to both
/// halves of the verified identity, and never touches a revoked device. Returns
/// the number of rows changed (0 or 1).
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
