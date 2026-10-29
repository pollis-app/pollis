//! Keeping a relay node's [`RevocationStore`] loaded (#813 Phase C, relay half).
//!
//! Fetch the signed `revocations.json`, [`RevocationStore::install`] it, repeat.
//!
//! # Why a loop is allowed here
//!
//! A server daemon holding evidence with a 5-minute TTL enforced at use time; no event exists to drive it.
//!
//! [`REFRESH_INTERVAL`] gives ~5× margin, with jitter; failures back off exponentially.
//!
//! **A missed refresh is safe by construction:** expiry is enforced at `admit`, so a wedged fetch just stops extends.
//!
//! # What it does not do
//!
//! It fetches directly (never through the overlay) and stores nothing on disk.

use std::future::Future;
use std::time::Duration;

use crate::backoff::jittered;
use crate::policy::RevocationStore;
use crate::proto;

/// Steady-state refresh cadence.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// First retry delay after a failed fetch or a rejected list.
pub const MIN_BACKOFF: Duration = Duration::from_secs(2);

/// Ceiling on the retry delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// How long a single fetch may take before it is abandoned.
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Cap on the artifact size we will read. The published list is a few KB; this
/// stops a hostile or misconfigured origin from feeding a node an unbounded body.
const MAX_ARTIFACT_BYTES: usize = 1024 * 1024;

/// Exponential from [`MIN_BACKOFF`], clamped at [`MAX_BACKOFF`].
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

/// Fetch the signed revocation list once and install it.
///
/// Returns the accepted sequence number; no failure mutates the held list.
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
    // Floor 0: relays do not consume the directory; the store's high-water mark still blocks rollback.
    let seq = store.install(&bytes, proto::now_unix(), 0)?;
    Ok(seq)
}

/// Run the refresh loop until `shutdown` resolves.
///
/// Returns immediately if `store` is unconfigured (already strictest).
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
                    // Warn only: the store fails closed by itself.
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
        // It is a ceiling, not a ramp that runs away.
        assert_eq!(backoff_after(50), MAX_BACKOFF);
        assert!(backoff_after(u32::MAX) <= MAX_BACKOFF);
        // Every retry delay is shorter than the steady-state cadence, so a
        // recovering node re-arms quickly rather than sitting out a full minute.
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
