//! Turning consent into an actually-reachable device: read the signed
//! directory, keep the peer's own revocation evidence current, and park a
//! connection at every first-party relay it lists.
//!
//! The join between [`super::park`] and [`super::serving`], and the only part
//! of peer relaying that does network I/O on a schedule:
//!
//! - the directory decides where to park and, installed into the engine's
//!   [`NextHopDirectory`], where it may extend to;
//! - the revocation list decides whether this peer may forward at all;
//!   `serve_extend` refuses without one, and expiry is checked at use time, so
//!   a missed refresh means "not forwarding", never "trusting a stale list".
//!
//! The loop sleeps on the directory's `expires_at`, capped by the revocation
//! TTL: renewing a short-lived signed artifact, not a keepalive poll, and gone
//! the moment consent is withdrawn. Without a directory the device parks
//! nowhere (`Waiting{NoInboundPath}`); without a usable list it parks but
//! forwards nothing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pollis_relay::nexthop::NextHopDirectory;
use pollis_relay::policy::RevocationStore;
use pollis_relay::CertificateDer;
use tokio::task::JoinHandle;

use super::context::ServingContext;
use super::park::{LinkAcceptor, ParkedRelay, ParkingSupervisor};
use crate::net::directory;

/// Refresh the directory this long before it expires.
const DIRECTORY_REFRESH_SKEW: Duration = Duration::from_secs(5 * 60);

/// Ceiling on the sleep between refreshes: the revocation list is re-signed
/// every ~2 minutes with a 5-minute TTL, so a minute leaves ~5× margin.
const REVOCATION_REFRESH: Duration = Duration::from_secs(60);

/// Floor on the sleep, so a pathological `expires_at` cannot tight-loop.
const MIN_REFRESH: Duration = Duration::from_secs(15);

/// Fixed backoff after a failed refresh.
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// Owns the refresh loop and, through it, every parked connection. Dropping it
/// closes them all: withdrawing consent needs no flag or shutdown handshake.
pub struct Reachability {
    task: JoinHandle<()>,
}

impl Drop for Reachability {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub struct ReachabilityConfig {
    pub context: Arc<dyn ServingContext>,
    /// The running engine's revocation store (same cell), so a list installed
    /// here reaches every in-flight `Extend`.
    pub revocations: RevocationStore,
    /// The engine's next-hop directory, installed from the same verified bytes.
    pub next_hops: NextHopDirectory,
    /// This device's relay leaf, the `Park` frame's payload.
    pub leaf_der: CertificateDer<'static>,
    pub acceptor: Arc<dyn LinkAcceptor>,
    /// Called with the live parked-connection count whenever it changes.
    pub on_change: Arc<dyn Fn(usize) + Send + Sync>,
}

impl Reachability {
    /// Start making this device reachable. Returns immediately.
    pub fn start(config: ReachabilityConfig) -> Reachability {
        Reachability {
            task: tokio::spawn(async move {
                maintain(config).await;
            }),
        }
    }
}

async fn maintain(config: ReachabilityConfig) {
    let Some((url, key)) = config.context.directory() else {
        // Cannot change without a rebuild, so report once and stop.
        crate::diag!(
            "[peer-relay] no signed relay directory configured — this device runs its node but \
             nothing can reach it"
        );
        return;
    };

        // Kept across iterations so a no-op refresh doesn't drop connections.
    let mut supervisor: Option<ParkingSupervisor> = None;

    loop {
        let sleep_for = match refresh(&url, &key, &config, &mut supervisor).await {
            Ok(sleep_for) => sleep_for,
            Err(e) => {
                crate::diag!("[peer-relay] reachability refresh failed: {e}");
                pollis_relay::backoff::jittered(RETRY_BACKOFF)
            }
        };
        tokio::time::sleep(sleep_for).await;
    }
}

/// One refresh: re-verify the directory, renew revocation evidence, and make the
/// parked set match. Returns how long to sleep.
async fn refresh(
    url: &str,
    key: &str,
    config: &ReachabilityConfig,
    supervisor: &mut Option<ParkingSupervisor>,
) -> anyhow::Result<Duration> {
    let bytes = directory::fetch_directory(url).await?;
    let now = pollis_relay::proto::now_unix();
    let dir = directory::verify_directory(&bytes, key, now)?;

    // Installed from the same verified bytes that decide where to park. A
    // failure is not fatal to parking; the engine just keeps refusing Extend.
    if let Err(e) = config.next_hops.install(&bytes, now) {
        crate::diag!(
            "[peer-relay] next-hop directory unusable ({e}) — this device stays parked but \
             forwards nothing until it recovers"
        );
    }

    // Best-effort: a failure leaves the engine unable to admit a next hop,
    // which is safe, so it must not abort parking.
    if let Err(e) = renew_revocations(url, &dir, &config.revocations, now).await {
        crate::diag!(
            "[peer-relay] revocation evidence unusable ({e}) — this device stays parked but \
             forwards nothing until it recovers"
        );
    }

    let relays = parked_relays(&dir);
    if relays.is_empty() {
        anyhow::bail!("directory verified but no relay had a usable address and pinned cert");
    }

    let wanted: Vec<SocketAddr> = {
        let mut addrs: Vec<SocketAddr> = relays.iter().map(|r| r.addr).collect();
        addrs.sort();
        addrs
    };
    if supervisor.as_ref().is_some_and(|s| s.relay_set() == wanted) {
        return Ok(until_refresh(dir.expires_at));
    }

    // Parking authenticates as this device; before login that fails closed.
    let identity = config.context.identity().await?;
    *supervisor = Some(ParkingSupervisor::start(
        relays,
        identity,
        config.leaf_der.clone(),
        config.acceptor.clone(),
        Some(config.on_change.clone()),
    ));
    Ok(until_refresh(dir.expires_at))
}

/// Fetch and install the revocation list the directory anchors.
async fn renew_revocations(
    directory_url: &str,
    dir: &directory::Directory,
    store: &RevocationStore,
    now: i64,
) -> anyhow::Result<()> {
    let Some(anchor) = dir.revocation.as_ref() else {
        // No anchor: the store holds no list and the engine forwards nothing,
        // stricter than the pool-side carve-out, as a peer should be.
        return Ok(());
    };
    let url = crate::net::revocation::revocation_url(directory_url, anchor.resolved_path());
    let bytes = directory::fetch_signed(&url).await?;
    // Floored at this directory's anchored `seq`.
    store.install(&bytes, now, anchor.seq)?;
    Ok(())
}

/// Map a verified directory to the relays this device parks at, skipping any we
/// cannot pin (unparseable address or cert).
fn parked_relays(dir: &directory::Directory) -> Vec<ParkedRelay> {
    use base64::Engine as _;
    dir.relays
        .iter()
        .filter_map(|relay| {
            let addr: SocketAddr = relay.addr.trim().parse().ok()?;
            let der = base64::engine::general_purpose::STANDARD
                .decode(relay.cert_b64.trim())
                .ok()?;
            if der.is_empty() {
                return None;
            }
            Some(ParkedRelay {
                addr,
                cert: CertificateDer::from(der),
            })
        })
        .collect()
}

/// Sleep until the next refresh: the directory's expiry less a skew, capped by
/// the revocation TTL and floored.
fn until_refresh(expires_at: i64) -> Duration {
    let now = pollis_relay::proto::now_unix();
    let secs = (expires_at - now).saturating_sub(DIRECTORY_REFRESH_SKEW.as_secs() as i64);
    let clamped = Duration::from_secs(secs.max(0) as u64)
        .min(REVOCATION_REFRESH)
        .max(MIN_REFRESH);
    // Jittered downward only (#875): every device reads the same `expires_at`,
    // and both bounds are load-bearing.
    pollis_relay::backoff::jittered_down(clamped).max(MIN_REFRESH)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory_json(relays: &str) -> directory::Directory {
        serde_json::from_str(&format!(
            r#"{{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{relays}]}}"#
        ))
        .expect("directory parses")
    }

    /// Only relays we can both reach and pin are parked at.
    #[test]
    fn only_pinnable_relays_are_parked_at() {
        let dir = directory_json(
            r#"{"addr":"203.0.113.7:9444","cert_b64":"QUJD"},
               {"addr":"not-an-address","cert_b64":"QUJD"},
               {"addr":"203.0.113.8:9444","cert_b64":"!!!not base64!!!"},
               {"addr":"203.0.113.9:9444","cert_b64":""}"#,
        );
        let relays = parked_relays(&dir);
        assert_eq!(relays.len(), 1);
        assert_eq!(relays[0].addr.to_string(), "203.0.113.7:9444");
    }

    /// Contract §2: park at EVERY first-party relay, not a subset.
    #[test]
    fn every_listed_relay_is_parked_at() {
        let dir = directory_json(
            r#"{"addr":"203.0.113.7:9444","cert_b64":"QUJD"},
               {"addr":"203.0.113.8:9444","cert_b64":"QUJD"}"#,
        );
        assert_eq!(parked_relays(&dir).len(), 2);
    }

    /// The sleep is bounded on both sides over many jittered draws: never a
    /// tight loop, never past the revocation list's freshness.
    #[test]
    fn the_refresh_interval_is_bounded_on_both_sides() {
        let now = pollis_relay::proto::now_unix();
        for _ in 0..200 {
            assert_eq!(until_refresh(now - 10_000), MIN_REFRESH);
            let far = until_refresh(now + 10_000);
            assert!(far <= REVOCATION_REFRESH, "{far:?} exceeded the revocation ceiling");
            assert!(far >= MIN_REFRESH, "{far:?} fell through the floor");
            assert!(
                until_refresh(now + DIRECTORY_REFRESH_SKEW.as_secs() as i64 + 1) >= MIN_REFRESH
            );
        }
    }

    /// The jitter actually varies, so the fleet doesn't refresh in lockstep.
    #[test]
    fn the_refresh_interval_is_not_the_same_value_every_time() {
        let now = pollis_relay::proto::now_unix();
        let first = until_refresh(now + 10_000);
        assert!(
            (0..500).any(|_| until_refresh(now + 10_000) != first),
            "the refresh interval is constant — the fleet would refresh in lockstep"
        );
    }
}
