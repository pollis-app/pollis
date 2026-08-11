//! Relay **path selection** and **guard pinning** — issue #813 Phase B, design
//! `docs/relay-overlay-design.md` §6.2 (multi-hop onion) and §4.4 (Sybil).
//!
//! `pollis-relay` owns the circuit *mechanism* (`Circuit::build` telescopes an
//! ordered `Vec<Hop>` and runs one nested TLS layer per hop). Which relays a
//! circuit is made of is **policy**, and policy lives here.
//!
//! Three rules this module exists to hold:
//!
//! 1. **The last hop is first-party.** §4.4's structural mitigation: because
//!    every destination is one of our own hosts, we can require the hop that
//!    talks to it to be ours, which means a Sybil adversary can never be the
//!    exit — half the both-ends attack removed for free. Nothing in
//!    `pollis-relay` can enforce this (the relay's destination allowlist stops a
//!    peer exit *reaching* anything interesting, but choosing the terminal hop is
//!    ours to get right). It is enforced here **as a type**: [`RelayPath`] stores
//!    its terminal hop in a [`FirstPartyEndpoint`] field, a newtype whose only
//!    constructor rejects a peer-tier endpoint, and [`RelayPath::endpoints`]
//!    always emits that field last. There is no way to *express* a path whose
//!    exit is a peer, so it cannot be violated by accident.
//! 2. **The entry hop is a pinned guard.** A freshly-random first hop every
//!    circuit maximises the chance of *eventually* drawing a hostile one; a
//!    small stable pinned set bounds that (§4.4a). See [`GuardBook`].
//! 3. **A path never repeats a node.** Two hops that are the same relay collapse
//!    the circuit back to one trust unit; entry == exit is exactly the both-ends
//!    position multi-hop exists to prevent. [`RelayPath::multi`] rejects it.
//!
//! Peer-hosted relays (phase D1) are [`RelayTier::Peer`] and are selectable as
//! **middle hops only** — never the guard, never the exit.

use std::path::PathBuf;
use std::sync::Mutex;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use pollis_relay::circuit::Hop;
use pollis_relay::CertificateDer;

/// The path length we aim for when the pool can support it.
///
/// **Two, not three.** Tor needs three because its exit is untrusted and must be
/// kept in a different trust domain from the guard; here the exit is *required*
/// to be first-party (§4.4) and the guard is first-party too, so two hops
/// already deliver the property multi-hop is for — no single relay holds both
/// the client's IP and the destination. A third hop would add a full RTT of
/// circuit-build latency (telescoping pays one RTT per hop) to buy separation
/// between two nodes we already run. The slot exists — [`MAX_PATH_HOPS`] and
/// [`RelayPath::multi`]'s `middles` are real and tested — so that once D1 puts
/// *peer* relays in the pool, a middle from a genuinely different trust domain
/// can be inserted without reshaping any of this.
pub(crate) const TARGET_HOPS: usize = 2;

/// Hard cap on a selected path, below the crate's own `MAX_HOPS` ceiling. Every
/// hop is an RTT on the control plane's latency budget (§6.4), so the selector
/// refuses to grow paths even if a future policy asks it to.
pub(crate) const MAX_PATH_HOPS: usize = 3;

// The selector can never ask the transport for more hops than it supports.
const _: () = assert!(MAX_PATH_HOPS <= pollis_relay::MAX_HOPS);
const _: () = assert!(TARGET_HOPS <= MAX_PATH_HOPS);

/// How many entry relays a client pins at once.
///
/// **Two.** One guard minimises the number of relays that ever see this client's
/// IP (the Tor argument), but a pool whose nodes are Spot instances loses members
/// routinely — a single pinned guard would strand the client on the direct
/// fallback every time its one guard rotated out. Two keeps the exposure set
/// small and constant while leaving a live spare, and the second guard is only
/// *used* when the first is unreachable, so the steady-state exposure is still
/// effectively one.
pub(crate) const GUARD_SET_SIZE: usize = 2;

/// How long a guard stays pinned before it is rotated out on purpose.
///
/// **30 days.** Tor pins for months because its consensus is large and stable;
/// ours is a handful of first-party nodes that already churn on Spot reclamation,
/// so in practice guards rotate when they leave the directory, not when this
/// expires. The lifetime is therefore a *ceiling* on how long a single node can
/// keep observing one client's IP — long enough that a client is not re-rolling
/// its entry set (which is what makes guards worth having), short enough that a
/// quietly-compromised node's window is bounded.
pub(crate) const GUARD_LIFETIME_SECS: i64 = 30 * 24 * 60 * 60;

/// Who runs a relay. The distinction is load-bearing: it decides what positions
/// in a circuit the relay may occupy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelayTier {
    /// Ours: published by the signed directory (or the static v0 config). The
    /// only tier allowed to be a guard or an exit.
    FirstParty,
    /// A peer-hosted relay (phase D1). **Middle hops only** — never terminal.
    ///
    /// Constructed by [`RelayEndpoint::peer`], whose only producer today is the
    /// test suite: D1 owns peer discovery and has not landed. The variant exists
    /// now, and is exercised now, because the position restriction it carries is
    /// exactly the thing that must already be true when peers do arrive.
    Peer,
}

/// A resolved relay endpoint: where to dial, which region it claims, the pinned
/// QUIC leaf the client verifies it against (the relay's identity *is* its cert,
/// §7), and who runs it.
#[derive(Clone)]
pub(crate) struct RelayEndpoint {
    /// As configured/advertised; resolved to a `SocketAddr` per dial.
    pub(crate) addr: String,
    /// Advertised region. Only ever used as a *diversity* hint — an empty or
    /// unknown region is never treated as diverse from another unknown one.
    pub(crate) region: String,
    pub(crate) cert: CertificateDer<'static>,
    pub(crate) tier: RelayTier,
}

impl RelayEndpoint {
    /// A peer-hosted endpoint (phase D1's discovery will produce these).
    /// Selectable as a middle hop only — never a guard, never an exit.
    #[allow(dead_code)]
    pub(crate) fn peer(
        addr: String,
        region: String,
        cert: CertificateDer<'static>,
    ) -> RelayEndpoint {
        RelayEndpoint {
            addr,
            region,
            cert,
            tier: RelayTier::Peer,
        }
    }

    /// A first-party endpoint, the shape the directory and the static config
    /// produce.
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
        }
    }

    /// Base64 (STANDARD) of SHA-256 over this endpoint's DER leaf — the same
    /// selector the revocation list uses, and the identity half of a guard pin.
    pub(crate) fn cert_digest_b64(&self) -> String {
        use base64::Engine as _;
        let digest = pollis_relay::proto::cert_fingerprint(self.cert.as_ref());
        base64::engine::general_purpose::STANDARD.encode(digest)
    }
}

/// An endpoint proven to be first-party.
///
/// The ONLY way to build one is [`FirstPartyEndpoint::new`], which returns
/// `None` for a peer. Carrying that proof in the type — rather than checking a
/// `tier` field at the point of use — is what makes "the last hop is first-party"
/// impossible to violate by accident: [`RelayPath`]'s terminal field has this
/// type, so a path with a peer exit does not typecheck.
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
/// The two shapes are distinct types rather than a `Vec` with rules attached,
/// because the rules are the whole point:
/// - [`RelayPath::Single`] is the v0 path — one first-party hop that is both
///   entry and exit. Used when the pool is too small to give a multi-hop path a
///   spare node to fail over to.
/// - [`RelayPath::Multi`] is the §6.2 path — a first-party **guard**, zero or
///   more middles (peers allowed here and nowhere else), and a first-party
///   **exit**.
///
/// Both terminal positions are [`FirstPartyEndpoint`], so §4.4's
/// "the last hop is always ours" is a property of the type, not of a check
/// somebody has to remember to run.
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
    /// A multi-hop path. Rejects (rather than silently repairing) a path that
    /// repeats a node or exceeds [`MAX_PATH_HOPS`] — a repeated node means one
    /// operator occupies two positions, and guard == exit is precisely the
    /// both-ends position the design exists to prevent.
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
        if len > MAX_PATH_HOPS {
            anyhow::bail!("relay path of {len} hops exceeds the {MAX_PATH_HOPS}-hop cap");
        }
        let mut seen: Vec<&str> = Vec::with_capacity(len);
        for endpoint in path.endpoints() {
            if seen.contains(&endpoint.addr.as_str()) {
                anyhow::bail!("relay path repeats node {}", endpoint.addr);
            }
            seen.push(&endpoint.addr);
        }
        Ok(path)
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            RelayPath::Single(_) => 1,
            RelayPath::Multi { middles, .. } => 2 + middles.len(),
        }
    }

    /// The path's endpoints in dial order, client-outward. The exit is emitted
    /// last here and only here, so no caller can reorder it into the middle.
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

/// How many hops to build for a pool of `usable` admissible relays.
///
/// The rule is **leave a spare**: a path consumes one distinct node per hop, and
/// a pool with no node left over has nothing to fail over to when one member
/// dies. "Messages must work" outranks path length, so a 2-node pool builds
/// single-hop rather than a 2-hop path with zero redundancy.
pub(crate) fn desired_hops(usable: usize, target: usize) -> usize {
    usable.saturating_sub(1).clamp(1, target.min(MAX_PATH_HOPS))
}

/// Pick the guard to use for attempt `attempt`, given the pinned guard order and
/// the exit already chosen. Returns an index into `pool`.
///
/// Guards rotate **per attempt within one connect**, not per connect: that way a
/// dead guard costs at most one wasted dial before the backup is tried, and the
/// total number of dials a single `connect` performs is unchanged from the
/// single-hop pool (so the fail-open worst-case latency does not regress).
fn pick_guard(guards: &[usize], attempt: usize, exit: usize) -> Option<usize> {
    if guards.is_empty() {
        return None;
    }
    // Rotate, then skip the guard that IS the exit — a circuit whose first and
    // last hop are the same relay hands that one node both the client's IP and
    // the destination, which is the entire attack multi-hop prevents.
    (0..guards.len())
        .map(|i| guards[(attempt + i) % guards.len()])
        .find(|&g| g != exit)
}

/// Choose `count` middle hops from `candidates` (indices into `pool`), avoiding
/// anything already on the path and preferring regions not already represented.
///
/// Region diversity is a *preference*, not a requirement: today's first-party
/// pool is single-region, and refusing to build a path because two of our own
/// nodes share a region would mean never using the overlay at all. A distinct
/// node is still a distinct trust unit against §4.3 (one compromised relay), and
/// that is the property a same-region second hop still buys.
fn pick_middles(
    pool: &[RelayEndpoint],
    candidates: &[usize],
    on_path: &[usize],
    count: usize,
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
    for _ in 0..count {
        let next = candidates
            .iter()
            .copied()
            .filter(|i| !taken.contains(i))
            // Diverse regions first; `position` order (health/latency) breaks ties.
            .min_by_key(|&i| {
                let region = pool[i].region.as_str();
                let duplicate = !region.is_empty() && regions.contains(&region);
                (duplicate as u8, candidates.iter().position(|&c| c == i).unwrap_or(usize::MAX))
            });
        match next {
            Some(i) => {
                if !pool[i].region.is_empty() {
                    regions.push(pool[i].region.as_str());
                }
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
/// Returns `None` when no valid path exists for this exit — most importantly
/// when the candidate is not first-party, which is how a peer relay is refused
/// the terminal position at the one place paths are built.
pub(crate) fn plan_path(
    pool: &[RelayEndpoint],
    order: &[usize],
    guards: &[usize],
    exit: usize,
    attempt: usize,
    target_hops: usize,
) -> Option<RelayPath> {
    // The exit must be first-party. This is the type-level gate: a peer endpoint
    // cannot produce a `FirstPartyEndpoint`, so it cannot terminate a path.
    let exit_fp = FirstPartyEndpoint::new(pool.get(exit)?)?;
    let hops = desired_hops(order.len(), target_hops);
    if hops <= 1 {
        return Some(RelayPath::Single(exit_fp));
    }
    let guard = pick_guard(guards, attempt, exit)?;
    // A guard is only ever drawn from the first-party set (see
    // `GuardBook::guards_for`), but re-prove it here rather than assume it: the
    // entry hop is the one that sees the client's IP.
    let guard_fp = FirstPartyEndpoint::new(pool.get(guard)?)?;
    // Shorten rather than fail when the pool cannot supply enough distinct
    // middles — a 2-hop path still beats falling back to a direct dial.
    let middles = pick_middles(pool, order, &[guard, exit], hops - 2)
        .into_iter()
        .map(|i| pool[i].clone())
        .collect();
    RelayPath::multi(guard_fp, middles, exit_fp).ok()
}

// ── Guard pinning (§4.4a) ──────────────────────────────────────────────────

/// One pinned guard, as persisted. Identity is `addr` **and** cert digest: if a
/// node keeps the address but presents a different leaf it is a different relay,
/// and the pin must not silently transfer to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct PinnedGuard {
    addr: String,
    cert_sha256_b64: String,
    /// Unix seconds when this guard was first pinned.
    pinned_at: i64,
}

/// The on-disk shape. Versioned so a future rotation policy can migrate rather
/// than silently reinterpret an old file.
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
/// **Why pin at all (§4.4a).** Drawing a fresh first hop every circuit means
/// that over enough circuits a client will eventually route through *every*
/// relay in the pool, including any hostile one — the probability of never
/// touching a bad guard decays toward zero. Pinning a small set converts that
/// into a one-off draw: either the pinned guards are honest (and stay honest for
/// the whole pin) or they are not, and the client's exposure stops growing with
/// usage.
///
/// **Rotation policy.** A guard is replaced when it (a) leaves the signed
/// directory, (b) is revoked or otherwise inadmissible, or (c) has been pinned
/// longer than [`GUARD_LIFETIME_SECS`]. It is deliberately **not** replaced for
/// being merely unreachable: evicting on failure is the classic guard-discovery
/// attack — an adversary who can knock your guards offline can walk you onto
/// theirs. Unreachability is handled by rotating to the *other* pinned guard for
/// that attempt, never by un-pinning.
pub(crate) struct GuardBook {
    /// `None` ⇒ in-memory only (tests, and the static v0 config path where the
    /// pool is too small for a guard to matter).
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
    /// Pins that no longer match a *usable* pool member (gone from the directory,
    /// re-keyed, revoked — `usable` is the caller's admissible set) or that have
    /// outlived [`GUARD_LIFETIME_SECS`] are dropped; the set is then topped back
    /// up to [`GUARD_SET_SIZE`] from the first-party members of `usable`, chosen
    /// at random so a pool-wide pin stampede is impossible, preferring a region
    /// not already pinned.
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

        // (1) Keep pins that still match a usable, unexpired, first-party member.
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

        // (2) Top up from the first-party members we are not already pinning.
        let mut chosen: Vec<usize> = kept.iter().map(|(_, i)| *i).collect();
        if chosen.len() < GUARD_SET_SIZE {
            let mut fresh: Vec<usize> = usable
                .iter()
                .copied()
                .filter(|&i| pool[i].tier == RelayTier::FirstParty && !chosen.contains(&i))
                .collect();
            fresh.shuffle(&mut rand::thread_rng());
            // Prefer a region we are not already pinning; the shuffle above keeps
            // the choice within a region uniform across clients.
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
        // `pinned_at` is part of the comparison: a guard re-pinned after its
        // lifetime expired is a NEW pin, and if its refreshed clock were not
        // written back it would look expired again on the next restart and
        // re-roll forever.
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

    /// When each currently-held pin was taken. Rotation is otherwise invisible
    /// through the index-returning API: re-pinning the same relay is a *new* pin
    /// with a fresh clock, which is exactly what the lifetime rule is about.
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
        // A corrupt/foreign file means "no pins" — re-pinning is cheap and
        // strictly safer than trusting bytes we cannot parse.
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
            let _ = std::fs::create_dir_all(parent);
        }
        // Best effort: losing the pin file costs a re-pin, never correctness.
        if let Err(e) = std::fs::write(path, bytes) {
            eprintln!("[overlay] could not persist guard pins: {e}");
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

    // ── The hard invariant: the last hop is first-party ─────────────────────

    /// The type-level gate itself: a peer endpoint cannot become the proof-
    /// carrying type that `RelayPath`'s terminal fields require.
    #[test]
    fn a_peer_cannot_become_a_first_party_endpoint() {
        assert!(FirstPartyEndpoint::new(&first_party("a:1", "us", 1)).is_some());
        assert!(FirstPartyEndpoint::new(&peer("p:1", "us", 2)).is_none());
    }

    /// THE INVARIANT, exhaustively: over every pool shape the selector can be
    /// handed — all first-party, mixed, peers first, peers last — every path it
    /// produces ends on a first-party hop.
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
                        // The entry hop is first-party too — it is the one that
                        // sees the client's address.
                        assert_eq!(hops[0].tier, RelayTier::FirstParty);
                    }
                }
            }
        }
    }

    /// A peer candidate is never given the terminal position: asked to use one
    /// as the exit, the selector returns no path at all rather than a path that
    /// exits through a third party.
    #[test]
    fn a_peer_candidate_yields_no_path_as_exit() {
        let pool = vec![
            first_party("a:1", "us", 1),
            first_party("b:1", "us", 2),
            peer("p:1", "eu", 9),
        ];
        let order = all(3);
        assert!(plan_path(&pool, &order, &[0, 1], 2, 0, TARGET_HOPS).is_none());
        // ...while a first-party candidate in the same pool does produce one.
        assert!(plan_path(&pool, &order, &[0, 1], 1, 0, TARGET_HOPS).is_some());
    }

    /// A pool of nothing but peers produces no path — fail-closed, so `Prefer`
    /// dials direct and `Strict` degrades rather than exiting through a peer.
    #[test]
    fn an_all_peer_pool_yields_no_path() {
        let pool = vec![peer("p:1", "eu", 9), peer("p:2", "ap", 8)];
        let order = all(2);
        for exit in 0..2 {
            assert!(plan_path(&pool, &order, &[], exit, 0, TARGET_HOPS).is_none());
        }
    }

    /// Peers ARE usable as middles — the tier is a position restriction, not a
    /// ban. Forcing a 3-hop target with a peer available puts it in the middle,
    /// between two first-party hops.
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
        // The middle is the region-diverse candidate, and it may be a peer.
        assert_eq!(hops[1].addr, "p:1");
        assert_eq!(hops[1].tier, RelayTier::Peer);
        assert_eq!(hops[2].addr, "b:1");
        assert_eq!(hops[2].tier, RelayTier::FirstParty);
    }

    // ── Path shape ─────────────────────────────────────────────────────────

    /// Leave-a-spare: a pool with no node to fail over to builds single-hop.
    #[test]
    fn hop_count_leaves_a_spare() {
        assert_eq!(desired_hops(1, TARGET_HOPS), 1);
        assert_eq!(desired_hops(2, TARGET_HOPS), 1);
        assert_eq!(desired_hops(3, TARGET_HOPS), 2);
        assert_eq!(desired_hops(9, TARGET_HOPS), 2);
        // A larger target still leaves a spare, and never exceeds the cap.
        assert_eq!(desired_hops(3, 3), 2);
        assert_eq!(desired_hops(4, 3), 3);
        assert_eq!(desired_hops(9, 99), MAX_PATH_HOPS);
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
        // The guard that is the exit is skipped, not returned.
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
        // On-path = {0 (us-west-2), 1 (us-west-2)}; "same" duplicates the region,
        // "far" does not, so "far" wins despite sorting later in `order`.
        let picked = pick_middles(&pool, &all(4), &[0, 1], 1);
        assert_eq!(picked, vec![3]);
    }

    // ── Guard pinning ──────────────────────────────────────────────────────

    /// Pinning actually pins: repeated selections over a stable pool return the
    /// same guard set, and it survives a reload from disk.
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
        // Stable across repeated selection — this is the whole point of a guard.
        for _ in 0..20 {
            assert_eq!(book.guards_for(&pool, &usable, 1_000), first);
        }
        // ...and across a restart: a brand-new book at the same path reloads it.
        let reloaded = GuardBook::at(path.clone());
        assert_eq!(reloaded.guards_for(&pool, &usable, 1_000), first);
        let _ = std::fs::remove_file(&path);
    }

    /// A guard that leaves the pool (or is filtered out as revoked) is replaced;
    /// the surviving guard keeps its pin rather than the whole set re-rolling.
    #[test]
    fn a_guard_that_leaves_the_pool_is_replaced() {
        let pool: Vec<RelayEndpoint> = (0..4)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let book = GuardBook::ephemeral();
        let pinned = book.guards_for(&pool, &all(4), 1_000);
        assert_eq!(pinned.len(), 2);

        // Drop the first pinned guard from the usable set (gone / revoked).
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

        // Just inside the lifetime: the same pins, with their original clocks.
        book.guards_for(&pool, &all(4), 1_000 + GUARD_LIFETIME_SECS - 1);
        assert_eq!(book.pinned_at(), vec![1_000, 1_000], "pin rotated too early");

        // At the lifetime: both pins are dropped and re-taken, so every pin's
        // clock is the new `now` — a re-pinned relay is a NEW pin, not a kept one.
        let now = 1_000 + GUARD_LIFETIME_SECS;
        let later = book.guards_for(&pool, &all(4), now);
        assert_eq!(later.len(), GUARD_SET_SIZE);
        assert_eq!(book.pinned_at(), vec![now, now], "an expired pin was kept");
    }

    /// Being *unreachable* is not grounds for un-pinning — evicting on failure
    /// is the guard-discovery attack (knock the guards over, walk the client
    /// onto yours). The caller expresses "down" via health ordering, never by
    /// removing the guard from the usable set.
    #[test]
    fn a_guard_is_not_un_pinned_for_being_down() {
        let pool: Vec<RelayEndpoint> = (0..4)
            .map(|i| first_party(&format!("n{i}:9444"), "us-west-2", i as u8 + 1))
            .collect();
        let book = GuardBook::ephemeral();
        let pinned = book.guards_for(&pool, &all(4), 1_000);
        // Same usable set (the pool still advertises them), many selections.
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
