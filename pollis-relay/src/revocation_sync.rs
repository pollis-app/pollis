//! Keeping a relay node's [`RevocationStore`] loaded (#813 Phase C, relay half).
//!
//! The store is pure — bytes in, verdict out — so something has to put bytes in
//! it. On a deployed node that is this task: fetch the signed `revocations.json`
//! the reconciler publishes, hand it to [`RevocationStore::install`], repeat.
//!
//! # Why a loop is allowed here
//!
//! `CLAUDE.md` bans periodic polling — but that rule is about the *app*: a
//! renderer keepalive burns a user's battery and radio to learn nothing. This is
//! a server daemon whose whole job is to hold fresh evidence, and the artifact it
//! holds carries a **5-minute TTL** that is enforced at *use* time. There is no
//! event to be driven by; the only alternatives to a refresh are holding stale
//! evidence (forbidden — `admit` rejects it) or refusing every extend.
//!
//! The cadence is [`REFRESH_INTERVAL`] (60s) against a 5-minute TTL re-signed
//! every 2 minutes, i.e. ~5× margin, with jitter so a pool of nodes does not
//! stampede the CDN in lockstep. Failures back off exponentially from
//! [`MIN_BACKOFF`] to [`MAX_BACKOFF`] rather than hammering — the `with_retry`
//! shape the repo asks for.
//!
//! **A missed refresh is safe by construction.** The store enforces expiry when
//! `admit` is called, not when a list is installed, so a wedged fetch degrades to
//! "this node stops being a middle hop" — never to "this node keeps trusting a
//! stale list". That is why this task never needs to be reliable, only usually
//! working.
//!
//! # What it does not do
//!
//! It fetches over the **direct** HTTP client. A relay must not route its own
//! control traffic through the overlay: it would be dialling its own tier, and a
//! revocation list fetched through a relay that may itself be revoked is circular.
//! It also stores nothing on disk — the store is in-memory, and a restarted node
//! simply refuses to extend until its first successful fetch (§5.2, §11.7).

use std::future::Future;
use std::time::Duration;

use crate::policy::RevocationStore;
use crate::proto;

/// Steady-state refresh cadence. See the module docs for why this is 60s.
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

/// Deterministic ±12.5% jitter around `base`, derived from the process's own
/// randomness so two nodes started together do not stay in lockstep.
fn jittered(base: Duration) -> Duration {
    let mut byte = [0u8; 1];
    if getrandom::getrandom(&mut byte).is_err() {
        return base;
    }
    // byte/255 maps to [0, 1]; shift to [-0.125, +0.125] of the base.
    let spread = base.as_millis() as i64 / 4;
    let offset = (i64::from(byte[0]) * spread / 255) - (spread / 2);
    let millis = (base.as_millis() as i64 + offset).max(1) as u64;
    Duration::from_millis(millis)
}

/// The next delay after `failures` consecutive failures: exponential from
/// [`MIN_BACKOFF`], clamped at [`MAX_BACKOFF`]. Pure, so the schedule is testable
/// without waiting for it.
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
/// Returns the accepted sequence number. Every failure mode — transport, HTTP
/// status, oversize body, bad signature, expired, rolled back — is an `Err`, and
/// none of them mutate the store's held list.
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
    // `directory_seq_floor` is 0 here: this node has no signed directory of its
    // own to anchor against (the directory is a CLIENT artifact — the relay is
    // listed in it, it does not consume it). The store's own high-water mark
    // still provides rollback protection across refreshes.
    let seq = store.install(&bytes, proto::now_unix(), 0)?;
    Ok(seq)
}

/// Run the refresh loop until `shutdown` resolves.
///
/// Returns immediately (spawning nothing) if `store` cannot evaluate revocation
/// at all — there is nothing to install into an unconfigured store, and it is
/// already in its strictest state.
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
                    // Not an error-level event on its own: the store fails closed
                    // by itself, so the consequence is a node that stops
                    // extending, not a node that trusts something stale.
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
