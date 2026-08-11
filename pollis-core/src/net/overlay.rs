//! Closed-overlay relay wiring for `pollis-core` (design
//! `docs/relay-overlay-design.md` §14). This is the CONSUMER side of the
//! `pollis-relay` transport crate: it derives the routing policy from `Config`,
//! builds the real circuit factory + starts the loopback SOCKS5 shim
//! ([`start_overlay_shim`]), hands out the shared reqwest client, and builds the
//! libsql SOCKS connector. The runtime on/off/switch engine that DRIVES this lives
//! in [`crate::commands::overlay`] (`set_overlay_mode` / `apply_overlay_mode`).
//!
//! **Off-by-default is sacred.** With `POLLIS_OVERLAY` unset (`OverlayMode::Off`)
//! no shim is ever started, `AppState.overlay` stays `None`, and every network
//! path is byte-for-byte identical to a build without the overlay: [`http_client`]
//! returns a proxy-less `reqwest::Client`, and `RemoteDb::connect` takes libsql's
//! unchanged `.build()` path (no `.connector()`).

use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use hyper::client::connect::{Connected, Connection};
use ml_dsa::Keypair;
use hyper::Uri;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::JoinHandle;
use tower_service::Service;

use super::directory;
use super::path::{self, GuardBook, RelayEndpoint, RelayPath};
use super::revocation::RelayAdmission;

use pollis_relay::circuit::{Circuit, CircuitFactory};
use pollis_relay::client::ClientIdentity;
use pollis_relay::proto::DeviceCertMaterial;
use pollis_relay::stream::BoxedStream;
use pollis_relay::{
    Allowlist, CertificateDer, OverlayHandle, OverlayMode, OverlayShim, RoutingPolicy,
};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::state::AppState;

// ── The shared reqwest seam (design §14.2) ─────────────────────────────────

/// The one HTTP client every control-plane caller uses instead of
/// `reqwest::Client::new()`. When `overlay` is `Some`, the client is pointed at
/// the loopback SOCKS5 shim (`socks5h://`, proxy-side DNS) so the real hostname
/// reaches the relay and the inner TLS still terminates at the real service; when
/// `None`, it is a plain client — identical to `reqwest::Client::new()`.
///
/// Concentrating every call site here also retires the per-call
/// `reqwest::Client::new()` anti-pattern (design §14.2).
pub(crate) fn http_client(overlay: Option<&OverlayHandle>) -> reqwest::Client {
    pollis_relay::http::http_client(overlay)
}

// ── Policy derivation (the plane split, design §6.4) ───────────────────────

/// Extract the bare host from a URL (`libsql://`, `https://`, `wss://`, or a
/// bare `host:port`). Ports, schemes, paths, and queries are stripped — the
/// allowlist matches on host only, and that is what the shim sees per request.
fn host_of(url: &str) -> Option<String> {
    let after_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let host = after_scheme
        .split(['/', ':', '?', '#'])
        .next()
        .unwrap_or("");
    if host.is_empty() {
        None
    } else {
        Some(host.to_ascii_lowercase())
    }
}

/// Build the per-host routing policy from `Config` for a given runtime `mode`:
/// the first-party control plane (Turso + optional commit-log DB, the DS, R2)
/// routes through the overlay, and so does **LiveKit signaling** (#813 phase E1);
/// LiveKit **media** stays direct in every mode (§6.4). Any host not on either
/// list is dialed direct (e.g. non-first-party Expo push, §14.4). The mode is
/// passed explicitly (not read from `config.overlay_mode`) because it is now a
/// RUNTIME value the shim can flip.
///
/// **The plane split is by TRANSPORT, not by host (E1).** LiveKit's signaling
/// WebSocket and its RTP media share one hostname, so no host list can separate
/// them — and the design's §6.4 rule is about the *media*, not the name it is
/// reached under. The separation that actually holds is the shim itself: it is a
/// SOCKS5 **CONNECT** proxy, i.e. TCP only, reached over loopback by callers that
/// explicitly opt in. Voice frames are UDP (RTP/ICE) emitted by the Rust media
/// stack's own sockets, which never dial the shim and could not traverse it if
/// they tried. So adding the LiveKit host here routes its *signaling* stream and
/// cannot route a media packet — proved by
/// `media_plane_stays_direct_while_signaling_is_overlaid`, which sends both over
/// the same host:port and watches which one reaches a circuit.
///
/// **What E1 does and does not buy.** It hides the signaling connection's source
/// address from the SFU's signaling endpoint. It does **not** make a voice call
/// IP-private: ICE hands the SFU the client's address by design, and media stays
/// direct on purpose. Do not describe overlaid signaling as anonymous voice.
pub(crate) fn overlay_policy(config: &Config, mode: OverlayMode) -> RoutingPolicy {
    let mut overlay_hosts: Vec<String> = Vec::new();
    let control_urls = [
        Some(config.turso_url.as_str()),
        config.log_db_url.as_deref(),
        config.pollis_delivery_url.as_deref(),
        Some(config.r2_endpoint.as_str()),
        Some(config.r2_public_url.as_str()),
        // E1: LiveKit signaling. Media to this same host stays direct because it
        // is UDP and never touches the shim — see the doc comment above.
        Some(config.livekit_url.as_str()),
    ];
    for url in control_urls.into_iter().flatten() {
        if let Some(host) = host_of(url) {
            if !overlay_hosts.contains(&host) {
                overlay_hosts.push(host);
            }
        }
    }

    RoutingPolicy::new(
        mode,
        Allowlist::from_patterns(overlay_hosts),
        Allowlist::default(),
    )
}

// ── The real circuit factory (design §9.2, §9.4, §14.1) ────────────────────

/// The `identity_version` stamped into the relay handshake cert this client
/// mints locally (see [`RealRelayFactory`]). The relay verifies a device cert for
/// **self-consistency only** — that the presented account key signed *this*
/// device key at *this* `(version, issued_at)` — and never cross-checks the value
/// against `users.identity_version`. So a fixed version is sufficient and correct
/// for the handshake; the rate limiter keys on `account_id_pub` (the real one),
/// not the version.
const OVERLAY_CERT_IDENTITY_VERSION: u32 = 1;

/// How long a relay endpoint stays marked dead after a failed dial before it is
/// eligible again. Mark-dead-on-failure + cooldown is the *event-driven*
/// alternative to a background health-poll loop (CLAUDE.md forbids periodic
/// keepalives): a dead relay is simply skipped until this window elapses, then
/// retried on the next connect that reaches it. Mirrors `RemoteDb::with_retry`'s
/// reconnect-on-demand posture — recover lazily, never poll.
const RELAY_DEAD_COOLDOWN: Duration = Duration::from_secs(30);

/// Weight of the newest dial sample in each endpoint's latency EWMA. Low enough
/// that one slow dial (a GC pause, a lossy moment) doesn't relegate an otherwise
/// fast relay, high enough that a genuinely degraded one is demoted within a
/// handful of dials.
const RELAY_LATENCY_EWMA_ALPHA: f64 = 0.25;

/// Endpoints whose measured latency is within this much of the best are treated
/// as EQUIVALENT and keep rotating between themselves. Without a band, "prefer
/// the fastest" collapses onto one node — every client in a region would pin to
/// the same relay, which is both a load-spread and an anonymity-set problem.
/// 20 ms is wide enough to cover same-region jitter and narrow enough to exclude
/// a cross-country hop (the #812 measurement saw 15 ms vs 69 ms).
const RELAY_LATENCY_BAND: Duration = Duration::from_millis(20);

/// Every Nth dial ignores latency ordering and uses plain rotation, so endpoints
/// that were slow once still get re-sampled. Without this, an endpoint demoted by
/// a transient bad measurement would never be dialed again, so its EWMA could
/// never recover — the pool would silently shrink to whoever won early.
const RELAY_EXPLORE_EVERY: usize = 20;

/// Upper bound on a single endpoint's dial (QUIC handshake + CONNECT). Without
/// it, a relay that is *unreachable* (packets dropped, no ICMP) stalls on the
/// QUIC handshake timeout — which would defeat the pool's purpose: a dead relay
/// must fail over FAST, not hang delivery. On timeout the endpoint is treated as
/// failed (marked dead) and the next candidate is tried. Generous enough for a
/// real first-party relay handshake over the internet.
const RELAY_DIAL_TIMEOUT: Duration = Duration::from_secs(8);

/// The production [`CircuitFactory`]: on each `connect`, present the logged-in
/// device's [`ClientIdentity`] and dial the configured relay, returning the
/// resulting byte pipe (over which the caller runs its own TLS to the real host).
///
/// **Identity (design §9.4).** The `ClientIdentity` carries the device ML-DSA-44
/// signing key — the SAME key `ds_client` signs DS writes with and that
/// `user_device.mls_signature_pub_pq` records (#668; before that it was the
/// Ed25519 key in `mls_signature_pub`, which is still carried and still bound by
/// the cert, but nothing signs under it) — plus the offline cert chain
/// (`account_id_pub` + `device_cert` + `version`/`issued_at`) the relay verifies
/// with zero I/O. Both halves are loaded from LOCAL state: the device signing key
/// from the open local DB (openmls storage), and the cert is minted on the spot
/// from the locally-held account identity key (`load_account_id_key`). Minting
/// locally — rather than reading the published `user_device.device_cert` through
/// `remote_db` — is deliberate: once the mode is applied, `remote_db` itself
/// routes through THIS shim, so reading the cert from it to *build* a circuit
/// would recurse into the very circuit being built. The minted cert is
/// cryptographically identical in what the relay checks (the current account key
/// certifying the real device key), and a device with NO account key yet
/// (pre-enrollment / locked / no user) simply can't mint one → `connect` errors →
/// `Prefer` falls back to direct and `Strict` degrades, never a silent send.
///
/// **Caching.** The built identity is cached (`identity`), but re-derived if the
/// cache is empty, so a device re-enroll is tolerated: `set_overlay_mode` rebuilds
/// the factory on the next apply, and each fresh factory reloads.
///
/// **Pool + failover (design §14.1, the "messages must work" slice).** The
/// factory holds a POOL of first-party relays and, per `connect`, tries them in
/// health order, returning the FIRST success; only when EVERY candidate fails
/// does it error — so `Prefer` still falls back to direct and `Strict` still
/// degrades, but only once the whole pool is exhausted, never on one dead relay.
/// Health is tracked inline (`health[i]` = `Some(dead_until)` while endpoint `i`
/// is in its cooldown after a failed dial, `None` when healthy): a failed dial
/// marks the endpoint dead for [`RELAY_DEAD_COOLDOWN`], a success clears it. There
/// is **no background poll** — recovery is lazy (the cooldown expires and the next
/// connect retries it), matching `RemoteDb::with_retry`. Selection is *fail-open*:
/// healthy endpoints are tried first, but if all are marked dead they are still
/// tried (a transient outage that marked the whole pool dead must never wedge it
/// permanently). A rotating start index (`next_start`) spreads load across healthy
/// endpoints instead of always hammering endpoint 0.
///
/// **Latency ordering (#812).** Among healthy endpoints the pool prefers the ones
/// whose measured dial latency is lowest, so a client is not sent cross-country
/// while a same-region relay is up — see [`RealRelayFactory::candidate_order`] for
/// the band/explore rules that keep load spread and stop any endpoint being
/// starved. Samples come from real dials ([`RealRelayFactory::record_latency`]);
/// there is no probe traffic and still no background poll.
///
/// **Path selection (#813 phase B).** A `connect` no longer dials one relay: it
/// picks an **exit** from the pool exactly as before (health, then latency band,
/// then explore — all of §14.1/#812 unchanged), and then asks
/// [`crate::net::path`] for the rest of the path around it — a pinned guard in
/// front, region-diverse middles if the target length calls for them. The exit is
/// still the endpoint whose health and latency are recorded, so the pool's
/// fail-open behaviour is per-attempt exactly as it was. A pool too small to
/// leave a spare node builds a single-hop path, which is byte-for-byte the v0
/// dial. **The last hop is first-party by construction** — see
/// [`crate::net::path::RelayPath`].
///
/// **Revocation (#813 phase C's client half).** Every candidate is run through
/// [`RelayAdmission`] at *dial* time. "Cannot evaluate" resolves identically to
/// "revoked", so a missing/expired/stale-vs-anchor list yields an EMPTY candidate
/// set — never the unfiltered pool — and `connect` then errors, which is the
/// existing, tested path to `Prefer`→direct and `Strict`→degrade.
struct RealRelayFactory {
    /// Weak so the factory (owned by the shim task, owned by `AppState.overlay`)
    /// does not form a reference cycle back into `AppState`.
    state: Weak<AppState>,
    endpoints: Vec<RelayEndpoint>,
    identity: AsyncMutex<Option<Arc<ClientIdentity>>>,
    /// Per-endpoint health, indexed parallel to `endpoints`: `Some(dead_until)`
    /// while in cooldown after a failed dial, `None` when healthy. A plain
    /// `std::sync::Mutex` (never held across an await) — the guard is dropped
    /// before any I/O.
    health: Mutex<Vec<Option<Instant>>>,
    /// Rotating start offset for load spread: each `connect` bumps this so the
    /// pool doesn't always begin at endpoint 0. Doubles as the explore counter
    /// (see [`RELAY_EXPLORE_EVERY`]).
    next_start: AtomicUsize,
    /// Per-endpoint dial-latency EWMA, indexed parallel to `endpoints`. `None`
    /// until that endpoint has been dialed successfully at least once — an
    /// unmeasured endpoint sorts with the fast band so it gets sampled rather
    /// than starved. Only successful dials are recorded: a failed dial's duration
    /// is a timeout, not a latency, and health already covers that case.
    latency: Mutex<Vec<Option<Duration>>>,
    /// How long a failed endpoint stays dead. `RELAY_DEAD_COOLDOWN` in production;
    /// tests inject a short value to exercise recovery.
    cooldown: Duration,
    /// Upper bound on a single dial. `RELAY_DIAL_TIMEOUT` in production; tests
    /// inject a short value so an unreachable endpoint fails over fast.
    dial_timeout: Duration,
    /// Client-side revocation gate. Shared across pool rebuilds so the store's
    /// high-water sequence (rollback protection) survives a directory refresh.
    admission: Arc<RelayAdmission>,
    /// Pinned entry relays (§4.4a), shared across pool rebuilds so a refresh
    /// does not silently re-roll the client's guards.
    guards: Arc<GuardBook>,
    /// Path length to aim for. [`path::TARGET_HOPS`] in production; tests inject
    /// a longer target to exercise middle-hop selection.
    target_hops: usize,
}

impl RealRelayFactory {
    /// Build a factory over `endpoints`, all initially healthy.
    fn new(
        state: Weak<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
        dial_timeout: Duration,
        admission: Arc<RelayAdmission>,
        guards: Arc<GuardBook>,
    ) -> Self {
        let n = endpoints.len();
        // Peers only pay for themselves as an EXTRA hop (see `path::hops_for`), so
        // the target length is raised exactly when the pool contains one. A pool
        // with no peers therefore builds byte-for-byte the paths it built before
        // wave 3 — no peers, no change.
        let target_hops = if endpoints.iter().any(|e| e.tier == path::RelayTier::Peer) {
            path::MAX_PATH_HOPS
        } else {
            path::TARGET_HOPS
        };
        RealRelayFactory {
            state,
            endpoints,
            identity: AsyncMutex::new(None),
            health: Mutex::new(vec![None; n]),
            next_start: AtomicUsize::new(0),
            latency: Mutex::new(vec![None; n]),
            cooldown,
            dial_timeout,
            admission,
            guards,
            target_hops,
        }
    }

    /// The order to try endpoints in for the next dial: healthy endpoints first
    /// (fastest-measured first, rotating among equally-fast ones so load still
    /// spreads), then any still in cooldown (fail-open — always tried, so a
    /// fully-dead pool is never permanently wedged). Returns endpoint indices.
    /// The locks are taken and dropped here; no I/O happens under them.
    ///
    /// **Why latency ordering (#812).** Health-only rotation sends a client to
    /// every healthy relay equally, wherever it is. Measured against the live
    /// pool from a US-East client, that meant a 15 ms hop half the time and a
    /// 69 ms one the other half — about +21 ms vs +123 ms of added control-plane
    /// latency, blowing the §6.4 budget on every other dial. Ordering by measured
    /// latency keeps §6.4's rule ("the user should never wait on the overlay")
    /// true by construction rather than by luck of which region the pool drew.
    ///
    /// Three properties this deliberately preserves:
    /// - **Load still spreads.** Everything within [`RELAY_LATENCY_BAND`] of the
    ///   best rotates as before, so a region's clients don't all pin to one node.
    /// - **Nothing is starved.** Unmeasured endpoints sort with the fast band, and
    ///   every [`RELAY_EXPLORE_EVERY`]th dial ignores latency entirely, so a
    ///   demoted endpoint can always earn its way back.
    /// - **Fail-open is untouched.** Latency only reorders *healthy* endpoints;
    ///   cooldowned ones are still appended and still tried.
    fn candidate_order(&self) -> Vec<usize> {
        let n = self.endpoints.len();
        let seq = self.next_start.fetch_add(1, Ordering::Relaxed);
        let start = seq % n;
        let now = Instant::now();
        let health = self.health.lock().unwrap();
        let mut healthy = Vec::with_capacity(n);
        let mut dead = Vec::new();
        for i in 0..n {
            let idx = (start + i) % n;
            let in_cooldown = health[idx].is_some_and(|until| until > now);
            if in_cooldown {
                dead.push(idx);
            } else {
                healthy.push(idx);
            }
        }
        drop(health);

        // Every Nth dial keeps the plain rotation so demoted endpoints get
        // re-sampled. `seq % N == N - 1` rather than `== 0` so the very first
        // dial of a fresh pool is a normal one.
        let exploring = seq % RELAY_EXPLORE_EVERY == RELAY_EXPLORE_EVERY - 1;
        if !exploring {
            let latency = self.latency.lock().unwrap();
            // Best *measured* latency among the healthy set. If nothing has been
            // measured yet (cold pool) there is nothing to sort by, and the
            // rotation order already computed above stands unchanged.
            if let Some(best) = healthy.iter().filter_map(|&i| latency[i]).min() {
                let cutoff = best + RELAY_LATENCY_BAND;
                // Unmeasured (`None`) counts as in-band so a fresh endpoint is
                // tried and gets a sample instead of sitting behind measured ones
                // forever. `partition` is order-preserving, so the rotation among
                // the fast band survives.
                let (fast, mut slow): (Vec<usize>, Vec<usize>) = healthy
                    .iter()
                    .copied()
                    .partition(|&i| latency[i].is_none_or(|d| d <= cutoff));
                slow.sort_by_key(|&i| latency[i]);
                healthy = fast;
                healthy.extend(slow);
            }
        }

        healthy.extend(dead);
        healthy
    }

    /// Fold a successful dial's duration into endpoint `idx`'s latency EWMA.
    fn record_latency(&self, idx: usize, sample: Duration) {
        let mut latency = self.latency.lock().unwrap();
        latency[idx] = Some(match latency[idx] {
            None => sample,
            Some(prev) => Duration::from_secs_f64(
                prev.as_secs_f64() * (1.0 - RELAY_LATENCY_EWMA_ALPHA)
                    + sample.as_secs_f64() * RELAY_LATENCY_EWMA_ALPHA,
            ),
        });
    }

    /// Mark endpoint `idx` dead until `now + cooldown` (failed dial).
    fn mark_dead(&self, idx: usize) {
        let until = Instant::now() + self.cooldown;
        self.health.lock().unwrap()[idx] = Some(until);
    }

    /// Clear endpoint `idx`'s dead mark (successful dial).
    fn mark_healthy(&self, idx: usize) {
        self.health.lock().unwrap()[idx] = None;
    }

    /// Dial along `path`: resolve every hop → build the circuit → connect to the
    /// target through it. A one-hop path is byte-for-byte the v0 exchange
    /// (`Circuit::connect` special-cases `n == 1`), so a small pool is unchanged.
    async fn dial_path(
        &self,
        path: &RelayPath,
        identity: &Arc<ClientIdentity>,
        host: &str,
        port: u16,
    ) -> anyhow::Result<BoxedStream> {
        let hops = path.resolve_hops().await?;
        let circuit = Circuit::build(hops, identity.clone())?;
        circuit.connect(host, port).await
    }

    /// The relays this connect may use at all, in try order:
    /// [`RealRelayFactory::candidate_order`] (health → latency band → explore,
    /// all unchanged) filtered through the revocation gate. A relay we cannot
    /// positively evaluate is treated as revoked, so an empty result is the
    /// fail-closed state — `connect` then errors, which is the same path an
    /// all-down pool already takes to `Prefer`→direct / `Strict`→degrade.
    fn admissible_order(&self, now_secs: i64) -> Vec<usize> {
        self.candidate_order()
            .into_iter()
            .filter(|&i| self.admission.admits(&self.endpoints[i], now_secs))
            .collect()
    }

    /// Load (or reuse the cached) device `ClientIdentity`. Errors — fail-closed —
    /// when the device isn't in a state to authenticate to a relay (no user, no
    /// local DB, locked/absent account key).
    async fn identity(&self) -> anyhow::Result<Arc<ClientIdentity>> {
        {
            let cached = self.identity.lock().await;
            if let Some(id) = cached.as_ref() {
                return Ok(id.clone());
            }
        }
        let state = self
            .state
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("overlay: app state gone"))?;
        let id = Arc::new(build_client_identity(&state).await?);
        *self.identity.lock().await = Some(id.clone());
        Ok(id)
    }
}

#[async_trait::async_trait]
impl CircuitFactory for RealRelayFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        // Fail-closed when nothing is configured (no endpoint / no pinned cert):
        // `Prefer` then dials direct and `Strict` degrades, same as today.
        if self.endpoints.is_empty() {
            anyhow::bail!("overlay: no relay endpoint / pinned cert configured");
        }
        let identity = self.identity().await?;

        // Revocation gate first: an inadmissible pool is an EMPTY pool, never the
        // unfiltered one.
        let now_secs = now_unix_secs() as i64;
        let admitted = self.admissible_order(now_secs);
        if admitted.is_empty() {
            anyhow::bail!(
                "overlay: no admissible relay (revocation fail-closed{})",
                if self.admission.enforcement_required() {
                    ", list required by the directory anchor"
                } else {
                    ""
                }
            );
        }
        // Pinned entry relays, restricted to what is admissible right now. Only
        // needed once a path is longer than one hop.
        let guards = if path::is_multi_hop(&self.endpoints, &admitted, self.target_hops) {
            self.guards.guards_for(&self.endpoints, &admitted, now_secs)
        } else {
            Vec::new()
        };

        // Try the pool in health order and return the FIRST success. Only when
        // EVERY candidate fails do we error — so a single dead relay never wedges
        // delivery, but an all-down pool still surfaces (Prefer→direct,
        // Strict→degrade). Each failed dial marks the EXIT endpoint dead; each
        // success clears it. The exit is what the health/latency model is about:
        // it is the hop that actually reaches the destination.
        let mut last_err = None;
        for (attempt, idx) in admitted.iter().copied().enumerate() {
            let Some(selected) = path::plan_path(
                &self.endpoints,
                &admitted,
                &guards,
                idx,
                attempt,
                self.target_hops,
            ) else {
                // No valid path with this candidate as the exit — most commonly a
                // peer relay, which may never terminate a circuit (§4.4).
                continue;
            };
            let started = Instant::now();
            let dial = self.dial_path(&selected, &identity, host, port);
            let outcome = match tokio::time::timeout(self.dial_timeout, dial).await {
                Ok(res) => res,
                Err(_) => Err(anyhow::anyhow!(
                    "overlay: relay {} dial timed out",
                    self.endpoints[idx].addr
                )),
            };
            match outcome {
                Ok(stream) => {
                    // Time the whole dial (resolve + circuit build + CONNECT), not
                    // just a ping: that IS the added latency §6.4 budgets for.
                    self.record_latency(idx, started.elapsed());
                    self.mark_healthy(idx);
                    return Ok(stream);
                }
                Err(e) => {
                    self.mark_dead(idx);
                    last_err = Some(e);
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| anyhow::anyhow!("overlay: relay pool exhausted with no endpoints")))
    }
}

/// Build the logged-in device's relay [`ClientIdentity`] from local state.
/// See [`RealRelayFactory`] for why the cert is minted locally.
async fn build_client_identity(state: &Arc<AppState>) -> anyhow::Result<ClientIdentity> {
    let user_id = overlay_signing_user(state).await?;

    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| anyhow::anyhow!("overlay: device_id not set (not logged in)"))?;

    // Account identity key (local): absent/locked ⇒ fail-closed (pre-enrollment).
    let account_key = crate::commands::account_identity::load_account_id_key(state, &user_id)
        .await
        .map_err(|e| anyhow::anyhow!("overlay: account identity unavailable ({e})"))?;
    let account_id_pub = account_key.verifying_key().encode().to_vec();

    // Device signing keys (local DB / openmls storage) — the keys the cert chain
    // certifies. The ML-DSA-44 one signs the handshake; the Ed25519 one is only
    // carried, because the cert binds both. Scoped so the !Send provider drops
    // before any await.
    let (device_signing, ed_pub, pq_pub) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("overlay: not signed in (local DB closed)"))?;
        let provider = crate::commands::mls::PollisProvider::new(db.conn());
        let (ed_pub, pq_pub) =
            crate::commands::mls::load_device_cert_pubs(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("overlay: device signing keys unavailable ({e})"))?;
        let (device_signing, _) =
            crate::commands::mls::load_device_pq_signing_key(&provider, &user_id, &device_id)
                .map_err(|e| anyhow::anyhow!("overlay: device PQ signing key unavailable ({e})"))?;
        (device_signing, ed_pub, pq_pub)
    };
    let ed_pub: [u8; 32] = ed_pub
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("overlay: device Ed25519 signing pub is not 32 bytes"))?;

    let issued_at = now_unix_secs();
    let cert = DeviceCertMaterial::mint(
        &account_key,
        &device_id,
        &ed_pub,
        &pq_pub,
        OVERLAY_CERT_IDENTITY_VERSION,
        issued_at,
    );
    debug_assert_eq!(cert.account_id_pub, account_id_pub);

    Ok(ClientIdentity::new(
        user_id,
        device_id,
        ed_pub,
        device_signing,
        cert,
    ))
}

/// The user this device signs as, mirroring `ds_client::current_user_id`: prefer
/// the unlocked session, fall back to the accounts index before unlock.
async fn overlay_signing_user(state: &Arc<AppState>) -> anyhow::Result<String> {
    if let Some(u) = state.unlock.lock().await.as_ref() {
        if !u.user_id.is_empty() {
            return Ok(u.user_id.clone());
        }
    }
    let index = crate::accounts::read_accounts_index()
        .map_err(|e| anyhow::anyhow!("overlay: read accounts index: {e}"))?;
    index
        .last_active_user
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow::anyhow!("overlay: no active user to sign relay handshake"))
}

fn now_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Load the configured relay endpoint(s) + the pinned QUIC leaf. Empty when the
/// endpoint or the pinned cert is absent/unreadable — the fail-closed state:
/// `RealRelayFactory::connect` then errors, so `Prefer` dials direct and `Strict`
/// degrades. A cert that can't be loaded is treated the same as none (never dial
/// an unverified relay).
fn load_relay_endpoints(config: &Config) -> Vec<RelayEndpoint> {
    let addrs = config.overlay_relay_endpoints();
    if addrs.is_empty() {
        return Vec::new();
    }
    let cert = match config.overlay_relay_cert.as_deref().and_then(load_pinned_cert) {
        Some(c) => c,
        None => {
            eprintln!("[overlay] relay endpoint set but no valid pinned cert — staying fail-closed");
            return Vec::new();
        }
    };
    addrs
        .into_iter()
        // The static config names OUR relays and carries no region, so every
        // entry is first-party with an unknown region (which the selector treats
        // as "not known to be diverse", never as diverse).
        .map(|addr| RelayEndpoint::first_party(addr, String::new(), cert.clone()))
        .collect()
}

/// Resolve `POLLIS_OVERLAY_RELAY_CERT`: a filesystem path to a DER cert, else the
/// base64 (STANDARD) of the DER bytes.
fn load_pinned_cert(s: &str) -> Option<CertificateDer<'static>> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if std::path::Path::new(s).is_file() {
        return std::fs::read(s).ok().map(CertificateDer::from);
    }
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .ok()
        .map(CertificateDer::from)
}

// ── Dynamic pool: the signed directory (design §14.1, issue #616) ──────────
//
// When `POLLIS_OVERLAY_DIRECTORY_URL` + `_KEY` are configured, the pool is not a
// static list but a signed directory (`crate::net::directory`) the client fetches
// + verifies and REFRESHES as the hosted pool's membership changes (nodes rotate
// on Spot reclamation / self-heal). A `SwappableFactory` holds the live pool and
// the refresh loop swaps it in place — no shim restart, no DB reconnect.

/// Refresh the directory this long BEFORE it expires, so the pool is never serving
/// from an expired directory. The reconciler re-signs every couple of minutes with
/// a ~1h expiry, so at steady state this refetches roughly hourly.
const DIRECTORY_REFRESH_SKEW: Duration = Duration::from_secs(5 * 60);
/// Clamp the computed sleep so a pathological `expires_at` can neither tight-loop
/// nor park refresh forever.
const DIRECTORY_MIN_REFRESH: Duration = Duration::from_secs(30);
const DIRECTORY_MAX_REFRESH: Duration = Duration::from_secs(55 * 60);
/// After a failed fetch, retry on this fixed backoff — recover lazily, keep the
/// previous pool meanwhile (mirrors `RemoteDb::with_retry`; never poll tightly).
const DIRECTORY_RETRY_BACKOFF: Duration = Duration::from_secs(60);
/// Floor on the interval between fetches, so an on-exhaustion notify storm during
/// an outage can never hammer the directory host.
const DIRECTORY_MIN_REFETCH: Duration = Duration::from_secs(15);

/// Map a verified [`directory::Directory`] to the factory's endpoint pool. Each
/// entry carries its OWN pinned cert (the format supports per-node certs; v0.5
/// shares one across the pool, but we consume whatever is listed). Entries whose
/// `cert_b64` is not valid base64/DER are skipped — never dial a relay we cannot
/// pin.
pub(crate) fn directory_to_endpoints(dir: &directory::Directory) -> Vec<RelayEndpoint> {
    use base64::Engine as _;
    let mut endpoints: Vec<RelayEndpoint> = dir
        .relays
        .iter()
        .filter_map(|r| {
            let der = base64::engine::general_purpose::STANDARD
                .decode(r.cert_b64.trim())
                .ok()?;
            // Everything in `relays[]` is ours: the reconciler only lists nodes it
            // runs. Peer-hosted relays are a separate array and are tiered `Peer`
            // below, which is what keeps them out of the guard and exit positions.
            Some(RelayEndpoint::first_party(
                r.addr.clone(),
                r.region.clone(),
                CertificateDer::from(der),
            ))
        })
        .collect();
    let peers = directory_to_peer_endpoints(dir, &endpoints);
    endpoints.extend(peers);
    endpoints
}

/// The peer-hosted middle hops a verified directory offers (#813 wave 3).
///
/// The signed directory is the **only** channel a peer can arrive through
/// (`crate::net::peer::discovery`'s module docs spell out why an unsigned one
/// would undo the design), and this is the only producer of [`RelayTier::Peer`]
/// endpoints. Everything here fails toward *fewer peers*, which is the pool as it
/// was before peers existed:
///
/// - an entry whose `cert_b64` is not valid base64/DER is skipped — a hop we
///   cannot pin is a hop we will not build a layer to;
/// - an entry parked at no relay in **this** directory is skipped as unreachable.
///   A peer accepts no inbound connections, so a relay we are not advertising
///   cannot be asked to splice into its link;
/// - the rendezvous address recorded on the endpoint is the first parked relay
///   that IS advertised, so a peer is never pinned to a node clients have stopped
///   using.
///
/// Revocation is deliberately **not** applied here: it is evaluated per candidate
/// at dial time by [`RelayAdmission`], on the same `cert_sha256_b64` selector
/// relays use, so a peer revoked after this directory was fetched is refused too.
fn directory_to_peer_endpoints(
    dir: &directory::Directory,
    first_party: &[RelayEndpoint],
) -> Vec<RelayEndpoint> {
    use base64::Engine as _;
    dir.peers
        .iter()
        .filter_map(|p| {
            let der = base64::engine::general_purpose::STANDARD
                .decode(p.cert_b64.trim())
                .ok()?;
            let parked_at: Vec<String> = p
                .parked_at
                .iter()
                .filter(|a| first_party.iter().any(|e| &&e.addr == a))
                .cloned()
                .collect();
            let rendezvous = parked_at.first()?.clone();
            Some(RelayEndpoint::peer_parked_at(
                rendezvous,
                // The directory carries no region for a peer, and inventing one
                // would be worse than none: the selector treats an unknown region
                // as "not known to be diverse", never as diverse.
                String::new(),
                CertificateDer::from(der),
                parked_at,
            ))
        })
        .collect()
}

/// Seconds to sleep before the next scheduled refresh, derived from the current
/// directory's `expires_at` minus the skew, clamped to a sane window.
fn until_refresh(expires_at: i64) -> Duration {
    let now = now_unix_secs() as i64;
    let secs = (expires_at - now).saturating_sub(DIRECTORY_REFRESH_SKEW.as_secs() as i64);
    Duration::from_secs(secs.max(0) as u64).clamp(DIRECTORY_MIN_REFRESH, DIRECTORY_MAX_REFRESH)
}

/// Owns the directory refresh task and aborts it when the factory drops (overlay
/// stopped or rebuilt) — so no orphan refresh loop outlives the pool it feeds.
struct AbortOnDrop(JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A [`CircuitFactory`] whose relay pool can be swapped live, so the refresh loop
/// replaces the pool as membership changes without restarting the shim. `connect`
/// reads the current pool per call (a cheap `Arc` clone; the lock is never held
/// across I/O) and, when a dial fully fails, nudges the refresh loop — the pool it
/// holds may be stale.
struct SwappableFactory {
    inner: Mutex<Arc<dyn CircuitFactory>>,
    /// Pinged when a connect exhausts the current pool: membership likely changed
    /// under us, so refetch out of band instead of waiting for the schedule.
    refresh_notify: tokio::sync::Notify,
    /// Aborts the refresh task on drop. `Option` only so it can be set AFTER the
    /// `Arc<SwappableFactory>` exists (the task holds a `Weak` back to it).
    refresh_task: Mutex<Option<AbortOnDrop>>,
}

impl SwappableFactory {
    fn new(initial: Arc<dyn CircuitFactory>) -> Self {
        SwappableFactory {
            inner: Mutex::new(initial),
            refresh_notify: tokio::sync::Notify::new(),
            refresh_task: Mutex::new(None),
        }
    }

    fn current(&self) -> Arc<dyn CircuitFactory> {
        self.inner.lock().unwrap().clone()
    }

    fn swap(&self, next: Arc<dyn CircuitFactory>) {
        *self.inner.lock().unwrap() = next;
    }
}

#[async_trait::async_trait]
impl CircuitFactory for SwappableFactory {
    async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
        let factory = self.current();
        match factory.connect(host, port).await {
            Ok(stream) => Ok(stream),
            Err(e) => {
                // Pool exhausted — likely stale membership. Nudge a refetch; the
                // refresh loop's debounce keeps this from hammering the host.
                self.refresh_notify.notify_one();
                Err(e)
            }
        }
    }
}

/// Fetch + verify the directory and build a fresh relay pool from it. Returns the
/// new factory and the directory's `expires_at` (to schedule the next refresh).
/// Fails (fail-closed) if the fetch, verification, or every listed cert fails.
///
/// **The revocation list rides this same tick** (#813 phase C's client half): no
/// second timer, because CLAUDE.md bans polling loops and the list's freshness
/// only has to keep up with the pool it filters. A failed revocation refresh is
/// deliberately NOT an error here — the pool is still built, and
/// [`RelayAdmission`] then refuses every member of it while the anchor requires a
/// list. Making it an error instead would swap a fail-closed pool for a *stale*
/// pool, which is the wrong way round.
async fn fetch_and_build_pool(
    url: &str,
    key: &str,
    state: &Weak<AppState>,
    cooldown: Duration,
    dial_timeout: Duration,
    admission: &Arc<RelayAdmission>,
    guards: &Arc<GuardBook>,
) -> anyhow::Result<(Arc<dyn CircuitFactory>, i64)> {
    let bytes = directory::fetch_directory(url).await?;
    let now = now_unix_secs() as i64;
    let dir = directory::verify_directory(&bytes, key, now)?;
    // Adopt the anchor BEFORE fetching the list, so the seq floor the install
    // checks against is the one this directory commits to.
    admission.observe_directory(&dir);
    if let Err(e) = admission.refresh(url, &dir, now).await {
        eprintln!("[overlay] revocation list unusable ({e}) — relays fail closed while the directory anchors one");
    }
    let endpoints = directory_to_endpoints(&dir);
    if endpoints.is_empty() {
        anyhow::bail!("overlay: directory verified but no relay had a usable pinned cert");
    }
    let factory: Arc<dyn CircuitFactory> = Arc::new(RealRelayFactory::new(
        state.clone(),
        endpoints,
        cooldown,
        dial_timeout,
        admission.clone(),
        guards.clone(),
    ));
    Ok((factory, dir.expires_at))
}

/// The directory refresh loop: keep the [`SwappableFactory`]'s pool current. Wakes
/// on either the expiry-derived schedule OR an on-exhaustion notify (debounced by
/// [`DIRECTORY_MIN_REFETCH`]). On success it swaps the pool and schedules the next
/// refresh near the new directory's expiry; on failure it keeps the existing pool
/// and retries on a fixed backoff. Exits when the factory is dropped (overlay
/// stopped) — the `Weak` upgrade fails. This is event-driven + lazy, not a
/// keepalive poll: it sleeps on the directory's own TTL and on real dial failures.
#[allow(clippy::too_many_arguments)]
async fn directory_refresh_loop(
    factory: Weak<SwappableFactory>,
    state: Weak<AppState>,
    url: String,
    key: String,
    cooldown: Duration,
    dial_timeout: Duration,
    admission: Arc<RelayAdmission>,
    guards: Arc<GuardBook>,
    mut next_sleep: Duration,
) {
    let mut last_fetch = Instant::now();
    loop {
        let sf = match factory.upgrade() {
            Some(sf) => sf,
            None => return,
        };
        tokio::select! {
            _ = tokio::time::sleep(next_sleep) => {}
            _ = sf.refresh_notify.notified() => {}
        }
        drop(sf);

        // Debounce: never refetch more often than DIRECTORY_MIN_REFETCH.
        let since = last_fetch.elapsed();
        if since < DIRECTORY_MIN_REFETCH {
            tokio::time::sleep(DIRECTORY_MIN_REFETCH - since).await;
        }
        last_fetch = Instant::now();

        match fetch_and_build_pool(
            &url,
            &key,
            &state,
            cooldown,
            dial_timeout,
            &admission,
            &guards,
        )
        .await
        {
            Ok((next_factory, expires_at)) => match factory.upgrade() {
                Some(sf) => {
                    sf.swap(next_factory);
                    next_sleep = until_refresh(expires_at);
                }
                None => return,
            },
            Err(e) => {
                eprintln!("[overlay] directory refresh failed, keeping current pool: {e}");
                next_sleep = DIRECTORY_RETRY_BACKOFF;
            }
        }
    }
}

/// Build the DYNAMIC (directory-backed) circuit factory: one initial fetch so the
/// pool is usable immediately, then spawn the refresh loop that keeps it current.
/// If the initial fetch fails the factory starts empty (fail-closed: Prefer→direct,
/// Strict→degrade) and the refresh loop recovers it.
async fn build_directory_factory(state: &Arc<AppState>) -> Arc<dyn CircuitFactory> {
    // Both are Some here — start_overlay_shim only calls this when configured.
    let url = state.config.overlay_directory_url.clone().unwrap_or_default();
    let key = state.config.overlay_directory_key.clone().unwrap_or_default();
    let weak_state = Arc::downgrade(state);
    // Built ONCE and shared across every pool rebuild: the revocation store's
    // high-water sequence (rollback protection) and the client's guard pins must
    // both survive a directory refresh, or each refresh would silently reset them.
    let admission = Arc::new(RelayAdmission::enforcing(&key));
    let guards = Arc::new(GuardBook::at(GuardBook::default_path()));

    let (initial, next_sleep) = match fetch_and_build_pool(
        &url,
        &key,
        &weak_state,
        RELAY_DEAD_COOLDOWN,
        RELAY_DIAL_TIMEOUT,
        &admission,
        &guards,
    )
    .await
    {
        Ok((factory, expires_at)) => (factory, until_refresh(expires_at)),
        Err(e) => {
            eprintln!("[overlay] initial directory fetch failed, fail-closed until refresh: {e}");
            let empty: Arc<dyn CircuitFactory> = Arc::new(RealRelayFactory::new(
                weak_state.clone(),
                Vec::new(),
                RELAY_DEAD_COOLDOWN,
                RELAY_DIAL_TIMEOUT,
                admission.clone(),
                guards.clone(),
            ));
            (empty, DIRECTORY_RETRY_BACKOFF)
        }
    };

    let swappable = Arc::new(SwappableFactory::new(initial));
    let task = tokio::spawn(directory_refresh_loop(
        Arc::downgrade(&swappable),
        weak_state,
        url,
        key,
        RELAY_DEAD_COOLDOWN,
        RELAY_DIAL_TIMEOUT,
        admission,
        guards,
        next_sleep,
    ));
    *swappable.refresh_task.lock().unwrap() = Some(AbortOnDrop(task));
    swappable
}

/// Start the overlay shim for `state` under `mode` (must be non-off). Builds the
/// routing policy + the real circuit factory and binds the loopback SOCKS5 shim.
/// The returned handle owns the shim task (aborted on drop) and lets the caller
/// flip Prefer↔Strict live. The factory loads the device identity lazily, so this
/// succeeds even before login — the shim runs (so `Strict` degrades) while
/// circuits fail-closed until a signing device is available.
pub(crate) async fn start_overlay_shim(
    state: &Arc<AppState>,
    mode: OverlayMode,
) -> Result<OverlayHandle> {
    let policy = overlay_policy(&state.config, mode);
    // Dynamic pool (signed directory) when configured; else the static v0 list.
    let factory: Arc<dyn CircuitFactory> = if state.config.overlay_directory_configured() {
        build_directory_factory(state).await
    } else {
        Arc::new(RealRelayFactory::new(
            Arc::downgrade(state),
            load_relay_endpoints(&state.config),
            RELAY_DEAD_COOLDOWN,
            RELAY_DIAL_TIMEOUT,
            // No signed directory ⇒ no revocation anchor to enforce against, and
            // a static pool small enough that guards are never consulted.
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        ))
    };
    let handle = OverlayShim::start(policy, factory)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("overlay shim start: {e}")))?;
    let relay = if state.config.overlay_directory_configured() {
        state
            .config
            .overlay_directory_url
            .as_deref()
            .unwrap_or("<directory>")
    } else {
        state
            .config
            .overlay_relay_url
            .as_deref()
            .unwrap_or("<none configured>")
    };
    eprintln!(
        "[overlay] shim on {} (mode={mode:?}, relay={relay})",
        handle.socks_addr()
    );
    Ok(handle)
}

// ── The libsql SOCKS connector (design §14.1) ──────────────────────────────

/// Build the libsql connector that routes Turso's Hrana/TLS through the overlay
/// shim. It mirrors libsql's own default remote connector (native roots verify
/// the REAL service cert via SNI from the request URI, http/1 ALPN, https-or-http)
/// but wraps a [`SocksConnector`] so the TCP lands on the loopback shim instead of
/// dialing Turso directly. The inner client TLS still terminates at the real host,
/// so the relay only ever forwards opaque bytes (§8).
pub(crate) fn overlay_connector(
    shim: SocketAddr,
) -> Result<hyper_rustls::HttpsConnector<SocksConnector>> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_native_roots()
        .map_err(|e| Error::Other(anyhow::anyhow!("overlay connector native roots: {e}")))?
        .https_or_http()
        .enable_http1()
        .wrap_connector(SocksConnector::new(shim));
    Ok(connector)
}

/// A `tower::Service<Uri>` that dials a target through the loopback SOCKS5 shim.
/// This is the inner connector libsql (via [`overlay_connector`]) calls to obtain
/// a TCP-shaped byte pipe, over which it then runs its own TLS to the real host.
#[derive(Clone, Copy)]
pub(crate) struct SocksConnector {
    shim: SocketAddr,
}

impl SocksConnector {
    pub(crate) fn new(shim: SocketAddr) -> Self {
        SocksConnector { shim }
    }
}

impl Service<Uri> for SocksConnector {
    type Response = SocksStream;
    type Error = std::io::Error;
    type Future = Pin<Box<dyn Future<Output = std::io::Result<SocksStream>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<std::result::Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let shim = self.shim;
        Box::pin(async move {
            let host = uri
                .host()
                .ok_or_else(|| io_err("overlay connector: request URI has no host"))?
                .to_string();
            // Turso/Hrana is always TLS; default to 443 when the URI omits a port.
            let port = uri.port_u16().unwrap_or(443);
            let inner = socks5_connect(shim, &host, port).await?;
            Ok(SocksStream { inner })
        })
    }
}

/// A TCP stream to the shim, wrapped so it satisfies libsql's `Socket` bound
/// (`hyper::client::connect::Connection` + `AsyncRead`/`AsyncWrite`).
pub(crate) struct SocksStream {
    inner: TcpStream,
}

impl Connection for SocksStream {
    fn connected(&self) -> Connected {
        // A plain proxied TCP hop — no HTTP/2 negotiation to advertise.
        Connected::new()
    }
}

impl AsyncRead for SocksStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for SocksStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn io_err(msg: &str) -> std::io::Error {
    std::io::Error::other(msg.to_string())
}

/// Open a TCP connection to `host:port` via a SOCKS5 CONNECT to the loopback
/// `shim`, using proxy-side DNS (ATYP=DOMAIN) so the shim resolves + allowlists
/// the real hostname. Mirrors the client half of `pollis_relay::shim`'s server.
async fn socks5_connect(shim: SocketAddr, host: &str, port: u16) -> std::io::Result<TcpStream> {
    let mut sock = TcpStream::connect(shim).await?;

    // Greeting: VER=5, one method, METHOD=0 (no auth — loopback only).
    sock.write_all(&[0x05, 0x01, 0x00]).await?;
    let mut method = [0u8; 2];
    sock.read_exact(&mut method).await?;
    if method[0] != 0x05 || method[1] != 0x00 {
        return Err(io_err("overlay shim declined SOCKS5 no-auth"));
    }

    // Request: VER, CMD=CONNECT, RSV, ATYP=DOMAIN, LEN, host, port (big-endian).
    let host_bytes = host.as_bytes();
    if host_bytes.len() > 255 {
        return Err(io_err("overlay connector: host too long for SOCKS5"));
    }
    let mut req = Vec::with_capacity(7 + host_bytes.len());
    req.extend_from_slice(&[0x05, 0x01, 0x00, 0x03, host_bytes.len() as u8]);
    req.extend_from_slice(host_bytes);
    req.extend_from_slice(&port.to_be_bytes());
    sock.write_all(&req).await?;

    // Reply: VER, REP, RSV, ATYP, BND.ADDR, BND.PORT. REP=0 is success.
    let mut head = [0u8; 4];
    sock.read_exact(&mut head).await?;
    if head[1] != 0x00 {
        return Err(io_err(&format!(
            "overlay shim CONNECT failed (SOCKS reply code {})",
            head[1]
        )));
    }
    // Drain the bound address the shim echoes (ignored for CONNECT).
    match head[3] {
        0x01 => {
            let mut addr = [0u8; 4];
            sock.read_exact(&mut addr).await?;
        }
        0x04 => {
            let mut addr = [0u8; 16];
            sock.read_exact(&mut addr).await?;
        }
        0x03 => {
            let len = sock.read_u8().await? as usize;
            let mut addr = vec![0u8; len];
            sock.read_exact(&mut addr).await?;
        }
        other => {
            return Err(io_err(&format!(
                "overlay shim replied with unknown ATYP {other}"
            )));
        }
    }
    let mut bnd_port = [0u8; 2];
    sock.read_exact(&mut bnd_port).await?;

    Ok(sock)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use pollis_relay::circuit::{Hop, SingleHopFactory};
    use pollis_relay::client::ClientIdentity;
    use pollis_relay::proto::DeviceCertMaterial;
    use pollis_relay::server::{Allowlist as RelayAllowlist, RelayConfig, RelayServer, RelayStats};
    use rustls::pki_types::{CertificateDer, ServerName};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use zeroize::Zeroizing;

    use crate::commands::pin::UnlockState;
    use crate::db::remote::RemoteDb;

    const USER: &str = "u_overlay_test";
    const DEVICE: &str = "d_overlay_test";
    const ORIGIN_NAME: &str = "origin.test";
    const ISSUED_AT: u64 = 1_700_000_000;

    /// The device's classic leaf key. Only carried by the handshake — the cert
    /// binds it, nothing signs under it — so a fixed byte pattern is enough.
    const DEVICE_ED_PUB: [u8; 32] = [11u8; 32];

    /// The device ML-DSA-44 signing key (signs the relay handshake).
    fn signing_key() -> ml_dsa::SigningKey<ml_dsa::MlDsa44> {
        ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&[11u8; 32].into())
    }

    /// The account identity key that certifies the device into a cert chain.
    fn account_key() -> ml_dsa::SigningKey<ml_dsa::MlDsa44> {
        ml_dsa::SigningKey::<ml_dsa::MlDsa44>::from_seed(&[12u8; 32].into())
    }

    /// A client identity carrying the device key + a valid offline cert chain —
    /// exactly what the client-side presents in production (built from the
    /// device's stored cert material).
    fn identity() -> Arc<ClientIdentity> {
        let device = signing_key();
        let cert = DeviceCertMaterial::mint(
            &account_key(),
            DEVICE,
            &DEVICE_ED_PUB,
            &device.verifying_key().encode(),
            1,
            ISSUED_AT,
        );
        Arc::new(ClientIdentity::new(
            USER,
            DEVICE,
            DEVICE_ED_PUB,
            device,
            cert,
        ))
    }

    fn cfg(mode: OverlayMode, relay: Option<&str>) -> Config {
        Config {
            turso_url: "libsql://turso.example.com".into(),
            turso_token: "t".into(),
            log_db_url: None,
            log_db_token: None,
            r2_endpoint: "https://r2.example.com".into(),
            r2_public_url: "https://cdn.example.com".into(),
            livekit_url: "wss://livekit.example.com".into(),
            pollis_delivery_url: Some("https://api.example.com".into()),
            overlay_mode: mode,
            overlay_relay_url: relay.map(|s| s.to_string()),
            overlay_relay_cert: None,
            overlay_directory_url: None,
            overlay_directory_key: None,
        }
    }

    /// A base64 (STANDARD) DER encoding of a relay's pinned cert, for
    /// `POLLIS_OVERLAY_RELAY_CERT`.
    fn cert_b64(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref())
    }

    /// A never-connecting factory: the fail-closed shape the shim sees when a
    /// device can't authenticate to a relay (no identity) or none is configured.
    struct FailingFactory;

    #[async_trait::async_trait]
    impl CircuitFactory for FailingFactory {
        async fn connect(&self, _host: &str, _port: u16) -> anyhow::Result<BoxedStream> {
            anyhow::bail!("overlay circuit unavailable (test fail-closed factory)")
        }
    }

    struct TestRelay {
        addr: SocketAddr,
        cert: CertificateDer<'static>,
        stats: Arc<RelayStats>,
        _task: tokio::task::JoinHandle<()>,
    }

    fn spawn_relay(allow: &[&str], overrides: &[(&str, IpAddr)]) -> TestRelay {
        let mut config = RelayConfig::new(
            "127.0.0.1:0".parse().unwrap(),
            RelayAllowlist::from_patterns(allow.iter().copied()),
        )
        .unwrap();
        for (host, ip) in overrides {
            config.resolve_overrides.insert((*host).to_string(), *ip);
        }
        // Without a current list this relay refuses every Extend, so multi-hop
        // tests fail with ExtendFailed. See net::testing.
        config.revocations = crate::net::testing::healthy_revocations();
        let cert = config.server_cert();
        let stats = config.stats.clone();
        let (task, addr) = RelayServer::spawn(config).unwrap();
        TestRelay { addr, cert, stats, _task: task }
    }

    impl TestRelay {
        fn factory(&self) -> Arc<dyn CircuitFactory> {
            Arc::new(SingleHopFactory::new(
                Hop::new(self.addr, self.cert.clone()),
                identity(),
            ))
        }
    }

    async fn spawn_plain_http(body: &'static str) -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let body = body.to_string();
                tokio::spawn(async move {
                    let _ = read_http_head(&mut sock).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        addr
    }

    /// A loopback TLS "origin" for `origin.test`, returning its addr + the CA to
    /// trust + a counter of accepted connections.
    async fn spawn_tls_origin(
        body: &'static str,
    ) -> (SocketAddr, CertificateDer<'static>, Arc<AtomicUsize>) {
        pollis_relay::tls::ensure_crypto_provider();
        let chain = pollis_relay::tls::generate_issued_chain(ORIGIN_NAME).unwrap();
        let ca = chain.ca_der.clone();

        let mut server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![chain.leaf_cert_der.clone()], chain.leaf_key_der)
            .unwrap();
        server_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server_cfg));

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let count = Arc::new(AtomicUsize::new(0));
        let c = count.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let c = c.clone();
                tokio::spawn(async move {
                    let mut tls = match acceptor.accept(tcp).await {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    c.fetch_add(1, Ordering::Relaxed);
                    let _ = read_http_head(&mut tls).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.shutdown().await;
                });
            }
        });
        (addr, ca, count)
    }

    async fn read_http_head<S: AsyncReadExt + Unpin>(s: &mut S) -> std::io::Result<Vec<u8>> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = s.read(&mut byte).await?;
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        Ok(buf)
    }

    // ── (a) POLLIS_OVERLAY parsing ─────────────────────────────────────────

    #[test]
    fn parse_overlay_mode_cases() {
        use crate::config::parse_overlay_mode;
        assert_eq!(parse_overlay_mode("off"), OverlayMode::Off);
        assert_eq!(parse_overlay_mode("prefer"), OverlayMode::Prefer);
        assert_eq!(parse_overlay_mode("STRICT"), OverlayMode::Strict);
        assert_eq!(parse_overlay_mode(" Prefer "), OverlayMode::Prefer);
        assert_eq!(parse_overlay_mode("bogus"), OverlayMode::Off);
        assert_eq!(parse_overlay_mode(""), OverlayMode::Off);
    }

    #[test]
    fn host_of_strips_scheme_port_path() {
        assert_eq!(host_of("libsql://turso.example.com").as_deref(), Some("turso.example.com"));
        assert_eq!(host_of("https://api.example.com:8080/v1").as_deref(), Some("api.example.com"));
        assert_eq!(host_of("wss://LiveKit.Example.com").as_deref(), Some("livekit.example.com"));
        assert_eq!(host_of("bare-host:443").as_deref(), Some("bare-host"));
        assert_eq!(host_of(""), None);
    }

    /// The plane split, including E1: the control plane AND LiveKit *signaling*
    /// route overlay; anything not first-party stays direct. What keeps LiveKit
    /// **media** direct is the transport, not this list — see
    /// `media_plane_stays_direct_while_signaling_is_overlaid`.
    #[test]
    fn policy_routes_control_plane_and_livekit_signaling() {
        let policy = overlay_policy(&cfg(OverlayMode::Prefer, None), OverlayMode::Prefer);
        use pollis_relay::PlannedRoute;
        // Control-plane hosts route overlay (with direct fallback in Prefer).
        assert_eq!(
            policy.plan("turso.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(
            policy.plan("api.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        assert_eq!(
            policy.plan("r2.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        // E1: the LiveKit signaling host routes overlay too. (Before #813 it was
        // pinned direct, which also pinned its signaling metadata to the client's
        // real address.)
        assert_eq!(
            policy.plan("livekit.example.com"),
            PlannedRoute::Overlay { fallback_to_direct: true }
        );
        // A non-first-party host (e.g. Expo push) is direct.
        assert_eq!(policy.plan("exp.host"), PlannedRoute::Direct);
    }

    // ── (b) overlay-off ⇒ inert ────────────────────────────────────────────

    /// `Off` derives a policy that routes every host — control-plane included —
    /// direct, so no shim is ever consulted. (The apply state machine never even
    /// starts a shim for `Off`; that is covered in `commands::overlay` tests.)
    #[test]
    fn off_mode_policy_is_all_direct() {
        use pollis_relay::PlannedRoute;
        let policy = overlay_policy(&cfg(OverlayMode::Off, None), OverlayMode::Off);
        assert_eq!(policy.plan("turso.example.com"), PlannedRoute::Direct);
        assert_eq!(policy.plan("api.example.com"), PlannedRoute::Direct);
        // E1 did not weaken this: with the overlay off, the LiveKit signaling
        // host is direct like everything else.
        assert_eq!(policy.plan("livekit.example.com"), PlannedRoute::Direct);
    }

    #[tokio::test]
    async fn off_mode_http_client_is_proxyless() {
        let addr = spawn_plain_http("direct-and-inert").await;
        // http_client(None) is a plain client — reaches a plain origin with no proxy.
        let client = http_client(None);
        let resp = client
            .get(format!("http://{addr}/"))
            .send()
            .await
            .expect("direct request with the overlay off");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "direct-and-inert");
    }

    /// Off is behaviorally distinct from on: with a Strict shim whose relay is
    /// unavailable, a proxied client FAILS for a control-plane host, while the
    /// proxy-less `http_client(None)` reaches the same host directly.
    #[tokio::test]
    async fn off_client_bypasses_a_shim_that_would_degrade() {
        let addr = spawn_plain_http("only-direct-reaches-me").await;
        let host = addr.ip().to_string();
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, Arc::new(FailingFactory))
            .await
            .unwrap();

        // Proxied + Strict + no relay ⇒ degraded, so the request fails.
        let proxied = http_client(Some(&shim));
        let via_overlay = proxied
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await;
        assert!(via_overlay.is_err(), "Strict with no relay must not silently go direct");

        // Proxy-less ⇒ reaches the origin directly.
        let direct = http_client(None)
            .get(format!("http://{host}:{}/", addr.port()))
            .send()
            .await
            .expect("proxy-less client reaches the origin directly");
        assert_eq!(direct.status(), 200);
        drop(shim);
    }

    // ── (c) end-to-end: reqwest AND the libsql connector route through the shim ─

    #[tokio::test]
    async fn reqwest_routes_through_shim_to_tls_origin() {
        let (origin_addr, ca, origin_conns) = spawn_tls_origin("hello-overlay").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, relay.factory()).await.unwrap();

        let client = pollis_relay::http::http_client_builder(Some(&shim))
            .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
            .build()
            .unwrap();
        let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
        let resp = client.get(&url).send().await.expect("request through overlay");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "hello-overlay");
        assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin");
        assert_eq!(origin_conns.load(Ordering::Relaxed), 1);
        drop(shim);
    }

    /// The libsql-shaped path: drive the exact `SocksConnector` that
    /// `overlay_connector` feeds libsql, then run client TLS over the stream it
    /// returns — the cert is verified for the REAL name `origin.test` through
    /// shim→relay, proving end-to-end TLS survives tunneling (design §14.1, T2).
    #[tokio::test]
    async fn libsql_socks_connector_carries_verified_tls() {
        let (origin_addr, ca, _c) = spawn_tls_origin("libsql-connector-shape").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([ORIGIN_NAME]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, relay.factory()).await.unwrap();

        // The inner connector: SOCKS-dial the real name through the shim.
        let mut connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("https://{ORIGIN_NAME}:{}", origin_addr.port())
            .parse()
            .unwrap();
        let stream = Service::call(&mut connector, uri)
            .await
            .expect("SOCKS connect through shim");

        // Client TLS over the proxied stream, verifying `origin.test`.
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).unwrap();
        let mut client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let server_name = ServerName::try_from(ORIGIN_NAME).unwrap();
        let mut tls = tls_connector
            .connect(server_name, stream)
            .await
            .expect("inner TLS verified for origin.test through the shim");

        tls.write_all(b"GET / HTTP/1.1\r\nHost: origin.test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.unwrap();
        let resp = String::from_utf8_lossy(&resp);
        assert!(resp.starts_with("HTTP/1.1 200 OK"), "unexpected response: {resp}");
        assert!(resp.contains("libsql-connector-shape"));
        assert_eq!(relay.stats.dials(), 1);

        // And the production connector builds (native roots) for this shim.
        assert!(overlay_connector(shim.socks_addr()).is_ok());
        drop(shim);
    }

    /// Strict + non-off mode with no usable circuit must surface a degraded error
    /// rather than silently dialing direct (messages-must-work). The shim runs
    /// (so the mode is honored) but every control-plane CONNECT fails.
    #[tokio::test]
    async fn strict_without_relay_degrades_not_silent_direct() {
        let addr = spawn_plain_http("must-not-be-reached").await;
        let host = addr.ip().to_string();
        // Route the echo host as control-plane so Strict applies, with a
        // fail-closed factory standing in for "no relay reachable".
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, Arc::new(FailingFactory))
            .await
            .expect("Strict starts the shim even with no relay reachable");

        let mut connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("https://{host}:{}", addr.port()).parse().unwrap();
        let result = Service::call(&mut connector, uri).await;
        assert!(result.is_err(), "Strict + no relay must degrade, never silent-direct");
        drop(shim);
    }

    // ── (d) LIVE application: set_overlay_mode actually routes traffic ──────────

    /// Build an `AppState` with device cert material provisioned (unlocked
    /// session + account identity key + open local DB + device id), so the
    /// `RealRelayFactory` can load a `ClientIdentity` and authenticate to a relay.
    /// `remote_db` is a (lazy) remote handle so `set_overlay_shim` exercises the
    /// real rebuild path; it is never queried here.
    async fn provisioned_state(config: Config) -> Arc<AppState> {
        let remote = Arc::new(RemoteDb::connect(&config.turso_url, "tok").await.unwrap());
        let keystore: Arc<dyn crate::keystore::Keystore> =
            Arc::new(crate::keystore::InMemoryKeystore::new());
        // log_db == remote_db (unconfigured commit-log DB), like production.
        let state = Arc::new(AppState::new_with_parts(
            config,
            Arc::clone(&remote),
            remote,
            keystore,
        ));
        *state.unlock.lock().await = Some(UnlockState {
            user_id: USER.to_string(),
            db_key: Zeroizing::new(vec![7u8; 32]),
            account_id_key: Zeroizing::new(account_key().to_seed().to_vec()),
        });
        *state.device_id.lock().await = Some(DEVICE.to_string());
        *state.local_db.lock().await = Some(crate::db::local::LocalDb::open_in_memory().unwrap());
        state
    }

    /// Drive the exact libsql-shaped `SocksConnector` through `shim` to the TLS
    /// origin and verify the inner TLS terminates at the REAL name `origin.test`.
    async fn tls_probe_through_shim(shim: SocketAddr, port: u16, ca: &CertificateDer<'static>) {
        let mut connector = SocksConnector::new(shim);
        let uri: Uri = format!("https://{ORIGIN_NAME}:{port}").parse().unwrap();
        let stream = Service::call(&mut connector, uri)
            .await
            .expect("SOCKS connect through shim");

        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.clone()).unwrap();
        let mut client_cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        let tls_connector = tokio_rustls::TlsConnector::from(Arc::new(client_cfg));
        let server_name = ServerName::try_from(ORIGIN_NAME).unwrap();
        let mut tls = tls_connector
            .connect(server_name, stream)
            .await
            .expect("inner TLS verified for origin.test through the shim");
        tls.write_all(b"GET / HTTP/1.1\r\nHost: origin.test\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        tls.read_to_end(&mut resp).await.unwrap();
        assert!(
            String::from_utf8_lossy(&resp).starts_with("HTTP/1.1 200 OK"),
            "libsql-shaped probe did not reach origin through the relay"
        );
    }

    /// THE live-application proof: flipping `set_overlay_mode` genuinely routes a
    /// reqwest control-plane call AND a libsql-shaped connection through an
    /// in-process relay (cert verified for the real name), Prefer↔Strict flips the
    /// live policy with no shim restart / DB reconnect, and Off restores the
    /// byte-for-byte direct path (relay sees no further dials).
    #[tokio::test]
    async fn set_overlay_mode_routes_live_through_relay() {
        let (origin_addr, ca, origin_conns) = spawn_tls_origin("live-apply").await;
        let relay = spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, IpAddr::V4(Ipv4Addr::LOCALHOST))]);

        let mut config = cfg(OverlayMode::Off, Some(&relay.addr.to_string()));
        // origin.test is the control-plane (Turso) host, so it routes overlay.
        config.turso_url = format!("libsql://{ORIGIN_NAME}");
        config.overlay_relay_cert = Some(cert_b64(&relay.cert));
        let state = provisioned_state(config).await;

        use crate::commands::overlay::{apply_overlay_mode, get_overlay_mode};

        // Off → Prefer: shim up, both remote DBs repointed through it.
        apply_overlay_mode(&state, OverlayMode::Prefer).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "prefer");
        let handle = state.overlay_handle().expect("shim running after Prefer");
        assert_eq!(handle.mode(), OverlayMode::Prefer);
        let shim_addr = handle.socks_addr();
        assert_eq!(
            state.remote_db.overlay_shim(),
            Some(shim_addr),
            "remote_db must be routed through the shim after Prefer"
        );

        // (1) A reqwest control-plane call routes THROUGH THE RELAY.
        let client = pollis_relay::http::http_client_builder(Some(handle.as_ref()))
            .add_root_certificate(reqwest::Certificate::from_der(&ca).unwrap())
            .build()
            .unwrap();
        let url = format!("https://{ORIGIN_NAME}:{}/", origin_addr.port());
        let resp = client.get(&url).send().await.expect("reqwest routes through relay");
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.text().await.unwrap(), "live-apply");
        assert_eq!(relay.stats.dials(), 1, "relay must have dialed the origin (reqwest)");

        // (2) A libsql-shaped connection routes through the relay too, cert
        //     verified for the REAL name origin.test.
        tls_probe_through_shim(shim_addr, origin_addr.port(), &ca).await;
        assert_eq!(relay.stats.dials(), 2, "relay must have dialed the origin (libsql shape)");
        assert!(origin_conns.load(Ordering::Relaxed) >= 2);

        // Prefer → Strict: live policy flip — SAME shim, no DB reconnect.
        apply_overlay_mode(&state, OverlayMode::Strict).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "strict");
        let handle2 = state.overlay_handle().expect("shim still running after Strict");
        assert_eq!(handle2.mode(), OverlayMode::Strict);
        assert_eq!(handle2.socks_addr(), shim_addr, "Prefer↔Strict must not restart the shim");
        assert_eq!(
            state.remote_db.overlay_shim(),
            Some(shim_addr),
            "Prefer↔Strict must not reconnect the DBs"
        );

        // Strict → Off: shim dropped, DBs back to direct, relay sees no new dials.
        apply_overlay_mode(&state, OverlayMode::Off).await.unwrap();
        assert_eq!(get_overlay_mode(&state).await.unwrap(), "off");
        assert!(state.overlay_handle().is_none(), "shim must stop after Off");
        assert_eq!(state.remote_db.overlay_shim(), None, "remote_db must be direct after Off");

        // A direct call now bypasses the relay entirely.
        let plain = spawn_plain_http("direct-after-off").await;
        let direct = http_client(None)
            .get(format!("http://{plain}/"))
            .send()
            .await
            .expect("direct request after Off");
        assert_eq!(direct.status(), 200);
        assert_eq!(relay.stats.dials(), 2, "Off routes direct — relay sees no new dials");
    }

    /// Strict with the relay DOWN, applied LIVE, must surface a degraded error —
    /// never a silent direct send.
    #[tokio::test]
    async fn set_overlay_strict_relay_down_degrades_live() {
        // A pinned cert we can load, but an endpoint that points nowhere.
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        config.turso_url = format!("libsql://{ORIGIN_NAME}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Strict)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("https://{ORIGIN_NAME}:443").parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_err(),
            "Strict + relay down must degrade, never silent-direct"
        );
    }

    /// Prefer with the relay DOWN, applied LIVE, falls back to a direct dial of
    /// the (directly reachable) control-plane host.
    #[tokio::test]
    async fn set_overlay_prefer_relay_down_falls_back_direct_live() {
        let plain = spawn_plain_http("prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1"));
        // Control-plane host = the directly-connectable plain origin.
        config.turso_url = format!("libsql://{host}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("http://{host}:{}", plain.port()).parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_ok(),
            "Prefer + relay down must fall back to a direct dial"
        );
    }

    // ── (e) POOL + health + failover (design §14.1, "messages must work") ───────

    const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

    /// A relay that resolves `origin.test` to loopback and allows it as a target
    /// — an in-process pool member the client can dial an origin through.
    fn spawn_pool_relay() -> TestRelay {
        spawn_relay(&[ORIGIN_NAME], &[(ORIGIN_NAME, LOCALHOST)])
    }

    /// A `RealRelayFactory` over `endpoints`, drawing its device identity from a
    /// provisioned `AppState` (kept alive by the caller via the returned `Arc`).
    fn pool_factory(
        state: &Arc<AppState>,
        endpoints: Vec<RelayEndpoint>,
        cooldown: Duration,
    ) -> RealRelayFactory {
        // Short dial timeout so an unreachable endpoint fails over fast in tests.
        RealRelayFactory::new(
            Arc::downgrade(state),
            endpoints,
            cooldown,
            Duration::from_secs(2),
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        )
    }

    fn endpoint(addr: String, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::first_party(addr, String::new(), cert)
    }

    /// FAILOVER: [relayA(unreachable), relayB(up)] → a control-plane dial still
    /// succeeds THROUGH relayB, relayA is tried first and marked unhealthy.
    #[tokio::test]
    async fn pool_fails_over_to_healthy_relay() {
        let origin = spawn_plain_http("failover-target").await;
        let relay_b = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // relayA is an unreachable address at index 0 (tried first on the first
        // connect: start offset is 0); relayB is the live pool member at index 1.
        let endpoints = vec![
            endpoint("127.0.0.1:1".into(), relay_b.cert.clone()),
            endpoint(relay_b.addr.to_string(), relay_b.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        let stream = factory.connect(ORIGIN_NAME, origin.port()).await;
        assert!(stream.is_ok(), "pool must fail over to the healthy relay B");
        assert_eq!(relay_b.stats.dials(), 1, "relay B dialed the origin");

        let health = factory.health.lock().unwrap();
        assert!(health[0].is_some(), "relay A must be marked unhealthy after its failed dial");
        assert!(health[1].is_none(), "relay B stays healthy after a successful dial");
    }

    /// ALL-DOWN → policy holds: every endpoint fails → `connect` errors (so Prefer
    /// falls back to direct / Strict degrades), and every endpoint is marked dead.
    #[tokio::test]
    async fn pool_all_down_errors_so_policy_applies() {
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;
        let endpoints = vec![
            endpoint("127.0.0.1:1".into(), cert.clone()),
            endpoint("127.0.0.1:2".into(), cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        assert!(
            factory.connect(ORIGIN_NAME, 443).await.is_err(),
            "an all-down pool must error so Prefer→direct / Strict→degrade applies"
        );
        let health = factory.health.lock().unwrap();
        assert!(
            health.iter().all(|h| h.is_some()),
            "every endpoint marked dead after all fail"
        );
    }

    /// ALL-DOWN, applied LIVE via a comma-separated multi-endpoint config: Prefer
    /// still falls back to a direct dial (the whole pool → policy path).
    #[tokio::test]
    async fn pool_multi_endpoint_prefer_falls_back_direct_live() {
        let plain = spawn_plain_http("pool-prefer-fallback").await;
        let host = plain.ip().to_string();
        let cert = pollis_relay::tls::generate_self_signed("pollis-relay")
            .unwrap()
            .cert_der;
        // Two dead relays, comma-separated — parsed into a two-member pool.
        let mut config = cfg(OverlayMode::Off, Some("127.0.0.1:1,127.0.0.1:2"));
        config.turso_url = format!("libsql://{host}");
        config.overlay_relay_cert = Some(cert_b64(&cert));
        let state = provisioned_state(config).await;

        crate::commands::overlay::apply_overlay_mode(&state, OverlayMode::Prefer)
            .await
            .unwrap();
        let handle = state.overlay_handle().unwrap();

        let mut connector = SocksConnector::new(handle.socks_addr());
        let uri: Uri = format!("http://{host}:{}", plain.port()).parse().unwrap();
        assert!(
            Service::call(&mut connector, uri).await.is_ok(),
            "Prefer + whole pool down must still fall back to a direct dial"
        );
    }

    /// HEALTH/COOLDOWN: an endpoint in its cooldown window is skipped (the healthy
    /// one is preferred); after the cooldown expires it is retried.
    #[tokio::test]
    async fn pool_skips_dead_endpoint_during_cooldown_then_retries() {
        let origin = spawn_plain_http("cooldown-target").await;
        let relay0 = spawn_pool_relay();
        let relay1 = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let cooldown = Duration::from_millis(150);
        let endpoints = vec![
            endpoint(relay0.addr.to_string(), relay0.cert.clone()),
            endpoint(relay1.addr.to_string(), relay1.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, cooldown);

        // Both relays are UP, but pin endpoint 0 as dead within its cooldown.
        factory.health.lock().unwrap()[0] = Some(Instant::now() + cooldown);

        // The dead endpoint is skipped — endpoint 1 serves the connect.
        factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        assert_eq!(relay0.stats.dials(), 0, "dead endpoint 0 skipped while in cooldown");
        assert_eq!(relay1.stats.dials(), 1, "healthy endpoint 1 served the connect");

        // After the cooldown expires, endpoint 0 is eligible again; over a few
        // rotating connects it is retried and dials.
        tokio::time::sleep(cooldown + Duration::from_millis(50)).await;
        for _ in 0..4 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert!(
            relay0.stats.dials() >= 1,
            "endpoint 0 is retried once its cooldown has expired"
        );
    }

    /// FAIL-OPEN: if ALL endpoints are marked dead, they are still TRIED — a
    /// transient outage that marked the whole pool dead must never wedge it.
    #[tokio::test]
    async fn pool_tries_all_dead_endpoints_fail_open() {
        let origin = spawn_plain_http("fail-open-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        // Mark the only endpoint dead for the full cooldown; fail-open must still
        // try it rather than refuse to dial.
        factory.health.lock().unwrap()[0] = Some(Instant::now() + Duration::from_secs(30));

        factory
            .connect(ORIGIN_NAME, origin.port())
            .await
            .expect("fail-open: a fully-dead pool is still tried");
        assert_eq!(relay.stats.dials(), 1, "the dead endpoint was tried anyway");
        // A successful dial clears its dead mark.
        assert!(factory.health.lock().unwrap()[0].is_none());
    }

    /// LOAD SPREAD: with two healthy relays, repeated connects do not all land on
    /// endpoint 0 — the rotating start index deals them out deterministically.
    #[tokio::test]
    async fn pool_spreads_load_across_healthy_relays() {
        let origin = spawn_plain_http("spread-target").await;
        let relay0 = spawn_pool_relay();
        let relay1 = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(relay0.addr.to_string(), relay0.cert.clone()),
            endpoint(relay1.addr.to_string(), relay1.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        // Rotation deals connects out across healthy endpoints instead of
        // hammering endpoint 0 — that is the property, and it is what a
        // round-robin regression would break.
        //
        // The share is deliberately NOT asserted as exactly N/2. These endpoints
        // are unmeasured on the first two dials and then carry REAL loopback
        // latencies, which feed #812's band: on a loaded machine one scheduler
        // hiccup can legitimately put an endpoint more than
        // `RELAY_LATENCY_BAND` behind and demote it for the rest of the run.
        // That is the latency preference working as designed, not a spread
        // failure — asserting exact halves made this test flake (~1 run in 8,
        // reproducible on the pre-#813 tree).
        assert_eq!(relay0.stats.dials() + relay1.stats.dials(), N);
        assert!(
            relay0.stats.dials() >= 1 && relay1.stats.dials() >= 1,
            "load collapsed onto one endpoint ({} vs {})",
            relay0.stats.dials(),
            relay1.stats.dials()
        );
    }

    /// #812: a relay measurably slower than the band is NOT dialed while a fast
    /// one is healthy. This is the invariant the live measurement showed missing —
    /// health-order rotation sent a US-East client to the 69 ms us-west-2 node on
    /// every other dial. Both test relays are loopback-fast, so the latencies are
    /// seeded directly; what is under test is the ordering, not the clock.
    #[tokio::test]
    async fn pool_prefers_the_lower_latency_relay() {
        let origin = spawn_plain_http("latency-target").await;
        let fast = spawn_pool_relay();
        let slow = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(fast.addr.to_string(), fast.cert.clone()),
            endpoint(slow.addr.to_string(), slow.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));
        // Mirrors the measured pool: 15 ms same-region vs 69 ms cross-country,
        // which is far outside RELAY_LATENCY_BAND.
        factory.record_latency(0, Duration::from_millis(15));
        factory.record_latency(1, Duration::from_millis(69));

        // Fewer than RELAY_EXPLORE_EVERY dials, so no explore dial intervenes.
        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(fast.stats.dials(), N, "every dial took the fast relay");
        assert_eq!(slow.stats.dials(), 0, "the +54ms relay was never dialed");
    }

    /// #812: relays within `RELAY_LATENCY_BAND` of each other are equivalent and
    /// keep rotating. Preferring the fastest must not collapse a region's clients
    /// onto a single node — that is a load-spread and anonymity-set regression.
    #[tokio::test]
    async fn pool_rotates_within_the_latency_band() {
        let origin = spawn_plain_http("band-target").await;
        let relay0 = spawn_pool_relay();
        let relay1 = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(relay0.addr.to_string(), relay0.cert.clone()),
            endpoint(relay1.addr.to_string(), relay1.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));
        // 5 ms apart — well inside the 20 ms band, so neither wins outright.
        factory.record_latency(0, Duration::from_millis(10));
        factory.record_latency(1, Duration::from_millis(15));

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(
            relay0.stats.dials(),
            N / 2,
            "in-band endpoint 0 keeps its share"
        );
        assert_eq!(
            relay1.stats.dials(),
            N / 2,
            "in-band endpoint 1 keeps its share"
        );
    }

    /// #812: a demoted endpoint must be able to earn its way back. Without the
    /// periodic explore dial, one bad sample would exile an endpoint permanently
    /// (it is never dialed, so its EWMA can never update) and the pool would
    /// silently shrink to whichever node happened to win early.
    #[tokio::test]
    async fn pool_explores_the_slow_relay_periodically() {
        let origin = spawn_plain_http("explore-target").await;
        let fast = spawn_pool_relay();
        let slow = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(fast.addr.to_string(), fast.cert.clone()),
            endpoint(slow.addr.to_string(), slow.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));
        factory.record_latency(0, Duration::from_millis(15));
        factory.record_latency(1, Duration::from_millis(500));

        // Exactly one full explore period: seq 0..=RELAY_EXPLORE_EVERY-1, whose
        // last dial is the explore one and (n=2, seq odd) starts at endpoint 1.
        for _ in 0..RELAY_EXPLORE_EVERY {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(
            slow.stats.dials(),
            1,
            "the slow relay is re-sampled exactly once per explore period"
        );
        assert_eq!(
            fast.stats.dials(),
            RELAY_EXPLORE_EVERY as u64 - 1,
            "every other dial still took the fast relay"
        );
    }

    /// #812 must not weaken the fail-open guarantee: latency only ever reorders
    /// HEALTHY endpoints. A fast-but-dead relay is still tried after a slow live
    /// one, and an all-dead pool is still fully attempted.
    #[tokio::test]
    async fn pool_latency_order_never_overrides_fail_open() {
        let origin = spawn_plain_http("failopen-target").await;
        let slow_live = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // Endpoint 0 is unreachable but "fast"; endpoint 1 is live but slow.
        let endpoints = vec![
            endpoint("127.0.0.1:1".to_string(), slow_live.cert.clone()),
            endpoint(slow_live.addr.to_string(), slow_live.cert.clone()),
        ];
        let factory = pool_factory(&state, endpoints, Duration::from_secs(30));
        factory.record_latency(0, Duration::from_millis(1));
        factory.record_latency(1, Duration::from_millis(400));

        // The fast endpoint is tried first (and fails), then the slow live one
        // serves the dial — delivery still works, which is the whole point.
        factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        assert_eq!(slow_live.stats.dials(), 1, "the slow live relay carried it");
    }

    // ── (f) #813 phase B: multi-hop paths, guards, the first-party exit ────────

    /// A pool factory with an explicit path length + revocation gate + guard
    /// book, for the phase-B tests. Everything else matches `pool_factory`.
    fn path_factory(
        state: &Arc<AppState>,
        endpoints: Vec<RelayEndpoint>,
        admission: Arc<RelayAdmission>,
        guards: Arc<GuardBook>,
        target_hops: usize,
    ) -> RealRelayFactory {
        let mut factory = RealRelayFactory::new(
            Arc::downgrade(state),
            endpoints,
            Duration::from_secs(30),
            Duration::from_secs(4),
            admission,
            guards,
        );
        factory.target_hops = target_hops;
        factory
    }

    fn peer_endpoint(addr: String, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::peer(addr, String::new(), cert)
    }

    fn endpoint_in(addr: String, region: &str, cert: CertificateDer<'static>) -> RelayEndpoint {
        RelayEndpoint::first_party(addr, region.to_string(), cert)
    }

    /// A pool big enough to leave a spare builds a real MULTI-HOP circuit: hop 1
    /// only ever `Extend`s (it never sees the destination) and the last hop is the
    /// one that dials the origin. Both are read off the relays' own counters, so
    /// this is the wire behaviour, not the selector's opinion of it.
    #[tokio::test]
    async fn a_three_relay_pool_builds_a_two_hop_circuit() {
        let origin = spawn_plain_http("two-hop-target").await;
        let relays = [spawn_pool_relay(), spawn_pool_relay(), spawn_pool_relay()];
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }

        let dials: u64 = relays.iter().map(|r| r.stats.dials()).sum();
        let extends: u64 = relays.iter().map(|r| r.stats.extends()).sum();
        assert_eq!(dials, N, "exactly one hop per circuit reaches the destination");
        assert_eq!(extends, N, "a 2-hop circuit extends exactly once");
        // Every circuit has a distinct entry and exit — enforced structurally by
        // `plan_path` (unit-tested in `net::path`), so no relay ever sees the
        // client's address and the destination on the SAME circuit. A guard may
        // still serve as an exit on a *different* circuit; see the module notes.
        assert!(
            relays.iter().filter(|r| r.stats.extends() > 0).count() >= 1,
            "no relay ever acted as an entry hop"
        );
        for r in &relays {
            assert!(
                r.stats.dials() + r.stats.extends() <= N,
                "a relay served more positions than there were circuits"
            );
        }
    }

    /// A pool with no spare node stays SINGLE-hop: "messages must work" outranks
    /// path length, and a 2-hop path across a 2-node pool has nothing to fail
    /// over to.
    #[tokio::test]
    async fn a_two_relay_pool_stays_single_hop() {
        let origin = spawn_plain_http("single-hop-target").await;
        let relays = [spawn_pool_relay(), spawn_pool_relay()];
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        for _ in 0..4u32 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        let extends: u64 = relays.iter().map(|r| r.stats.extends()).sum();
        assert_eq!(extends, 0, "a 2-node pool must not build multi-hop paths");
        assert_eq!(relays.iter().map(|r| r.stats.dials()).sum::<u64>(), 4);
    }

    /// THE INVARIANT, on the wire: a peer relay in the pool is **never** the hop
    /// that reaches the destination. It may carry traffic as a middle, but the
    /// terminal hop is always first-party (§4.4).
    #[tokio::test]
    async fn a_peer_relay_is_never_the_last_hop() {
        let origin = spawn_plain_http("peer-exit-target").await;
        let first_a = spawn_pool_relay();
        let first_b = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints = vec![
            endpoint(first_a.addr.to_string(), first_a.cert.clone()),
            peer_endpoint(peer.addr.to_string(), peer.cert.clone()),
            endpoint(first_b.addr.to_string(), first_b.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 8;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(
            peer.stats.dials(),
            0,
            "a peer relay reached the destination — the first-party exit invariant is broken"
        );
        assert_eq!(
            peer.stats.extends(),
            0,
            "a peer was used as an entry hop; peers are middle-only"
        );
        assert_eq!(
            first_a.stats.dials() + first_b.stats.dials(),
            N,
            "every circuit exited through a first-party relay"
        );
    }

    /// ...and the other half of the tier rule: a peer IS carried as a **middle**
    /// hop. Forcing a 3-hop target over a pool containing one peer puts it in the
    /// middle (it extends) and never at the end (it never dials).
    #[tokio::test]
    async fn a_peer_relay_is_usable_as_a_middle_hop_on_the_wire() {
        let origin = spawn_plain_http("peer-middle-target").await;
        let a = spawn_pool_relay();
        let b = spawn_pool_relay();
        let c = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // The peer sits in a distinct region, so middle selection prefers it —
        // which is exactly the point of allowing peers there.
        let endpoints = vec![
            endpoint_in(a.addr.to_string(), "us-west-2", a.cert.clone()),
            endpoint_in(b.addr.to_string(), "us-west-2", b.cert.clone()),
            RelayEndpoint::peer(peer.addr.to_string(), "eu-west-1".into(), peer.cert.clone()),
            endpoint_in(c.addr.to_string(), "us-west-2", c.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
            3,
        );

        const N: u64 = 4;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert!(
            peer.stats.extends() > 0,
            "the peer was never used as a middle hop"
        );
        assert_eq!(peer.stats.dials(), 0, "the peer must never be the exit");
        let extends: u64 = [&a, &b, &c, &peer].iter().map(|r| r.stats.extends()).sum();
        assert_eq!(extends, N * 2, "a 3-hop circuit extends twice");
    }

    /// GUARDS: the entry hop is drawn from a small pinned set, not re-rolled per
    /// circuit. Over many connects at most [`path::GUARD_SET_SIZE`] distinct
    /// relays ever occupy the entry position, and at least one pool member never
    /// does — which is the property a random first hop would not have.
    #[tokio::test]
    async fn guard_pinning_bounds_the_entry_set() {
        let origin = spawn_plain_http("guard-target").await;
        let relays: Vec<TestRelay> = (0..4).map(|_| spawn_pool_relay()).collect();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let endpoints: Vec<RelayEndpoint> = relays
            .iter()
            .map(|r| endpoint(r.addr.to_string(), r.cert.clone()))
            .collect();
        let book = Arc::new(GuardBook::ephemeral());
        let factory = path_factory(
            &state,
            endpoints,
            Arc::new(RelayAdmission::unenforced()),
            book,
            path::TARGET_HOPS,
        );

        for _ in 0..12u32 {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        let entries = relays.iter().filter(|r| r.stats.extends() > 0).count();
        assert!(entries >= 1, "somebody must have been the entry hop");
        assert!(
            entries <= path::GUARD_SET_SIZE,
            "entry hops were not pinned: {entries} distinct relays saw the client's address"
        );
        assert!(
            relays.iter().any(|r| r.stats.extends() == 0),
            "every relay served as an entry — that is a random first hop, not a guard"
        );
    }

    // ── (g) #813 phase C's client half: revocation, fail-closed ───────────────

    /// Build an enforcing admission gate holding a freshly-signed list that
    /// revokes `revoked_addrs`, anchored by a directory with a matching seq.
    fn enforcing_gate(revoked_addrs: &[&str], now: i64, expires_at: i64) -> Arc<RelayAdmission> {
        let revoked: Vec<serde_json::Value> = revoked_addrs
            .iter()
            .map(|a| serde_json::json!({ "addr": a }))
            .collect();
        enforcing_gate_for(revoked, now, expires_at)
    }

    /// The same, for revocation entries written out in full — the
    /// `cert_sha256_b64` selector is the only one that can name a peer, which has
    /// no address of its own.
    fn enforcing_gate_for(
        revoked: Vec<serde_json::Value>,
        now: i64,
        expires_at: i64,
    ) -> Arc<RelayAdmission> {
        use base64::Engine as _;
        use ed25519_dalek::{Signer, SigningKey};

        let sk = SigningKey::from_bytes(&[21u8; 32]);
        let key_b64 =
            base64::engine::general_purpose::STANDARD.encode(sk.verifying_key().to_bytes());
        let gate = Arc::new(RelayAdmission::enforcing(&key_b64));

        let revoked_count = revoked.len();
        let payload = serde_json::to_string(&serde_json::json!({
            "version": pollis_relay::REVOCATION_VERSION,
            "type": pollis_relay::REVOCATION_TYPE,
            "seq": 5,
            "issued_at": now - 10,
            "expires_at": expires_at,
            "revoked": revoked,
        }))
        .unwrap();
        let sig = sk.sign(payload.as_bytes());
        let envelope = serde_json::to_vec(&serde_json::json!({
            "payload_b64": base64::engine::general_purpose::STANDARD.encode(payload.as_bytes()),
            "signature_b64": base64::engine::general_purpose::STANDARD.encode(sig.to_bytes()),
        }))
        .unwrap();

        let dir_json = format!(
            r#"{{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{{"addr":"x:1","cert_b64":"QUJD"}}],"revocation":{{"seq":5,"count":{revoked_count}}}}}"#
        );
        let dir: directory::Directory = serde_json::from_str(&dir_json).unwrap();
        gate.observe_directory(&dir);
        gate.install(&envelope, now).expect("list installs");
        gate
    }

    /// A revoked relay is NEVER dialed — not deprioritized, not tried last. The
    /// pool routes around it entirely.
    #[tokio::test]
    async fn a_revoked_relay_is_never_dialed() {
        let origin = spawn_plain_http("revocation-target").await;
        let good = spawn_pool_relay();
        let seized = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let now = now_unix_secs() as i64;
        let gate = enforcing_gate(&[&seized.addr.to_string()], now, now + 3_600);
        let endpoints = vec![
            endpoint(seized.addr.to_string(), seized.cert.clone()),
            endpoint(good.addr.to_string(), good.cert.clone()),
        ];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        const N: u64 = 6;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(seized.stats.dials(), 0, "a revoked relay carried traffic");
        assert_eq!(seized.stats.extends(), 0, "a revoked relay was an entry hop");
        assert_eq!(good.stats.dials(), N, "the unrevoked relay carried everything");
    }

    /// FAIL-CLOSED: the directory anchors a revocation list, we hold none, so the
    /// pool is EMPTY — never the unfiltered pool. `connect` errors, which is the
    /// already-tested route to `Prefer`→direct / `Strict`→degrade.
    #[tokio::test]
    async fn an_unevaluable_revocation_list_empties_the_pool() {
        let origin = spawn_plain_http("failclosed-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        // Enforcing gate, anchor says 2 relays are revoked, nothing installed.
        let gate = Arc::new(RelayAdmission::enforcing("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="));
        let dir: directory::Directory = serde_json::from_str(
            r#"{"version":1,"issued_at":1,"expires_at":9999999999,"relays":[{"addr":"x:1","cert_b64":"QUJD"}],"revocation":{"seq":9,"count":2}}"#,
        )
        .unwrap();
        gate.observe_directory(&dir);

        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        assert!(
            factory.connect(ORIGIN_NAME, origin.port()).await.is_err(),
            "an unevaluable revocation list must empty the pool, not pass it through"
        );
        assert_eq!(
            relay.stats.dials(),
            0,
            "a relay was dialed while revocation could not be evaluated"
        );
    }

    /// The list expiring mid-session closes the pool at USE time — the client does
    /// not keep dialing on stale evidence until the next refresh tick.
    #[tokio::test]
    async fn an_expired_revocation_list_closes_the_pool_at_use_time() {
        let origin = spawn_plain_http("expiry-target").await;
        let relay = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let now = now_unix_secs() as i64;
        // A list that revokes something (so the anchor requires evidence) and is
        // already expired as far as `now` is concerned.
        let gate = enforcing_gate(&["198.51.100.9:9444"], now - 100, now - 1);
        let endpoints = vec![endpoint(relay.addr.to_string(), relay.cert.clone())];
        let factory = path_factory(
            &state,
            endpoints,
            gate,
            Arc::new(GuardBook::ephemeral()),
            path::TARGET_HOPS,
        );

        assert!(
            factory.connect(ORIGIN_NAME, origin.port()).await.is_err(),
            "an expired list is not evidence; the pool must close"
        );
        assert_eq!(relay.stats.dials(), 0);
    }

    // ── (g2) #813 wave 3: peers published by the directory ────────────────────

    fn cert_b64_of(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(cert.as_ref())
    }

    fn cert_digest_b64_of(cert: &CertificateDer<'static>) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(pollis_relay::proto::cert_fingerprint(cert.as_ref()))
    }

    /// A directory JSON carrying `relays` and (optionally) `peers`, parsed the way
    /// `verify_directory` hands it to the pool builder.
    fn directory_with(relays: &[(&str, &CertificateDer<'static>)], peers: serde_json::Value) -> directory::Directory {
        let relays: Vec<serde_json::Value> = relays
            .iter()
            .map(|(addr, cert)| {
                serde_json::json!({ "addr": addr, "region": "us-west-2", "cert_b64": cert_b64_of(cert) })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "issued_at": 1,
            "expires_at": 9_999_999_999i64,
            "relays": relays,
            "peers": peers,
        }))
        .expect("directory parses")
    }

    /// THE PRODUCER: peer entries in the signed directory become `Peer`-tier
    /// endpoints in the pool — the one channel a peer may arrive through.
    #[test]
    fn directory_peers_become_peer_tier_endpoints() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[("203.0.113.7:9444", &a), ("203.0.113.8:9444", &b)],
            serde_json::json!([{ "cert_b64": cert_b64_of(&p), "parked_at": ["203.0.113.8:9444", "203.0.113.7:9444"] }]),
        );

        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 3);
        assert!(endpoints[..2]
            .iter()
            .all(|e| e.tier == path::RelayTier::FirstParty));
        let peer = &endpoints[2];
        assert_eq!(peer.tier, path::RelayTier::Peer);
        assert_eq!(peer.cert.as_ref(), p.as_ref(), "the client pins the peer's own leaf");
        // The rendezvous is the first ADVERTISED relay it is parked at, so a peer
        // is never pinned to a node clients have stopped using.
        assert_eq!(peer.addr, "203.0.113.8:9444");
    }

    /// A directory with no `peers` produces exactly the pool it produced before
    /// wave 3 — and the factory built from it keeps the pre-wave-3 path length,
    /// so "no peers, no change" is a property of the code, not of the config.
    #[test]
    fn a_directory_without_peers_is_unchanged() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let dir: directory::Directory = serde_json::from_value(serde_json::json!({
            "version": 1, "issued_at": 1, "expires_at": 9_999_999_999i64,
            "relays": [
                { "addr": "203.0.113.7:9444", "region": "us-west-2", "cert_b64": cert_b64_of(&a) },
                { "addr": "203.0.113.8:9444", "region": "us-west-2", "cert_b64": cert_b64_of(&b) },
            ],
        }))
        .unwrap();
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 2);
        assert!(endpoints.iter().all(|e| e.tier == path::RelayTier::FirstParty));
    }

    /// Unusable peer entries are dropped — never dialed, never fatal to the
    /// directory. Each of these fails toward "fewer peers", i.e. shorter paths.
    #[test]
    fn unusable_peer_entries_are_dropped() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[("203.0.113.7:9444", &a)],
            serde_json::json!([
                // Not decodable as base64 → we could not pin this hop.
                { "cert_b64": "!!!not base64!!!", "parked_at": ["203.0.113.7:9444"] },
                // Parked only at a relay this directory does not advertise → no
                // relay we can reach can splice into its link.
                { "cert_b64": cert_b64_of(&p), "parked_at": ["198.51.100.9:9444"] },
                // Parked nowhere at all → same, and the reason `parked_at` is not
                // optional in practice.
                { "cert_b64": cert_b64_of(&p) },
            ]),
        );
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 1, "an unusable peer entered the pool");
        assert_eq!(endpoints[0].tier, path::RelayTier::FirstParty);
    }

    /// THE PRODUCER MEETS THE SELECTOR: a peer published by the signed directory
    /// is planned into the MIDDLE of a real 3-hop path — `first-party guard →
    /// peer → first-party exit` — and the pool raises its own path length to make
    /// room for it (the factory is built the production way, with no injected
    /// `target_hops`).
    ///
    /// This stops at the plan rather than the wire **because a parked peer is not
    /// dialable, which is the whole point of parking**: its endpoint address is a
    /// rendezvous — the first-party relay holding its link — and reaching it needs
    /// that relay to splice the `Extend` into the parked connection instead of
    /// dialing (wave 3's relay half). The byte-level proof that this selector's
    /// output carries traffic is `a_peer_relay_is_usable_as_a_middle_hop_on_the_wire`,
    /// which runs the same `plan_path` over a directly-dialable peer.
    #[test]
    fn a_directory_published_peer_is_planned_as_the_middle_hop() {
        let a = CertificateDer::from(vec![1u8; 24]);
        let b = CertificateDer::from(vec![2u8; 24]);
        let c = CertificateDer::from(vec![3u8; 24]);
        let p = CertificateDer::from(vec![9u8; 24]);
        let dir = directory_with(
            &[
                ("203.0.113.7:9444", &a),
                ("203.0.113.8:9444", &b),
                ("203.0.113.9:9444", &c),
            ],
            // v1 parks at every first-party relay, so any of them can reach it.
            serde_json::json!([{
                "cert_b64": cert_b64_of(&p),
                "parked_at": ["203.0.113.7:9444", "203.0.113.8:9444", "203.0.113.9:9444"],
            }]),
        );
        let endpoints = directory_to_endpoints(&dir);
        assert_eq!(endpoints.len(), 4);

        // A pool holding a peer raises its own target length; one without a peer
        // keeps the pre-wave-3 length. `RealRelayFactory::new` decides this, so it
        // is checked through the factory rather than restated here.
        let with_peer = pool_factory_for(&endpoints);
        assert_eq!(with_peer, path::MAX_PATH_HOPS);
        assert_eq!(
            pool_factory_for(&endpoints[..3]),
            path::TARGET_HOPS,
            "a peerless pool must not grow paths"
        );

        let order: Vec<usize> = (0..endpoints.len()).collect();
        let path = path::plan_path(&endpoints, &order, &[0], 1, 0, with_peer).expect("path");
        let hops = path.endpoints();
        assert_eq!(hops.len(), 3, "the peer did not earn its middle hop");
        assert_eq!(hops[0].tier, path::RelayTier::FirstParty, "the guard is ours");
        assert_eq!(hops[1].tier, path::RelayTier::Peer);
        assert_eq!(hops[1].cert.as_ref(), p.as_ref());
        assert_eq!(hops[2].tier, path::RelayTier::FirstParty, "the exit is ours");
    }

    /// The `target_hops` a production factory settles on for this pool.
    fn pool_factory_for(endpoints: &[RelayEndpoint]) -> usize {
        let state: Weak<AppState> = Weak::new();
        RealRelayFactory::new(
            state,
            endpoints.to_vec(),
            Duration::from_secs(30),
            Duration::from_secs(4),
            Arc::new(RelayAdmission::unenforced()),
            Arc::new(GuardBook::ephemeral()),
        )
        .target_hops
    }

    /// BELT AND BRACES with the reconciler: a peer whose cert is revoked is
    /// refused CLIENT-side even though the directory still advertises it — which
    /// is the case that matters, because a client can be holding a directory
    /// signed before the revocation. `cert_sha256_b64` is the only selector that
    /// can name a peer; its address belongs to the relay parking it.
    #[tokio::test]
    async fn a_revoked_peer_is_never_used_as_a_middle() {
        let origin = spawn_plain_http("revoked-peer-target").await;
        let a = spawn_pool_relay();
        let b = spawn_pool_relay();
        let c = spawn_pool_relay();
        let peer = spawn_pool_relay();
        let state = provisioned_state(cfg(OverlayMode::Prefer, None)).await;

        let dir = directory_with(
            &[
                (&a.addr.to_string(), &a.cert),
                (&b.addr.to_string(), &b.cert),
                (&c.addr.to_string(), &c.cert),
            ],
            serde_json::json!([{
                "cert_b64": cert_b64_of(&peer.cert),
                "parked_at": [a.addr.to_string(), b.addr.to_string(), c.addr.to_string()],
            }]),
        );
        let now = now_unix_secs() as i64;
        let gate = enforcing_gate_for(
            vec![serde_json::json!({ "cert_sha256_b64": cert_digest_b64_of(&peer.cert) })],
            now,
            now + 3_600,
        );

        let factory = RealRelayFactory::new(
            Arc::downgrade(&state),
            directory_to_endpoints(&dir),
            Duration::from_secs(30),
            Duration::from_secs(4),
            gate,
            Arc::new(GuardBook::ephemeral()),
        );

        const N: u64 = 4;
        for _ in 0..N {
            factory.connect(ORIGIN_NAME, origin.port()).await.unwrap();
        }
        assert_eq!(peer.stats.extends(), 0, "a REVOKED peer carried traffic");
        assert_eq!(peer.stats.dials(), 0);
        // Delivery still works — the pool routes around the revoked peer and
        // simply builds the shorter, first-party-only path.
        assert_eq!(
            a.stats.dials() + b.stats.dials() + c.stats.dials(),
            N,
            "revoking a peer must not cost delivery"
        );
    }

    // ── (h) #813 phase E1: LiveKit signaling over the overlay, media direct ───

    /// A factory that counts circuit requests, so a test can prove which flows
    /// reached the overlay and which never touched it.
    struct CountingFactory {
        inner: Arc<dyn CircuitFactory>,
        count: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl CircuitFactory for CountingFactory {
        async fn connect(&self, host: &str, port: u16) -> anyhow::Result<BoxedStream> {
            self.count.fetch_add(1, Ordering::Relaxed);
            self.inner.connect(host, port).await
        }
    }

    /// E1's load-bearing property: for ONE host, on ONE port, the signaling
    /// stream (TCP) is carried by a relay circuit while the media datagram (UDP)
    /// goes straight to the origin and never reaches the overlay at all.
    ///
    /// This is why §6.4's media rule survives putting LiveKit on the overlay host
    /// list: the shim is a SOCKS5 CONNECT proxy, so the plane split is enforced by
    /// the *transport*, which no host list could have expressed — signaling and
    /// media share a hostname.
    #[tokio::test]
    async fn media_plane_stays_direct_while_signaling_is_overlaid() {
        use tokio::net::UdpSocket;

        // One address serving both planes: UDP (media) and TCP (signaling) on the
        // same host and port, exactly as a real SFU presents itself.
        let media = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        let sfu_addr = media.local_addr().unwrap();
        let signaling = TcpListener::bind(sfu_addr).await.expect("same port, TCP side");
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = signaling.accept().await {
                tokio::spawn(async move {
                    let _ = read_http_head(&mut sock).await;
                    let _ = sock
                        .write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nsignaling",
                        )
                        .await;
                });
            }
        });
        // The media plane echoes whatever it is sent.
        tokio::spawn(async move {
            let mut buf = [0u8; 256];
            while let Ok((n, from)) = media.recv_from(&mut buf).await {
                let _ = media.send_to(&buf[..n], from).await;
            }
        });

        let host = sfu_addr.ip().to_string();
        let relay = spawn_relay(&[host.as_str()], &[]);
        let circuits = Arc::new(AtomicUsize::new(0));
        let factory: Arc<dyn CircuitFactory> = Arc::new(CountingFactory {
            inner: relay.factory(),
            count: circuits.clone(),
        });
        // The SFU host is on the overlay list — this is the E1 policy change.
        let policy = RoutingPolicy::new(
            OverlayMode::Strict,
            Allowlist::from_patterns([host.as_str()]),
            Allowlist::default(),
        );
        let shim = OverlayShim::start(policy, factory).await.unwrap();

        // (1) SIGNALING: a TCP stream to the SFU is carried by a circuit.
        let mut connector = SocksConnector::new(shim.socks_addr());
        let uri: Uri = format!("http://{host}:{}", sfu_addr.port()).parse().unwrap();
        let mut stream = Service::call(&mut connector, uri)
            .await
            .expect("signaling routes through the overlay");
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: sfu\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut resp = Vec::new();
        stream.read_to_end(&mut resp).await.unwrap();
        assert!(String::from_utf8_lossy(&resp).contains("signaling"));
        assert_eq!(circuits.load(Ordering::Relaxed), 1, "signaling used a circuit");
        assert_eq!(relay.stats.dials(), 1, "the relay dialed the SFU for signaling");

        // (2) MEDIA: a UDP datagram to the SAME host:port. The media stack owns
        //     its own socket and never dials the shim — and a SOCKS5 CONNECT
        //     proxy could not carry a datagram even if it did.
        let rtp = UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
        rtp.send_to(b"rtp-frame", sfu_addr).await.unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = tokio::time::timeout(Duration::from_secs(2), rtp.recv_from(&mut buf))
            .await
            .expect("media must not be blocked by the overlay")
            .unwrap();
        assert_eq!(&buf[..n], b"rtp-frame");
        assert_eq!(from, sfu_addr, "media came straight back from the SFU");

        assert_eq!(
            circuits.load(Ordering::Relaxed),
            1,
            "MEDIA WAS OVERLAID — §6.4's plane split is broken"
        );
        assert_eq!(
            relay.stats.dials(),
            1,
            "the relay saw a second dial; media must never traverse it"
        );
        drop(shim);
    }
}
