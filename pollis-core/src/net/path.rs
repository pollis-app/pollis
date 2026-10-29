//! Relay path selection and guard pinning (#813; `docs/relay-overlay-design.md`
//! §6.2, §4.4). `pollis-relay` builds circuits; which relays they use is policy,
//! and it lives here. Three rules:
//!
//! 1. **The last hop is first-party**, so a Sybil can never be the exit.
//!    Enforced as a type: [`RelayPath`]'s terminal hop is a
//!    [`FirstPartyEndpoint`], whose constructor rejects a peer.
//! 2. **The entry hop is a pinned guard** ([`GuardBook`], §4.4a).
//! 3. **A path never repeats a node**; guard == exit is the both-ends position
//!    multi-hop exists to prevent.
//!
//! Peer-hosted relays ([`RelayTier::Peer`]) are middle hops only, at most one
//! per path. A peer's `addr` is the rendezvous relay holding its parked link,
//! so peers are compared by cert, first-party relays by address; and a peer
//! adds a hop rather than counting toward the spare rule ([`hops_for`]).

use std::path::PathBuf;
use std::sync::Mutex;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use pollis_relay::circuit::Hop;
use pollis_relay::CertificateDer;

/// The path length we aim for when the pool can support it.
///
/// Two, not Tor's three: guard and exit are both first-party, so two hops
/// already keep the client's IP and the destination on different nodes. A
/// third first-party hop would cost an RTT for nothing; a peer middle is what
/// [`MAX_PATH_HOPS`] leaves room for.
pub(crate) const TARGET_HOPS: usize = 2;

/// Hard cap on a selected path, below `pollis_relay::MAX_HOPS`: every hop is an
/// RTT on the control plane's latency budget (§6.4).
pub(crate) const MAX_PATH_HOPS: usize = 3;

const _: () = assert!(MAX_PATH_HOPS <= pollis_relay::MAX_HOPS);
const _: () = assert!(TARGET_HOPS <= MAX_PATH_HOPS);

/// How many entry relays a client pins at once. Two: one minimises IP exposure,
/// but Spot nodes churn, and the second is only used when the first is down.
pub(crate) const GUARD_SET_SIZE: usize = 2;

/// How long a guard stays pinned before it is rotated out on purpose: a ceiling
/// on how long one node can observe a client's IP. In practice guards rotate
/// when they leave the directory first.
pub(crate) const GUARD_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;

/// Who runs a relay; it decides which circuit positions the relay may occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayTier {
    /// Ours (signed directory or static config); the only tier allowed to be a
    /// guard or an exit.
    FirstParty,
    /// A peer-hosted relay: middle hops only, at most one per path. Produced only
    /// from the signed directory's `peers`
    /// ([`crate::net::overlay::directory_to_endpoints`]); an unsigned channel
    /// could feed a target hand-picked relays.
    Peer,
}

/// A resolved relay endpoint: where to dial, its claimed region, the pinned QUIC
/// leaf it is verified against (its identity), and who runs it.
#[derive(Clone)]
pub(crate) struct RelayEndpoint {
    /// As configured/advertised; resolved per dial. For a peer, the rendezvous
    /// relay holding its parked link; if that link is gone, the dial reaches a
    /// relay whose leaf fails the pinned fingerprint: a clean `ExtendFailed`.
    pub(crate) addr: String,
    /// Advertised region, a diversity hint only; unknown is never diverse from
    /// unknown.
    pub(crate) region: String,
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) tier: RelayTier,
    /// For a peer: the first-party relays it is parked at. Empty means "no
    /// constraint" (v1 parks everywhere); a peer parked nowhere is never an
    /// endpoint. Always empty for a first-party endpoint.
    parked_at: Vec<String>,
}

impl RelayEndpoint {
    /// A peer-hosted endpoint reachable from any first-party relay.
    #[allow(dead_code)]
    pub(crate) fn peer(
        addr: String,
        region: String,
        cert: CertificateDer<'static>,
    ) -> RelayEndpoint {
        RelayEndpoint::peer_parked_at(addr, region, cert, Vec::new())
    }

    /// A peer-hosted endpoint reachable only from `parked_at`, with `addr` one
    /// of those relays (what the signed directory produces).
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

    /// A first-party endpoint (from the directory or the static config).
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

    /// Can the relay at `addr` reach this endpoint as its next hop? Always for a
    /// first-party relay; for a peer, when that relay holds its parked link or
    /// the peer names no parking.
    fn reachable_from(&self, addr: &str) -> bool {
        match self.tier {
            RelayTier::FirstParty => true,
            RelayTier::Peer => self.parked_at.is_empty() || self.parked_at.iter().any(|a| a == addr),
        }
    }

    /// The "same node" key for path assembly. First-party nodes by address
    /// (every hydra node currently shares one pinned leaf); peers by cert (their
    /// `addr` is a rendezvous shared with the relay holding their link).
    fn identity_key(&self) -> String {
        match self.tier {
            RelayTier::FirstParty => format!("addr:{}", self.addr),
            RelayTier::Peer => format!("peer:{}", self.cert_digest_b64()),
        }
    }

    /// Base64 of SHA-256 over the DER leaf: the revocation selector and the
    /// identity half of a guard pin.
    pub(crate) fn cert_digest_b64(&self) -> String {
        use base64::Engine as _;
        let digest = pollis_relay::proto::cert_fingerprint(self.cert.as_ref());
        base64::engine::general_purpose::STANDARD.encode(digest)
    }
}

/// An endpoint proven to be first-party; [`FirstPartyEndpoint::new`] returns
/// `None` for a peer, so a path with a peer exit does not typecheck.
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

/// An ordered relay path, client-outward: [`RelayPath::Single`] (one hop, for
/// pools too small to leave a spare) or [`RelayPath::Multi`] (a guard, middles
/// where peers may sit, and an exit). Both terminal positions are
/// [`FirstPartyEndpoint`].
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
    /// A multi-hop path. Rejects, rather than repairs, a path that repeats a
    /// node, carries more than one peer, or exceeds [`MAX_PATH_HOPS`].
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
        // At most one peer: a second adds no separation but doubles the Sybil
        // odds. Checked before the hop cap, which today implies it, so that
        // losing this rule would be visible.
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

    /// The path's endpoints in dial order, client-outward; the exit is always
    /// last.
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

    /// The hop that talks to the destination. First-party by construction.
    #[allow(dead_code)]
    pub(crate) fn exit(&self) -> &FirstPartyEndpoint {
        match self {
            RelayPath::Single(exit) => exit,
            RelayPath::Multi { exit, .. } => exit,
        }
    }

    /// Resolve every endpoint to a dialable `SocketAddr` and produce the
    /// `Vec<Hop>` `Circuit::build` consumes, in the same client-outward order.
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

/// How many hops to build for a pool of `usable` admissible relays. Leaves a
/// spare node to fail over to: messages must work, so a 2-node pool builds
/// single-hop.
pub(crate) fn desired_hops(usable: usize, target: usize) -> usize {
    usable.saturating_sub(1).clamp(1, target.min(MAX_PATH_HOPS))
}

/// How many hops to build, given the admissible first-party count and whether a
/// peer can take a middle slot. Only first-party relays count toward the spare
/// (only they can be guard or exit); a peer then adds one hop in a different
/// trust domain.
pub(crate) fn hops_for(first_party_usable: usize, peer_available: bool, target: usize) -> usize {
    let base = desired_hops(first_party_usable, target);
    if base > 1 && peer_available {
        return (base + 1).min(target).min(MAX_PATH_HOPS);
    }
    base
}

/// Does a pool of this shape build multi-hop paths (and so need guard pins)?
/// `usable` indexes `pool`.
pub(crate) fn is_multi_hop(pool: &[RelayEndpoint], usable: &[usize], target: usize) -> bool {
    let first_party = usable
        .iter()
        .filter(|&&i| pool[i].tier == RelayTier::FirstParty)
        .count();
    desired_hops(first_party, target) > 1
}

/// Pick the guard for attempt `attempt`, given the pinned guard order and the
/// chosen exit; an index into `pool`. Guards rotate per attempt within one
/// connect, so a dead guard costs one dial and the dial count is unchanged.
fn pick_guard(guards: &[usize], attempt: usize, exit: usize) -> Option<usize> {
    if guards.is_empty() {
        return None;
    }
    // Skip the guard that is the exit: one node would see both ends.
    (0..guards.len())
        .map(|i| guards[(attempt + i) % guards.len()])
        .find(|&g| g != exit)
}

/// Choose `count` middle hops from `candidates` (indices into `pool`), avoiding
/// anything on the path. Prefers a peer (at most one, where
/// [`peer_position_admissible`] allows and the guard can reach it), then an
/// unrepresented region. Region diversity is only a preference: today's
/// first-party pool is single-region.
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
                // The single statement of where a peer may sit.
                !peer_taken
                    && crate::net::peer::discovery::peer_position_admissible(position, path_len)
                    && pool[i].reachable_from(guard_addr)
            })
            // A peer first (a different trust domain is why the hop exists),
            // then diverse regions, then pool order.
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

/// Build the path for one dial attempt. `order` is the admissible candidate
/// order (revocation-filtered), `exit` the terminal candidate for this attempt,
/// `guards` the pinned guard order. `None` when no valid path exists, notably
/// when `exit` is a peer.
pub(crate) fn plan_path(
    pool: &[RelayEndpoint],
    order: &[usize],
    guards: &[usize],
    exit: usize,
    attempt: usize,
    target_hops: usize,
) -> Option<RelayPath> {
    let exit_fp = FirstPartyEndpoint::new(pool.get(exit)?)?;
    let first_party_usable = order
        .iter()
        .filter(|&&i| pool[i].tier == RelayTier::FirstParty)
        .count();
    if desired_hops(first_party_usable, target_hops) <= 1 {
        return Some(RelayPath::Single(exit_fp));
    }
    let guard = pick_guard(guards, attempt, exit)?;
    // Guards come from the first-party set, but re-prove it: the entry hop
    // sees the client's IP.
    let guard_fp = FirstPartyEndpoint::new(pool.get(guard)?)?;
    // A peer lengthens the path only if one is reachable from this guard.
    let peer_available = order.iter().any(|&i| {
        i != guard
            && i != exit
            && pool[i].tier == RelayTier::Peer
            && pool[i].reachable_from(&pool[guard].addr)
    });
    let hops = hops_for(first_party_usable, peer_available, target_hops);
    // Shorten rather than fail when middles run out.
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

/// One pinned guard, as persisted. Identity is `addr` and cert digest: a re-keyed
/// node at the same address is a different relay.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinnedGuard {
    addr: String,
    cert_sha256_b64: String,
    /// Unix seconds when this guard was first pinned.
    pinned_at: i64,
}

/// The on-disk shape, versioned so a future policy can migrate it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct GuardFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    guards: Vec<PinnedGuard>,
}

const GUARD_FILE_VERSION: u32 = 1;

/// The client's pinned entry relays, persisted across restarts (§4.4a): a fresh
/// first hop per circuit eventually draws every relay, hostile ones included.
///
/// A guard is replaced when it leaves the directory, becomes inadmissible, or
/// outlives [`GUARD_LIFETIME_SECS`], but never for being unreachable: evicting
/// on failure is the guard-discovery attack. A down guard just yields to the
/// other pin for that attempt.
pub(crate) struct GuardBook {
    /// `None` ⇒ in-memory only (tests, and the static config path).
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

    /// The pinned guard order for `pool`, as indices into it. Drops pins that no
    /// longer match a usable member or have expired, then tops up to
    /// [`GUARD_SET_SIZE`] at random from first-party `usable` members,
    /// preferring an unpinned region.
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
        // `pinned_at` is compared too: a re-pinned guard's fresh clock must be
        // written back, or it looks expired again on restart and re-rolls.
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

    /// When each held pin was taken (re-pinning is a new pin with a new clock).
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
        // A corrupt or foreign file means "no pins".
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
        // Best effort; owner-only, since the pins reveal where its owner
        // connects from.
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

    /// A peer endpoint cannot become a `FirstPartyEndpoint`.
    #[test]
    fn a_peer_cannot_become_a_first_party_endpoint() {
        assert!(FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).is_some());
        assert!(FirstPartyEndpoint::new(&peer("p:1", "us", 2)).is_none());
    }

    /// Over every pool shape, every path the selector produces ends on a
    /// first-party hop.
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
                        // The entry hop is first-party too.
                        assert_eq!(hops[0].tier, RelayTier::FirstParty);
                    }
                }
            }
        }
    }

    /// Asked to exit through a peer, the selector returns no path at all.
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

    /// An all-peer pool yields no path (fail-closed).
    #[test]
    fn an_all_peer_pool_yields_no_path() {
        let pool = vec![peer("p:1", "eu", 9), peer("p:2", "ap", 8)];
        let order = all(2);
        for exit in 0..2 {
            assert!(plan_path(&pool, &order, &[], exit, 0, TARGET_HOPS).is_none());
        }
    }

    /// Peers are usable as middles, between two first-party hops.
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

    /// At most one peer per path, held by the constructor. Asserts the reason:
    /// today the hop cap would reject a two-peer path anyway.
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

    /// The selector never puts two peers on a path either.
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

    /// A peer sharing a hop's rendezvous address is not a repeat.
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
        // Two first-party hops at one address still are a repeat.
        let a = FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).unwrap();
        let b = FirstPartyEndpoint::new(&first_party("b:1", "us", 2)).unwrap();
        assert!(RelayPath::multi(a, vec![first_party("a:1", "us", 9)], b).is_err());
    }

    /// A peer parked elsewhere is not reachable from this guard; the path
    /// shortens rather than failing.
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

    /// Peers do not count toward the spare rule; a peer adds a hop instead.
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

    /// The guard is never also the exit, even when the exit is a pinned guard.
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

    /// Guards rotate per attempt, so a dead primary costs one dial.
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
        // "far" wins on region despite sorting later in `order`.
        let picked = pick_middles(&pool, &all(4), &[0, 1], 1, 3, "a:1");
        assert_eq!(picked, vec![3]);
    }

    /// Pins are stable across selections and survive a reload.
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

    /// A guard leaving the pool is replaced; the survivor keeps its pin.
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

        // At the lifetime both pins re-take with the new `now`.
        let now = 1_000 + GUARD_LIFETIME_SECS;
        let later = book.guards_for(&pool, &all(4), now);
        assert_eq!(later.len(), GUARD_SET_SIZE);
        assert_eq!(book.pinned_at(), vec![now, now], "an expired pin was kept");
    }

    /// Being unreachable is not grounds for un-pinning (the guard-discovery
    /// attack); "down" is expressed through health ordering only.
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

    /// A peer is never pinned as a guard.
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
