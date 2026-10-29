//! Relay **path selection** and **guard pinning** — issue #813 Phase B, design
//! `docs/relay-overlay-design.md` §6.2 (multi-hop onion) and §4.4 (Sybil).
//!
//! `pollis-relay` owns the circuit mechanism; which relays form a circuit is policy, here.
//!
//! Three rules this module exists to hold:
//!
//! 1. **The last hop is first-party** (§4.4), so a Sybil can never be the exit; enforced by the [`FirstPartyEndpoint`] type.
//! 2. **The entry hop is a pinned guard** (§4.4a). See [`GuardBook`].
//! 3. **A path never repeats a node**; [`RelayPath::multi`] rejects it.
//!
//! Peer-hosted relays ([`RelayTier::Peer`]) are **middle hops only**, at most one per path.

use std::path::PathBuf;
use std::sync::Mutex;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use pollis_relay::circuit::Hop;
use pollis_relay::CertificateDer;

/// The path length we aim for when the pool can support it.
///
/// **Two, not three**: guard and exit are both first-party, so two hops already split IP from destination; a third costs an RTT.
pub(crate) const TARGET_HOPS: usize = 2;

/// Hard cap on a selected path; every hop is an RTT (§6.4).
pub(crate) const MAX_PATH_HOPS: usize = 3;

const _: () = assert!(MAX_PATH_HOPS <= pollis_relay::MAX_HOPS);
const _: () = assert!(TARGET_HOPS <= MAX_PATH_HOPS);

/// How many entry relays a client pins at once.
///
/// **Two**: Spot churn would strand a single guard; the second is only used when the first is unreachable.
pub(crate) const GUARD_SET_SIZE: usize = 2;

/// How long a guard stays pinned before it is rotated out on purpose.
///
/// **30 days**: a ceiling on how long one node can observe one client's IP.
pub(crate) const GUARD_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;

/// Who runs a relay; decides which circuit positions it may occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayTier {
    /// Ours. The only tier allowed to be a guard or an exit.
    FirstParty,
    /// A peer-hosted relay (D1). **Middle hops only**, at most one per path.
    ///
    /// Only from the signed directory's `peers`; an unsigned channel would let an attacker hand-pick relays.
    Peer,
}

/// A resolved relay endpoint: address, region, pinned QUIC leaf (the identity, §7) and tier.
#[derive(Clone)]
pub(crate) struct RelayEndpoint {
    /// As configured/advertised; resolved to a `SocketAddr` per dial.
    /// For a **peer**, a rendezvous: a first-party relay holding its parked link. A gone link fails as `ExtendFailed`, never a wrong hop.
    pub(crate) addr: String,
    /// A *diversity* hint only; unknown regions are never diverse from each other.
    pub(crate) region: String,
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) tier: RelayTier,
    /// For a peer, the relays it is parked at; **empty means "no constraint"** (v1). Empty for first-party.
    parked_at: Vec<String>,
}

impl RelayEndpoint {
    /// A peer reachable from any first-party relay (v1). Middle hop only.
    #[allow(dead_code)]
    pub(crate) fn peer(
        addr: String,
        region: String,
        cert: CertificateDer<'static>,
    ) -> RelayEndpoint {
        RelayEndpoint::peer_parked_at(addr, region, cert, Vec::new())
    }

    /// A peer reachable only from `parked_at`, as the signed directory produces.
    pub(crate) fn peer_parked_at(
        addr: String,
        region: String,
        cert: CertificateDer<'static>,
        parked_at: Vec<String>,
    ) -> RelayEndpoint {
        RelayEndpoint {
            addr,
            region,
            cert,
            tier: RelayTier::Peer,
            parked_at,
        }
    }

    /// A first-party endpoint.
    pub(crate) fn first_party(
        addr: String,
        region: String,
        cert: CertificateDer<'static>,
    ) -> RelayEndpoint {
        RelayEndpoint {
            addr,
            region,
            cert,
            tier: RelayTier::FirstParty,
            parked_at: Vec::new(),
        }
    }

    /// Can the relay at `addr` reach this endpoint as its next hop?
    ///
    /// Always true for first-party; for a peer, when `addr` holds its link or no parking is named.
    fn reachable_from(&self, addr: &str) -> bool {
        match self.tier {
            RelayTier::FirstParty => true,
            RelayTier::Peer => self.parked_at.is_empty() || self.parked_at.iter().any(|a| a == addr),
        }
    }

    /// How "is this the same node" is decided when a path is assembled.
    ///
    /// **First-party by ADDRESS**: hydra nodes share one pinned leaf.
    ///
    /// **A peer by CERT**: its `addr` is a shared rendezvous.
    fn identity_key(&self) -> String {
        match self.tier {
            RelayTier::FirstParty => format!("addr:{}", self.addr),
            RelayTier::Peer => format!("peer:{}", self.cert_digest_b64()),
        }
    }

    /// Base64 SHA-256 of the DER leaf; the revocation selector and half of a guard pin.
    pub(crate) fn cert_digest_b64(&self) -> String {
        use base64::Engine as _;
        let digest = pollis_relay::proto::cert_fingerprint(self.cert.as_ref());
        base64::engine::general_purpose::STANDARD.encode(digest)
    }
}

/// An endpoint proven to be first-party.
///
/// Only [`FirstPartyEndpoint::new`] builds one (`None` for a peer), so a peer exit does not typecheck.
#[derive(Clone)]
pub(crate) struct FirstPartyEndpoint(RelayEndpoint);

impl FirstPartyEndpoint {
    pub(crate) fn new(endpoint: &RelayEndpoint) -> Option<FirstPartyEndpoint> {
        match endpoint.tier {
            RelayTier::FirstParty => Some(FirstPartyEndpoint(endpoint.clone())),
            RelayTier::Peer => None,
        }
    }

    pub(crate) fn endpoint(&self) -> &RelayEndpoint {
        &self.0
    }
}

/// An ordered relay path, client-outward.
///
/// Two distinct types because the rules are the point:
/// - [`RelayPath::Single`]: v0, one first-party hop, when the pool has no spare.
/// - [`RelayPath::Multi`] (§6.2): first-party guard, middles, first-party exit.
#[derive(Clone)]
pub(crate) enum RelayPath {
    Single(FirstPartyEndpoint),
    Multi {
        guard: FirstPartyEndpoint,
        middles: Vec<RelayEndpoint>,
        exit: FirstPartyEndpoint,
    },
}

impl RelayPath {
    /// Rejects a path that repeats a node, carries two peers, or exceeds [`MAX_PATH_HOPS`].
    pub(crate) fn multi(
        guard: FirstPartyEndpoint,
        middles: Vec<RelayEndpoint>,
        exit: FirstPartyEndpoint,
    ) -> anyhow::Result<RelayPath> {
        let path = RelayPath::Multi {
            guard,
            middles,
            exit,
        };
        let len = path.len();
        // **At most one peer per path**: a second buys no separation and doubles Sybil exposure.
        let peers = path
            .endpoints()
            .iter()
            .filter(|e| e.tier == RelayTier::Peer)
            .count();
        if peers > 1 {
            anyhow::bail!("relay path carries {peers} peer relays; at most one is allowed");
        }
        if len > MAX_PATH_HOPS {
            anyhow::bail!("relay path of {len} hops exceeds the {MAX_PATH_HOPS}-hop cap");
        }
        let mut seen: Vec<String> = Vec::with_capacity(len);
        for endpoint in path.endpoints() {
            let key = endpoint.identity_key();
            if seen.contains(&key) {
                anyhow::bail!("relay path repeats node {}", endpoint.addr);
            }
            seen.push(key);
        }
        Ok(path)
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            RelayPath::Single(_) => 1,
            RelayPath::Multi { middles, .. } => 2 + middles.len(),
        }
    }

    /// Endpoints in dial order; the exit is emitted last here and only here.
    pub(crate) fn endpoints(&self) -> Vec<&RelayEndpoint> {
        match self {
            RelayPath::Single(exit) => vec![exit.endpoint()],
            RelayPath::Multi {
                guard,
                middles,
                exit,
            } => {
                let mut out = Vec::with_capacity(2 + middles.len());
                out.push(guard.endpoint());
                out.extend(middles.iter());
                out.push(exit.endpoint());
                out
            }
        }
    }

    #[allow(dead_code)]
    pub(crate) fn exit(&self) -> &FirstPartyEndpoint {
        match self {
            RelayPath::Single(exit) => exit,
            RelayPath::Multi { exit, .. } => exit,
        }
    }

    /// Resolve endpoints to the `Vec<Hop>` `Circuit::build` consumes.
    pub(crate) async fn resolve_hops(&self) -> anyhow::Result<Vec<Hop>> {
        let mut hops = Vec::with_capacity(self.len());
        for endpoint in self.endpoints() {
            let addr = tokio::net::lookup_host(&endpoint.addr)
                .await
                .map_err(|e| anyhow::anyhow!("overlay: resolve relay {}: {e}", endpoint.addr))?
                .next()
                .ok_or_else(|| {
                    anyhow::anyhow!("overlay: relay {} did not resolve", endpoint.addr)
                })?;
            hops.push(Hop::new(addr, endpoint.cert.clone()));
        }
        Ok(hops)
    }
}

/// How many hops to build for a pool of `usable` admissible relays.
///
/// **Leave a spare**: a 2-node pool builds single-hop rather than lose its failover.
pub(crate) fn desired_hops(usable: usize, target: usize) -> usize {
    usable.saturating_sub(1).clamp(1, target.min(MAX_PATH_HOPS))
}

/// Hops to build from the **first-party** admissible count, plus whether a peer can take a middle.
///
/// Volunteers don't count toward the spare: only first-party nodes can be guard or exit.
///
/// A peer adds ONE hop on top: a middle in a different trust domain.
pub(crate) fn hops_for(first_party_usable: usize, peer_available: bool, target: usize) -> usize {
    let base = desired_hops(first_party_usable, target);
    if base > 1 && peer_available {
        return (base + 1).min(target).min(MAX_PATH_HOPS);
    }
    base
}

/// Does this pool build multi-hop paths (so guards are needed)? `usable` indexes `pool`.
pub(crate) fn is_multi_hop(pool: &[RelayEndpoint], usable: &[usize], target: usize) -> bool {
    let first_party = usable
        .iter()
        .filter(|&&i| pool[i].tier == RelayTier::FirstParty)
        .count();
    desired_hops(first_party, target) > 1
}

/// Pick the guard to use for attempt `attempt`, given the pinned guard order and
/// the exit already chosen. Returns an index into `pool`.
///
/// Guards rotate **per attempt**, so a dead guard costs one dial and total dials don't regress.
fn pick_guard(guards: &[usize], attempt: usize, exit: usize) -> Option<usize> {
    if guards.is_empty() {
        return None;
    }
    // Skip the guard that IS the exit: that node would see both ends.
    (0..guards.len())
        .map(|i| guards[(attempt + i) % guards.len()])
        .find(|&g| g != exit)
}

/// Choose `count` middle hops from `candidates` (indices into `pool`), avoiding
/// anything already on the path and preferring regions not already represented.
///
/// Region diversity is a preference: today's pool is single-region.
/// Prefer one peer for a middle slot, where [`peer_position_admissible`] allows and the preceding relay holds its link.
///
/// [`peer_position_admissible`]: crate::net::peer::discovery::peer_position_admissible
fn pick_middles(
    pool: &[RelayEndpoint],
    candidates: &[usize],
    on_path: &[usize],
    count: usize,
    path_len: usize,
    guard_addr: &str,
) -> Vec<usize> {
    if count == 0 {
        return Vec::new();
    }
    let mut regions: Vec<&str> = on_path
        .iter()
        .map(|&i| pool[i].region.as_str())
        .filter(|r| !r.is_empty())
        .collect();
    let mut chosen = Vec::with_capacity(count);
    let mut taken: Vec<usize> = on_path.to_vec();
    let mut peer_taken = false;
    for slot in 0..count {
        // Middles sit between the guard (0) and the exit (path_len - 1).
        let position = 1 + slot;
        let next = candidates
            .iter()
            .copied()
            .filter(|i| !taken.contains(i))
            .filter(|&i| {
                if pool[i].tier != RelayTier::Peer {
                    return true;
                }
                // The position rule is `net::peer::discovery`'s, not re-derived.
                !peer_taken
                    && crate::net::peer::discovery::peer_position_admissible(position, path_len)
                    && pool[i].reachable_from(guard_addr)
            })
            // A peer first, then diverse regions; `position` breaks ties.
            .min_by_key(|&i| {
                let region = pool[i].region.as_str();
                let duplicate = !region.is_empty() && regions.contains(&region);
                let not_peer = (pool[i].tier != RelayTier::Peer) as u8;
                (
                    not_peer,
                    duplicate as u8,
                    candidates.iter().position(|&c| c == i).unwrap_or(usize::MAX),
                )
            });
        match next {
            Some(i) => {
                if !pool[i].region.is_empty() {
                    regions.push(pool[i].region.as_str());
                }
                peer_taken |= pool[i].tier == RelayTier::Peer;
                taken.push(i);
                chosen.push(i);
            }
            None => break,
        }
    }
    chosen
}

/// Build the path for one dial attempt.
///
/// - `order` is the admissible candidate order (health/latency ordered by the
///   pool), already filtered for revocation.
/// - `exit` is the candidate being tried as the terminal hop this attempt.
/// - `guards` is the pinned guard order (indices into `pool`).
///
/// `None` when no valid path exists, notably when the exit is not first-party.
pub(crate) fn plan_path(
    pool: &[RelayEndpoint],
    order: &[usize],
    guards: &[usize],
    exit: usize,
    attempt: usize,
    target_hops: usize,
) -> Option<RelayPath> {
    // The type-level gate: a peer cannot be a `FirstPartyEndpoint`.
    let exit_fp = FirstPartyEndpoint::new(pool.get(exit)?)?;
    let first_party_usable = order
        .iter()
        .filter(|&&i| pool[i].tier == RelayTier::FirstParty)
        .count();
    if desired_hops(first_party_usable, target_hops) <= 1 {
        return Some(RelayPath::Single(exit_fp));
    }
    let guard = pick_guard(guards, attempt, exit)?;
    // Re-proven here: the entry hop sees the client's IP.
    let guard_fp = FirstPartyEndpoint::new(pool.get(guard)?)?;
    // A peer lengthens the path only if usable: not on it and reachable from the guard.
    let peer_available = order.iter().any(|&i| {
        i != guard
            && i != exit
            && pool[i].tier == RelayTier::Peer
            && pool[i].reachable_from(&pool[guard].addr)
    });
    let hops = hops_for(first_party_usable, peer_available, target_hops);
    // Shorten rather than fail; 2 hops still beat a direct dial.
    let middles = pick_middles(
        pool,
        order,
        &[guard, exit],
        hops - 2,
        hops,
        &pool[guard].addr,
    )
    .into_iter()
    .map(|i| pool[i].clone())
    .collect();
    RelayPath::multi(guard_fp, middles, exit_fp).ok()
}

/// One pinned guard. Identity is `addr` **and** cert digest, so a new leaf is a new relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinnedGuard {
    addr: String,
    cert_sha256_b64: String,
    /// Unix seconds when this guard was first pinned.
    pinned_at: i64,
}

/// On-disk shape, versioned for future migration.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GuardFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    guards: Vec<PinnedGuard>,
}

const GUARD_FILE_VERSION: u32 = 1;

/// The client's pinned entry relays, persisted across restarts.
///
/// **Why pin (§4.4a).** Fresh first hops eventually hit every relay; a small pinned set makes it a one-off draw.
///
/// **Rotation**: replaced when it leaves the directory, is inadmissible, or outlives [`GUARD_LIFETIME_SECS`]; never for being unreachable (guard-discovery attack).
pub(crate) struct GuardBook {
    /// `None` ⇒ in-memory only.
    path: Option<PathBuf>,
    state: Mutex<Option<Vec<PinnedGuard>>>,
}

impl GuardBook {
    /// A book persisted at `path` (created lazily on first write).
    pub(crate) fn at(path: PathBuf) -> GuardBook {
        GuardBook {
            path: Some(path),
            state: Mutex::new(None),
        }
    }

    /// A book that never touches the filesystem.
    pub(crate) fn ephemeral() -> GuardBook {
        GuardBook {
            path: None,
            state: Mutex::new(Some(Vec::new())),
        }
    }

    /// The default location alongside the accounts index.
    pub(crate) fn default_path() -> PathBuf {
        crate::db::local::dirs_path().join("overlay-guards.json")
    }

    /// The pinned guard order for `pool`, as indices into it.
    ///
    /// Stale, unusable or expired pins are dropped, then topped up at random from first-party `usable`, preferring new regions.
    pub(crate) fn guards_for(
        &self,
        pool: &[RelayEndpoint],
        usable: &[usize],
        now_secs: i64,
    ) -> Vec<usize> {
        let mut guard = self.state.lock().unwrap();
        if guard.is_none() {
            *guard = Some(self.load());
        }
        let pinned = guard.as_mut().expect("loaded above");
        let before = pinned.clone();

        let mut kept: Vec<(PinnedGuard, usize)> = Vec::new();
        for pin in pinned.iter() {
            if now_secs.saturating_sub(pin.pinned_at) >= GUARD_LIFETIME_SECS {
                continue;
            }
            let found = usable.iter().copied().find(|&i| {
                pool[i].tier == RelayTier::FirstParty
                    && pool[i].addr == pin.addr
                    && pool[i].cert_digest_b64() == pin.cert_sha256_b64
            });
            if let Some(i) = found {
                kept.push((pin.clone(), i));
            }
        }

        let mut chosen: Vec<usize> = kept.iter().map(|(_, i)| *i).collect();
        if chosen.len() < GUARD_SET_SIZE {
            let mut fresh: Vec<usize> = usable
                .iter()
                .copied()
                .filter(|&i| pool[i].tier == RelayTier::FirstParty && !chosen.contains(&i))
                .collect();
            fresh.shuffle(&mut rand::thread_rng());
            // Prefer an unpinned region; the shuffle keeps the choice uniform.
            let pinned_regions: Vec<&str> = chosen
                .iter()
                .map(|&i| pool[i].region.as_str())
                .filter(|r| !r.is_empty())
                .collect();
            fresh.sort_by_key(|&i| {
                let region = pool[i].region.as_str();
                (!region.is_empty() && pinned_regions.contains(&region)) as u8
            });
            for i in fresh.into_iter().take(GUARD_SET_SIZE - chosen.len()) {
                kept.push((
                    PinnedGuard {
                        addr: pool[i].addr.clone(),
                        cert_sha256_b64: pool[i].cert_digest_b64(),
                        pinned_at: now_secs,
                    },
                    i,
                ));
                chosen.push(i);
            }
        }

        *pinned = kept.iter().map(|(p, _)| p.clone()).collect();
        // Compare `pinned_at` too, or a re-pinned guard's fresh clock is never written and it re-rolls forever.
        let changed = pinned.len() != before.len()
            || pinned.iter().zip(before.iter()).any(|(a, b)| {
                a.addr != b.addr
                    || a.cert_sha256_b64 != b.cert_sha256_b64
                    || a.pinned_at != b.pinned_at
            });
        let snapshot = pinned.clone();
        drop(guard);
        if changed {
            self.persist(&snapshot);
        }
        chosen
    }

    /// When each pin was taken; a re-pin is a NEW pin with a fresh clock.
    #[cfg(test)]
    fn pinned_at(&self) -> Vec<i64> {
        self.state
            .lock()
            .unwrap()
            .as_ref()
            .map(|g| g.iter().map(|p| p.pinned_at).collect())
            .unwrap_or_default()
    }

    fn load(&self) -> Vec<PinnedGuard> {
        let Some(path) = self.path.as_ref() else {
            return Vec::new();
        };
        let Ok(bytes) = std::fs::read(path) else {
            return Vec::new();
        };
        // A corrupt file means "no pins".
        match serde_json::from_slice::<GuardFile>(&bytes) {
            Ok(file) if file.version == GUARD_FILE_VERSION => file.guards,
            _ => Vec::new(),
        }
    }

    fn persist(&self, guards: &[PinnedGuard]) {
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let file = GuardFile {
            version: GUARD_FILE_VERSION,
            guards: guards.to_vec(),
        };
        let Ok(bytes) = serde_json::to_vec_pretty(&file) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = crate::private_fs::create_dir_all(parent);
        }
        // Best effort; owner-only since pins reveal where the owner connects from.
        if let Err(e) = crate::private_fs::write(path, bytes) {
            crate::diag!("[overlay] could not persist guard pins: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(seed: u8) -> CertificateDer<'static> {
        CertificateDer::from(vec![seed; 32])
    }

    fn first_party(addr: &str, region: &str, seed: u8) -> RelayEndpoint {
        RelayEndpoint::first_party(addr.to_string(), region.to_string(), cert(seed))
    }

    fn peer(addr: &str, region: &str, seed: u8) -> RelayEndpoint {
        RelayEndpoint::peer(addr.to_string(), region.to_string(), cert(seed))
    }

    fn all(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    /// A peer cannot become the proof type `RelayPath`'s terminal fields require.
    #[test]
    fn a_peer_cannot_become_a_first_party_endpoint() {
        assert!(FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).is_some());
        assert!(FirstPartyEndpoint::new(&peer("p:1", "us", 2)).is_none());
    }

    /// Over every pool shape, every path ends on a first-party hop.
    #[test]
    fn last_hop_is_always_first_party() {
        let pools: Vec<Vec<RelayEndpoint>> = vec![
            vec![first_party("a:1", "us", 1)],
            vec![first_party("a:1", "us", 1), peer("p:1", "eu", 9)],
            vec![
                first_party("a:1", "us", 1),
                peer("p:1", "eu", 9),
                first_party("b:1", "eu", 2),
            ],
            vec![
                peer("p:1", "eu", 9),
                peer("p:2", "ap", 8),
                first_party("a:1", "us", 1),
                first_party("b:1", "eu", 2),
            ],
            vec![
                peer("p:1", "eu", 9),
                first_party("a:1", "us", 1),
                first_party("b:1", "eu", 2),
                first_party("c:1", "ap", 3),
            ],
        ];
        for pool in &pools {
            let order = all(pool.len());
            let guards: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&i| pool[i].tier == RelayTier::FirstParty)
                .take(GUARD_SET_SIZE)
                .collect();
            for exit in order.iter().copied() {
                for attempt in 0..4 {
                    for target in 1..=MAX_PATH_HOPS {
                        let Some(path) = plan_path(pool, &order, &guards, exit, attempt, target)
                        else {
                            continue;
                        };
                        let hops = path.endpoints();
                        assert_eq!(
                            hops.last().unwrap().tier,
                            RelayTier::FirstParty,
                            "path terminated on a peer relay"
                        );
                        assert_eq!(
                            path.exit().endpoint().addr,
                            hops.last().unwrap().addr,
                            "exit() must be the hop that is actually dialed last"
                        );
                        assert_eq!(hops[0].tier, RelayTier::FirstParty);
                    }
                }
            }
        }
    }

    /// A peer asked to be the exit yields no path at all.
    #[test]
    fn a_peer_candidate_yields_no_path_as_exit() {
        let pool = vec![
            first_party("a:1", "us", 1),
            first_party("b:1", "us", 2),
            peer("p:1", "eu", 9),
        ];
        let order = all(3);
        assert!(plan_path(&pool, &order, &[0, 1], 2, 0, TARGET_HOPS).is_none());
        assert!(plan_path(&pool, &order, &[0, 1], 1, 0, TARGET_HOPS).is_some());
    }

    /// A peers-only pool produces no path (fail-closed).
    #[test]
    fn an_all_peer_pool_yields_no_path() {
        let pool = vec![peer("p:1", "eu", 9), peer("p:2", "ap", 8)];
        let order = all(2);
        for exit in 0..2 {
            assert!(plan_path(&pool, &order, &[], exit, 0, TARGET_HOPS).is_none());
        }
    }

    /// Peers ARE usable as middles, between two first-party hops.
    #[test]
    fn a_peer_is_usable_as_a_middle_hop() {
        let pool = vec![
            first_party("a:1", "us-west-2", 1),
            first_party("b:1", "us-west-2", 2),
            peer("p:1", "eu-west-1", 9),
            first_party("c:1", "us-east-1", 3),
        ];
        let order = all(4);
        let path = plan_path(&pool, &order, &[0], 1, 0, 3).expect("3-hop path");
        let hops = path.endpoints();
        assert_eq!(hops.len(), 3);
        assert_eq!(hops[0].addr, "a:1");
        assert_eq!(hops[1].addr, "p:1");
        assert_eq!(hops[1].tier, RelayTier::Peer);
        assert_eq!(hops[2].addr, "b:1");
        assert_eq!(hops[2].tier, RelayTier::FirstParty);
    }

    /// **At most one peer per path**, held by the constructor.
    ///
    /// Asserts the REASON: a two-peer path is over the hop cap anyway.
    #[test]
    fn a_path_carries_at_most_one_peer() {
        let a = FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).unwrap();
        let b = FirstPartyEndpoint::new(&first_party("b:1", "us", 2)).unwrap();
        let one = vec![peer("a:1", "eu", 9)];
        let two = vec![peer("a:1", "eu", 9), peer("b:1", "ap", 8)];
        assert!(RelayPath::multi(a.clone(), one, b.clone()).is_ok());
        let Err(err) = RelayPath::multi(a, two, b) else {
            panic!("a path with two peer relays must not be constructible");
        };
        let err = err.to_string();
        assert!(
            err.contains("at most one"),
            "rejected for the wrong reason: {err}"
        );
    }

    /// ...and the selector never produces one.
    #[test]
    fn the_selector_never_puts_two_peers_on_a_path() {
        let pool = vec![
            first_party("a:1", "us-west-2", 1),
            first_party("b:1", "us-east-1", 2),
            first_party("c:1", "eu-west-1", 3),
            first_party("d:1", "ap-south-1", 4),
            peer("a:1", "", 9),
            peer("b:1", "", 8),
            peer("c:1", "", 7),
        ];
        let order = all(pool.len());
        let guards = vec![0, 1];
        for exit in 0..pool.len() {
            for attempt in 0..4 {
                for target in 1..=MAX_PATH_HOPS {
                    let Some(path) = plan_path(&pool, &order, &guards, exit, attempt, target) else {
                        continue;
                    };
                    let peers = path
                        .endpoints()
                        .iter()
                        .filter(|e| e.tier == RelayTier::Peer)
                        .count();
                    assert!(peers <= 1, "path carried {peers} peer relays");
                }
            }
        }
    }

    /// A peer sharing its rendezvous address with a hop is not a repeat.
    #[test]
    fn a_peer_sharing_a_hop_s_rendezvous_address_is_not_a_repeat() {
        let pool = vec![
            first_party("a:1", "us-west-2", 1),
            first_party("b:1", "us-west-2", 2),
            first_party("c:1", "us-west-2", 3),
            peer("a:1", "", 9),
        ];
        let path = plan_path(&pool, &all(4), &[0], 1, 0, 3).expect("path");
        let hops = path.endpoints();
        assert_eq!(hops.len(), 3);
        assert_eq!(hops[0].addr, "a:1");
        assert_eq!(hops[1].tier, RelayTier::Peer);
        assert_eq!(hops[1].addr, "a:1", "the peer rides the guard's rendezvous");
        assert_eq!(hops[2].tier, RelayTier::FirstParty);
        let a = FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).unwrap();
        let b = FirstPartyEndpoint::new(&first_party("b:1", "us", 2)).unwrap();
        assert!(RelayPath::multi(a, vec![first_party("a:1", "us", 9)], b).is_err());
    }

    /// A peer is only offered by a relay that can reach it; otherwise the path shortens.
    #[test]
    fn a_peer_parked_elsewhere_is_not_reachable_from_this_guard() {
        let pool = vec![
            first_party("a:1", "us-west-2", 1),
            first_party("b:1", "us-west-2", 2),
            first_party("c:1", "us-west-2", 3),
            RelayEndpoint::peer_parked_at("b:1".into(), String::new(), cert(9), vec!["b:1".into()]),
        ];
        let path = plan_path(&pool, &all(4), &[0], 1, 0, 3).expect("path");
        assert_eq!(path.len(), 2);
        assert!(path.endpoints().iter().all(|e| e.tier == RelayTier::FirstParty));
        let path = plan_path(&pool, &all(4), &[1], 2, 0, 3).expect("path");
        assert_eq!(path.len(), 3);
        assert_eq!(path.endpoints()[1].tier, RelayTier::Peer);
    }

    /// Leave-a-spare: a pool with no node to fail over to builds single-hop.
    #[test]
    fn hop_count_leaves_a_spare() {
        assert_eq!(desired_hops(1, TARGET_HOPS), 1);
        assert_eq!(desired_hops(2, TARGET_HOPS), 1);
        assert_eq!(desired_hops(3, TARGET_HOPS), 2);
        assert_eq!(desired_hops(9, TARGET_HOPS), 2);
        assert_eq!(desired_hops(3, 3), 2);
        assert_eq!(desired_hops(4, 3), 3);
        assert_eq!(desired_hops(9, 99), MAX_PATH_HOPS);
    }

    /// Peers don't count toward the spare rule; a peer ADDS a hop.
    #[test]
    fn peers_lengthen_a_path_without_inflating_the_spare_rule() {
        assert_eq!(hops_for(2, false, 3), 1);
        assert_eq!(hops_for(3, false, 3), 2);
        assert_eq!(hops_for(3, false, TARGET_HOPS), 2);
        // A peer never rescues a pool too small for multi-hop.
        assert_eq!(hops_for(2, true, 3), 1);
        assert_eq!(hops_for(3, true, 3), 3);
        assert_eq!(hops_for(3, true, TARGET_HOPS), 2);
        assert_eq!(hops_for(9, true, 99), MAX_PATH_HOPS);
    }

    /// A path may never repeat a node, and guard == exit is refused outright.
    #[test]
    fn a_path_never_repeats_a_node() {
        let a = FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).unwrap();
        let b = FirstPartyEndpoint::new(&first_party("b:1", "us", 2)).unwrap();
        assert!(RelayPath::multi(a.clone(), vec![], a.clone()).is_err());
        assert!(
            RelayPath::multi(a.clone(), vec![first_party("a:1", "us", 1)], b.clone()).is_err()
        );
        assert!(RelayPath::multi(a, vec![], b).is_ok());
    }

    /// Over the cap is a hard error, not a truncation.
    #[test]
    fn a_path_cannot_exceed_the_hop_cap() {
        let a = FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).unwrap();
        let b = FirstPartyEndpoint::new(&first_party("b:1", "us", 2)).unwrap();
        let middles = vec![
            first_party("m1:1", "us", 3),
            first_party("m2:1", "us", 4),
            first_party("m3:1", "us", 5),
        ];
        assert!(RelayPath::multi(a, middles, b).is_err());
    }

    /// The selector never puts the same relay at both ends, even when the exit
    /// under test IS one of the pinned guards.
    #[test]
    fn the_guard_is_never_also_the_exit() {
        let pool = vec![
            first_party("a:1", "us", 1),
            first_party("b:1", "us", 2),
            first_party("c:1", "us", 3),
        ];
        let order = all(3);
        let guards = vec![0, 1];
        for exit in 0..3 {
            for attempt in 0..5 {
                let path =
                    plan_path(&pool, &order, &guards, exit, attempt, TARGET_HOPS).expect("path");
                let hops = path.endpoints();
                assert_ne!(hops[0].addr, hops[hops.len() - 1].addr);
                assert_eq!(hops[hops.len() - 1].addr, pool[exit].addr);
            }
        }
    }

    /// Guards rotate per attempt, so a dead primary guard costs one wasted dial
    /// rather than wedging every attempt in the connect.
    #[test]
    fn guards_rotate_across_attempts() {
        assert_eq!(pick_guard(&[0, 1], 0, 5), Some(0));
        assert_eq!(pick_guard(&[0, 1], 1, 5), Some(1));
        assert_eq!(pick_guard(&[0, 1], 2, 5), Some(0));
        assert_eq!(pick_guard(&[0, 1], 0, 0), Some(1));
        assert_eq!(pick_guard(&[0], 0, 0), None);
        assert_eq!(pick_guard(&[], 0, 0), None);
    }

    /// Region diversity is preferred when the pool offers it.
    #[test]
    fn middles_prefer_an_unrepresented_region() {
        let pool = vec![
            first_party("a:1", "us-west-2", 1),
            first_party("b:1", "us-west-2", 2),
            first_party("same:1", "us-west-2", 3),
            first_party("far:1", "eu-west-1", 4),
        ];
        // "far" wins on region diversity despite sorting later.
        let picked = pick_middles(&pool, &all(4), &[0, 1], 1, 3, "a:1");
        assert_eq!(picked, vec![3]);
    }

    /// Pins are stable across selections and a reload from disk.
    #[test]
    fn guard_pinning_pins_and_persists() {
        let dir = std::env::temp_dir().join(format!("pollis-guards-{}", std::process::id()));
        let path = dir.join("overlay-guards.json");
        let _ = std::fs::remove_file(&path);
        let pool: Vec<RelayEndpoint> = (0..5)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let usable = all(5);

        let book = GuardBook::at(path.clone());
        let first = book.guards_for(&pool, &usable, 1_000);
        assert_eq!(first.len(), GUARD_SET_SIZE);
        for _ in 0..20 {
            assert_eq!(book.guards_for(&pool, &usable, 1_000), first);
        }
        let reloaded = GuardBook::at(path.clone());
        assert_eq!(reloaded.guards_for(&pool, &usable, 1_000), first);
        let _ = std::fs::remove_file(&path);
    }

    /// A guard that leaves the pool is replaced; the survivor keeps its pin.
    #[test]
    fn a_guard_that_leaves_the_pool_is_replaced() {
        let pool: Vec<RelayEndpoint> = (0..4)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let book = GuardBook::ephemeral();
        let pinned = book.guards_for(&pool, &all(4), 1_000);
        assert_eq!(pinned.len(), 2);

        let usable: Vec<usize> = all(4).into_iter().filter(|&i| i != pinned[0]).collect();
        let next = book.guards_for(&pool, &usable, 1_000);
        assert_eq!(next.len(), 2);
        assert!(!next.contains(&pinned[0]), "a gone guard must be un-pinned");
        assert!(
            next.contains(&pinned[1]),
            "the surviving guard keeps its pin — the set is not re-rolled wholesale"
        );
    }

    /// An expired pin rotates out on its own.
    #[test]
    fn a_guard_rotates_out_after_its_lifetime() {
        let pool: Vec<RelayEndpoint> = (0..4)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let book = GuardBook::ephemeral();
        book.guards_for(&pool, &all(4), 1_000);
        assert_eq!(book.pinned_at(), vec![1_000, 1_000]);

        book.guards_for(&pool, &all(4), 1_000 + GUARD_LIFETIME_SECS - 1);
        assert_eq!(book.pinned_at(), vec![1_000, 1_000], "pin rotated too early");

        // At the lifetime both pins are re-taken with the new clock.
        let now = 1_000 + GUARD_LIFETIME_SECS;
        let later = book.guards_for(&pool, &all(4), now);
        assert_eq!(later.len(), GUARD_SET_SIZE);
        assert_eq!(book.pinned_at(), vec![now, now], "an expired pin was kept");
    }

    /// Unreachable is not grounds for un-pinning (guard-discovery attack).
    #[test]
    fn a_guard_is_not_un_pinned_for_being_down() {
        let pool: Vec<RelayEndpoint> = (0..4)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let book = GuardBook::ephemeral();
        let pinned = book.guards_for(&pool, &all(4), 1_000);
        for _ in 0..10 {
            assert_eq!(book.guards_for(&pool, &all(4), 1_100), pinned);
        }
    }

    /// A peer relay is never pinned as a guard: the entry hop sees the client's
    /// IP, so it stays first-party.
    #[test]
    fn guards_are_never_peers() {
        let pool = vec![
            peer("p1:1", "eu", 9),
            peer("p2:1", "ap", 8),
            first_party("a:1", "us", 1),
            first_party("b:1", "us", 2),
        ];
        let book = GuardBook::ephemeral();
        let guards = book.guards_for(&pool, &all(4), 1_000);
        assert_eq!(guards.len(), 2);
        for i in guards {
            assert_eq!(pool[i].tier, RelayTier::FirstParty);
        }
    }
}
