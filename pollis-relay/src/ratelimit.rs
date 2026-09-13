//! In-memory abuse control for the relay (design §11.5, §14 Slice 2a).
//!
//! Two knobs per key, applied to BOTH the source IP and the authenticated
//! account (`sha256(account_id_pub)`, see `proto::account_fingerprint`):
//!   1. a **new-circuit token bucket** (rate) — smooths bursts of stream opens;
//!   2. a **concurrent-circuit cap** — bounds simultaneously-open pipes.
//!
//! Everything is in-memory (no external store, per the task): a `HashMap` of
//! token buckets keyed by IP / account, plus counters for live circuits. A
//! [`CircuitGuard`] is returned on admission and decrements the concurrency
//! counters on drop, so the accounting self-heals when a pipe closes for any
//! reason. On breach the caller returns a clean `Rejected(RateLimited)` rather
//! than dropping the stream.
//!
//! Every admission is keyed on an IP. There is no account-only path: a stream
//! that reaches this node inside an onion layer is keyed on the address of the
//! QUIC peer that carried it — the previous relay, or whoever is *claiming* to be
//! one, since an opening `Layer` frame is unauthenticated and any client can send
//! it. An account-only path would let that client escape every per-IP bound by
//! prefixing one frame, and with self-minted accounts the per-account bound is
//! not a bound at all (`docs/relay-operations.md` §2). The cost is that a busy
//! guard's fan-in to one exit is bounded per IP too; operators size
//! `max_concurrent_per_ip` / `new_circuits_per_min_per_ip` for that fan-in.
//!
//! A second, coarser limiter — [`ConnectionLimiter`] — bounds **QUIC
//! connections** per source IP before any stream exists. A connection is the
//! unit of an unauthenticated attacker's cost: it needs no handshake frame, only
//! keep-alives, so nothing above the transport would ever see it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Tunable limits. All four are per-key (one key = one IP, one key = one
/// account). Defaults are deliberately generous — the allowlist already closes
/// the open-proxy abuse surface (§1.2); these exist to blunt a single misbehaving
/// or captured device, not to ration normal use.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    /// New circuits per minute allowed per source IP (token-bucket refill).
    pub new_circuits_per_min_per_ip: u32,
    /// New circuits per minute allowed per account.
    pub new_circuits_per_min_per_account: u32,
    /// Max simultaneously-open circuits per source IP.
    pub max_concurrent_per_ip: u32,
    /// Max simultaneously-open circuits per account.
    pub max_concurrent_per_account: u32,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        RateLimitConfig {
            new_circuits_per_min_per_ip: 600,
            new_circuits_per_min_per_account: 600,
            max_concurrent_per_ip: 256,
            max_concurrent_per_account: 128,
        }
    }
}

/// A classic token bucket. Capacity == the per-minute allowance, so a full
/// minute's worth can burst at once, then refills at `allowance / 60` per second.
#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    capacity: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(per_min: u32, now: Instant) -> Self {
        let capacity = per_min.max(1) as f64;
        TokenBucket {
            tokens: capacity,
            capacity,
            refill_per_sec: capacity / 60.0,
            last: now,
        }
    }

    /// Refill for elapsed time and take one token if available.
    fn try_take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// The shared limiter. Cheap to clone via `Arc`.
pub struct RateLimiter {
    cfg: RateLimitConfig,
    ip_buckets: Mutex<HashMap<IpAddr, TokenBucket>>,
    account_buckets: Mutex<HashMap<[u8; 32], TokenBucket>>,
    ip_active: Mutex<HashMap<IpAddr, u32>>,
    account_active: Mutex<HashMap<[u8; 32], u32>>,
}

impl RateLimiter {
    pub fn new(cfg: RateLimitConfig) -> Arc<Self> {
        Arc::new(RateLimiter {
            cfg,
            ip_buckets: Mutex::new(HashMap::new()),
            account_buckets: Mutex::new(HashMap::new()),
            ip_active: Mutex::new(HashMap::new()),
            account_active: Mutex::new(HashMap::new()),
        })
    }

    /// Admit a new circuit for `(ip, account)`, or `None` if a limit is tripped.
    /// On success returns a [`CircuitGuard`] that must be held for the circuit's
    /// lifetime; dropping it frees the concurrency slots. Concurrency is reserved
    /// first (reversible), then the rate tokens are spent — so a concurrency
    /// reject never burns a token.
    pub fn admit(self: &Arc<Self>, ip: IpAddr, account: [u8; 32]) -> Option<CircuitGuard> {
        if !self.reserve_ip(ip) {
            return None;
        }
        if !self.reserve_account(account) {
            self.release_ip(ip);
            return None;
        }

        let now = Instant::now();
        let ip_ok = self
            .ip_buckets
            .lock()
            .unwrap()
            .entry(ip)
            .or_insert_with(|| TokenBucket::new(self.cfg.new_circuits_per_min_per_ip, now))
            .try_take(now);
        let account_ok = self
            .account_buckets
            .lock()
            .unwrap()
            .entry(account)
            .or_insert_with(|| TokenBucket::new(self.cfg.new_circuits_per_min_per_account, now))
            .try_take(now);

        if !ip_ok || !account_ok {
            self.release_ip(ip);
            self.release_account(account);
            return None;
        }

        Some(CircuitGuard {
            limiter: self.clone(),
            ip,
            account,
        })
    }

    fn reserve_ip(&self, ip: IpAddr) -> bool {
        let mut map = self.ip_active.lock().unwrap();
        let n = map.entry(ip).or_insert(0);
        if *n >= self.cfg.max_concurrent_per_ip {
            return false;
        }
        *n += 1;
        true
    }

    fn reserve_account(&self, account: [u8; 32]) -> bool {
        let mut map = self.account_active.lock().unwrap();
        let n = map.entry(account).or_insert(0);
        if *n >= self.cfg.max_concurrent_per_account {
            return false;
        }
        *n += 1;
        true
    }

    fn release_ip(&self, ip: IpAddr) {
        let mut map = self.ip_active.lock().unwrap();
        if let Some(n) = map.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&ip);
            }
        }
    }

    fn release_account(&self, account: [u8; 32]) {
        let mut map = self.account_active.lock().unwrap();
        if let Some(n) = map.get_mut(&account) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&account);
            }
        }
    }
}

/// Held for a circuit's lifetime; frees the per-IP and per-account concurrency
/// slots when dropped. Every guard holds a per-IP slot — there is no way to
/// build one without an address, which is what makes "a circuit that no per-IP
/// limit counts" unrepresentable rather than merely avoided.
pub struct CircuitGuard {
    limiter: Arc<RateLimiter>,
    ip: IpAddr,
    account: [u8; 32],
}

impl Drop for CircuitGuard {
    fn drop(&mut self) {
        self.limiter.release_ip(self.ip);
        self.limiter.release_account(self.account);
    }
}

/// Bounds simultaneously-open **QUIC connections** per source IP.
///
/// Applied at accept, after QUIC address validation has proved the source can
/// receive at that address (so a spoofed Initial never reaches it), and before
/// the handshake runs — so the cost of holding a slot is borne by the address
/// that holds it. Every slot is released by dropping its [`ConnectionSlot`].
pub struct ConnectionLimiter {
    max_per_ip: u32,
    active: Mutex<HashMap<IpAddr, u32>>,
}

impl ConnectionLimiter {
    pub fn new(max_per_ip: u32) -> Arc<Self> {
        Arc::new(ConnectionLimiter {
            // Zero would refuse every connection; the accept loop treats the cap
            // as "at least one", the same way it treats the global cap.
            max_per_ip: max_per_ip.max(1),
            active: Mutex::new(HashMap::new()),
        })
    }

    /// Take a connection slot for `ip`, or `None` if it is at its cap.
    pub fn acquire(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionSlot> {
        let mut map = self.active.lock().unwrap();
        let n = map.entry(ip).or_insert(0);
        if *n >= self.max_per_ip {
            return None;
        }
        *n += 1;
        Some(ConnectionSlot {
            limiter: self.clone(),
            ip,
        })
    }

    /// Connections currently holding a slot for `ip`.
    pub fn active(&self, ip: IpAddr) -> u32 {
        self.active.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }

    fn release(&self, ip: IpAddr) {
        let mut map = self.active.lock().unwrap();
        if let Some(n) = map.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&ip);
            }
        }
    }
}

/// One QUIC connection's per-IP slot; released on drop.
pub struct ConnectionSlot {
    limiter: Arc<ConnectionLimiter>,
    ip: IpAddr,
}

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.limiter.release(self.ip);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn ip() -> IpAddr {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    }

    #[test]
    fn concurrent_cap_trips_and_recovers() {
        let cfg = RateLimitConfig {
            max_concurrent_per_account: 1,
            ..Default::default()
        };
        let rl = RateLimiter::new(cfg);
        let acct = [1u8; 32];

        let g1 = rl.admit(ip(), acct).expect("first admitted");
        // Second concurrent circuit for the same account is over the cap.
        assert!(rl.admit(ip(), acct).is_none(), "second must be rate-limited");
        drop(g1);
        // Slot freed → a new circuit is admitted again.
        assert!(rl.admit(ip(), acct).is_some(), "slot must free on drop");
    }

    #[test]
    fn rate_bucket_trips_when_burst_exhausted() {
        let cfg = RateLimitConfig {
            new_circuits_per_min_per_account: 2,
            max_concurrent_per_account: 1000,
            max_concurrent_per_ip: 1000,
            new_circuits_per_min_per_ip: 1000,
        };
        let rl = RateLimiter::new(cfg);
        let acct = [2u8; 32];
        // Hold the guards so concurrency isn't the limiter — only the rate bucket.
        let _g1 = rl.admit(ip(), acct).expect("1st");
        let _g2 = rl.admit(ip(), acct).expect("2nd");
        assert!(rl.admit(ip(), acct).is_none(), "3rd exhausts the 2-token bucket");
    }

    #[test]
    fn distinct_accounts_are_independent() {
        let cfg = RateLimitConfig {
            max_concurrent_per_account: 1,
            max_concurrent_per_ip: 1000,
            ..Default::default()
        };
        let rl = RateLimiter::new(cfg);
        let _g1 = rl.admit(ip(), [1u8; 32]).expect("acct 1");
        // A different account is unaffected by acct 1's concurrency use.
        assert!(rl.admit(ip(), [2u8; 32]).is_some());
    }

    #[test]
    fn per_ip_cap_binds_across_accounts() {
        let cfg = RateLimitConfig {
            max_concurrent_per_ip: 1,
            ..Default::default()
        };
        let rl = RateLimiter::new(cfg);
        let _g1 = rl.admit(ip(), [1u8; 32]).expect("first from this IP");
        // A fresh (self-minted) account buys nothing: the IP is at its cap.
        assert!(rl.admit(ip(), [2u8; 32]).is_none(), "IP cap must not be escapable by account");
    }

    #[test]
    fn connection_limiter_caps_per_ip_and_frees_on_drop() {
        let cl = ConnectionLimiter::new(2);
        let other = IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2));
        let s1 = cl.acquire(ip()).expect("1st");
        let _s2 = cl.acquire(ip()).expect("2nd");
        assert!(cl.acquire(ip()).is_none(), "3rd connection from one IP is over the cap");
        assert_eq!(cl.active(ip()), 2);
        // Another address is independent.
        let _o = cl.acquire(other).expect("other IP unaffected");
        drop(s1);
        assert_eq!(cl.active(ip()), 1);
        assert!(cl.acquire(ip()).is_some(), "slot frees on drop");
    }

    #[test]
    fn connection_limiter_zero_means_one() {
        let cl = ConnectionLimiter::new(0);
        let _held = cl.acquire(ip()).expect("a zero cap must not refuse everyone");
        assert!(cl.acquire(ip()).is_none());
    }
}
