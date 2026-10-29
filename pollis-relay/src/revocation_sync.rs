//! Keeps a relay node's [`RevocationStore`] loaded from the signed
//! `revocations.json` (#813).
//!
//! A loop is allowed here (the polling ban is about the app): this daemon holds
//! evidence with a 5-minute TTL enforced at use time, and there is no event to
//! drive it. 60s cadence, jittered, exponential backoff on failure.
//!
//! A missed refresh is safe: expiry is checked in `admit`, so a wedged fetch
//! means "stops being a middle hop", never "trusts a stale list". Fetches go
//! direct, never via the overlay (circular trust). Nothing is stored on disk; a
//! restarted node refuses to extend until its first fetch (§5.2, §11.7).

use std::future::Future;
use std::time::Duration;

use crate::backoff::jittered;
use crate::policy::RevocationStore;
use crate::proto;

/// Steady-state cadence: ~5x margin on a 5-minute TTL re-signed every 2 minutes.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

pub const MIN_BACKOFF: Duration = Duration::from_secs(2);

pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// The list is a few KB; this bounds a hostile origin's body.
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

/// The next delay after `failures` consecutive failures: exponential from
/// [`MIN_BACKOFF`], clamped at [`MAX_BACKOFF`].
pub fn backoff_after(failures: u32) -> Duration {
    if failures == 0 {
        return REFRESH_INTERVAL;
    }
    let shift = (failures - 1).min(16);
    let millis = MIN_BACKOFF
        .as_millis()
        .saturating_mul(1u128 << shift)
        .min(MAX_BACKOFF.as_millis());
    Duration::from_millis(millis as u64)
}

/// Fetch and install the signed revocation list once; returns the accepted
/// sequence. No failure mutates the store's held list.
pub async fn refresh_once(store: &RevocationStore, url: &str) -> anyhow::Result<u64> {
    let client = crate::http::http_client(None);
    let response = client
        .get(url)
        .timeout(FETCH_TIMEOUT)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("fetch {url}: {e}"))?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!("fetch {url}: HTTP {}", response.status()));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|e| anyhow::anyhow!("read {url}: {e}"))?;
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(anyhow::anyhow!(
            "revocation list from {url} is {} bytes, over the {MAX_ARTIFACT_BYTES} cap",
            bytes.len()
        ));
    }
    // Floor 0: a relay consumes no directory. The store's own high-water mark
    // still prevents rollback.
    let seq = store.install(&bytes, proto::now_unix(), 0)?;
    Ok(seq)
}

/// Run the refresh loop until `shutdown` resolves. Spawns nothing for an
/// unconfigured store, which is already in its strictest state.
pub fn spawn<F>(
    store: RevocationStore,
    url: String,
    shutdown: F,
) -> Option<tokio::task::JoinHandle<()>>
where
    F: Future<Output = ()> + Send + 'static,
{
    if !store.is_configured() {
        tracing::warn!(
            "revocation refresh not started: no pinned directory key — this node will refuse to extend circuits"
        );
        return None;
    }
    Some(tokio::spawn(async move {
        tokio::pin!(shutdown);
        let mut failures: u32 = 0;
        loop {
            match refresh_once(&store, &url).await {
                Ok(seq) => {
                    if failures > 0 {
                        tracing::info!("revocation list recovered at seq {seq}");
                    } else {
                        tracing::debug!("revocation list refreshed at seq {seq}");
                    }
                    failures = 0;
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    // Warn, not error: the store fails closed by itself.
                    tracing::warn!("revocation refresh failed ({failures} in a row): {e}");
                }
            }
            let delay = jittered(backoff_after(failures));
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    break;
                }
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_climbs_from_the_floor_to_the_ceiling_and_resets() {
        assert_eq!(backoff_after(0), REFRESH_INTERVAL);
        assert_eq!(backoff_after(1), MIN_BACKOFF);
        assert!(backoff_after(2) > backoff_after(1));
        assert!(backoff_after(3) > backoff_after(2));
        assert_eq!(backoff_after(50), MAX_BACKOFF);
        assert!(backoff_after(u32::MAX) <= MAX_BACKOFF);
        for failures in 1..20 {
            assert!(backoff_after(failures) <= REFRESH_INTERVAL);
        }
    }

    #[test]
    fn jitter_stays_within_an_eighth_of_the_base() {
        for _ in 0..64 {
            let d = jittered(REFRESH_INTERVAL);
            assert!(d >= REFRESH_INTERVAL * 7 / 8, "{d:?} too short");
            assert!(d <= REFRESH_INTERVAL * 9 / 8, "{d:?} too long");
        }
    }

    #[tokio::test]
    async fn an_unconfigured_store_starts_no_refresh_task() {
        let handle = spawn(
            RevocationStore::unconfigured(),
            "https://example.invalid/revocations.json".to_string(),
            std::future::pending::<()>(),
        );
        assert!(handle.is_none());
    }
}
