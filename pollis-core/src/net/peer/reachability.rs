//! Turning consent into an actually-reachable device: read the signed
//! directory, keep the peer's own revocation evidence current, and park a
//! connection at every first-party relay it lists.
//!
//! This is the join between [`super::park`] (the transport) and
//! [`super::serving`] (the consent state machine). It exists as its own module
//! because it is the only part of peer relaying that does **network I/O on a
//! schedule**, and that deserves to be one reviewable place.
//!
//! # Two things are refreshed here, for two different reasons
//!
//! - **The directory** decides *where* to park. Membership churns (the hosted
//!   pool runs on Spot instances), so a stale set means parking at a node that
//!   no longer exists.
//! - **The revocation list** decides whether this peer may forward *at all*.
//!   `PeerEngine` is handed the store built here, and `serve_extend` refuses
//!   when it cannot evaluate revocation (#813 phase C/E2). The artifact carries
//!   a 5-minute TTL and is re-signed every 2 minutes, and `admit` re-checks
//!   expiry at **use** time — so a missed refresh degrades this device to "no
//!   longer forwarding", never to "still trusting a stale list". That is why the
//!   refresh can be lazy and best-effort rather than a correctness dependency.
//!
//! # On the refresh cadence
//!
//! The loop sleeps on the **directory's own `expires_at`**, clamped by the
//! revocation artifact's much shorter TTL — the same shape `net::overlay`'s
//! already-shipped `directory_refresh_loop` uses, for the same reason. It is not
//! a keepalive poll: it does no work to *discover* state, it renews a
//! short-lived signed artifact whose expiry is the schedule, and it exists only
//! while the user's consent and conditions hold. When consent is withdrawn the
//! whole task is dropped and nothing is left ticking.
//!
//! # Fail-closed, in both directions
//!
//! - No directory configured ⇒ nowhere to park **and** no key to evaluate
//!   revocation with ⇒ the device runs its node, carries nothing, and reports
//!   `Waiting{NoInboundPath}`. Exactly the pre-wave-3 state, honestly reported.
//! - Directory but no usable revocation list ⇒ parked, reachable, and still
//!   forwarding nothing, because the engine's store cannot admit a next hop.
//!   Being reachable is not the same as being willing.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use pollis_relay::policy::RevocationStore;
use pollis_relay::CertificateDer;
use tokio::task::JoinHandle;

use super::context::ServingContext;
use super::park::{LinkAcceptor, ParkedRelay, ParkingSupervisor};
use crate::net::directory;

/// Refresh the directory this long before it expires, so parking never runs off
/// a directory that has already lapsed.
const DIRECTORY_REFRESH_SKEW: Duration = Duration::from_secs(5 * 60);

/// Ceiling on the sleep between refreshes, set by the *revocation* artifact
/// rather than the directory: the list is re-signed every ~2 minutes with a
/// 5-minute TTL, so renewing every minute leaves ~5× margin. A device that
/// oversleeps stops forwarding (fail-closed) rather than forwarding stale.
const REVOCATION_REFRESH: Duration = Duration::from_secs(60);

/// Floor on the sleep, so a pathological `expires_at` cannot tight-loop the
/// directory host.
const MIN_REFRESH: Duration = Duration::from_secs(15);

/// After a failed refresh, retry on this fixed backoff — recover lazily, never
/// hammer.
const RETRY_BACKOFF: Duration = Duration::from_secs(30);

/// Owns the refresh loop and, through it, every parked connection.
///
/// Dropping it aborts the loop, which drops the [`ParkingSupervisor`], which
/// closes every parked connection. That is the whole "withdrawing consent stops
/// the device being reachable, now" path — no flag to check, no shutdown
/// handshake to get wrong.
pub struct Reachability {
    task: JoinHandle<()>,
}

impl Drop for Reachability {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Everything the loop needs, gathered so the signature does not sprawl.
pub struct ReachabilityConfig {
    pub context: Arc<dyn ServingContext>,
    /// The store the running [`super::engine::PeerEngine`] was handed. Cloned
    /// from the same cell, so a list installed here is observed immediately by
    /// every in-flight `Extend`.
    pub revocations: RevocationStore,
    /// This device's relay leaf — the `Park` frame's payload.
    pub leaf_der: CertificateDer<'static>,
    pub acceptor: Arc<dyn LinkAcceptor>,
    /// Called with the live parked-connection count whenever it changes.
    pub on_change: Arc<dyn Fn(usize) + Send + Sync>,
}

impl Reachability {
    /// Start making this device reachable. Returns immediately; the first park
    /// lands once the directory has been fetched and verified.
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
        // No signed directory ⇒ no relays to park at, and no key to evaluate
        // revocation with. Report once and stop rather than retry a thing that
        // cannot change without a rebuild.
        eprintln!(
            "[peer-relay] no signed relay directory configured — this device runs its node but \
             nothing can reach it"
        );
        return;
    };

    // Held across iterations: replacing it is what a membership change costs,
    // and keeping it is what stops a no-op refresh dropping live connections.
    let mut supervisor: Option<ParkingSupervisor> = None;

    loop {
        let sleep_for = match refresh(&url, &key, &config, &mut supervisor).await {
            Ok(sleep_for) => sleep_for,
            Err(e) => {
                eprintln!("[peer-relay] reachability refresh failed: {e}");
                RETRY_BACKOFF
            }
        };
        tokio::time::sleep(sleep_for).await;
    }
}

/// One refresh: re-verify the directory, renew revocation evidence, and make the
/// parked set match what the directory now says. Returns how long to sleep.
async fn refresh(
    url: &str,
    key: &str,
    config: &ReachabilityConfig,
    supervisor: &mut Option<ParkingSupervisor>,
) -> anyhow::Result<Duration> {
    let bytes = directory::fetch_directory(url).await?;
    let now = pollis_relay::proto::now_unix();
    let dir = directory::verify_directory(&bytes, key, now)?;

    // Renew the peer's own revocation evidence. Best-effort by design: a failure
    // here leaves the engine unable to admit a next hop, which is the safe
    // outcome, so it must not abort the parking it is refreshing alongside.
    if let Err(e) = renew_revocations(url, &dir, &config.revocations, now).await {
        eprintln!(
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
        // Membership is unchanged: leave the live connections exactly alone.
        return Ok(until_refresh(dir.expires_at));
    }

    // A parked connection authenticates as this device, so the identity is
    // required before anything can be parked. Before login there is none, and
    // that is a fail-closed "not reachable yet", not an error to swallow.
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
        // A directory published before revocation existed anchors nothing. The
        // store then holds no list and the engine forwards nothing — strictly
        // safer than the pool-side carve-out, and correct here because a peer
        // that cannot evaluate revocation has no business extending.
        return Ok(());
    };
    let url = crate::net::revocation::revocation_url(directory_url, anchor.resolved_path());
    let bytes = directory::fetch_signed(&url).await?;
    // Floored at the sequence THIS directory commits to, which is what makes a
    // stale-list/fresh-directory pairing detectable.
    store.install(&bytes, now, anchor.seq)?;
    Ok(())
}

/// Map a verified directory to the relays this device parks at. An entry whose
/// address will not parse or whose cert is not decodable base64 DER is skipped —
/// never park at a relay we cannot pin.
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

/// Sleep until the next refresh: the directory's own expiry less a skew, capped
/// by the revocation artifact's far shorter TTL and floored so nothing tight
/// loops.
fn until_refresh(expires_at: i64) -> Duration {
    let now = pollis_relay::proto::now_unix();
    let secs = (expires_at - now).saturating_sub(DIRECTORY_REFRESH_SKEW.as_secs() as i64);
    Duration::from_secs(secs.max(0) as u64)
        .min(REVOCATION_REFRESH)
        .max(MIN_REFRESH)
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

    /// Only relays we can both reach and pin are parked at. A malformed address
    /// or an undecodable cert is skipped, never parked at unverified.
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

    /// The sleep is derived from the artifact's own expiry, bounded on both
    /// sides — never zero (which would tight-loop) and never longer than the
    /// revocation list stays fresh (which would forward on stale evidence).
    #[test]
    fn the_refresh_interval_is_bounded_on_both_sides() {
        let now = pollis_relay::proto::now_unix();
        assert_eq!(until_refresh(now - 10_000), MIN_REFRESH);
        assert_eq!(until_refresh(now + 10_000), REVOCATION_REFRESH);
        // Just past the skew: still floored, never a tight loop.
        assert!(until_refresh(now + DIRECTORY_REFRESH_SKEW.as_secs() as i64 + 1) >= MIN_REFRESH);
    }
}
