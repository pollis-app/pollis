//! In-memory abuse control (design §11.5): a new-circuit token bucket and a
//! concurrent-circuit cap, each applied per source IP and per account.
//!
//! Every admission is keyed on an IP; there is no account-only path. An
//! opening `Layer` frame is unauthenticated, so an account-only path would let
//! any client escape the per-IP bounds, and self-minted accounts make the
//! per-account bound no bound (`docs/relay-operations.md` §2).
//!
//! [`ConnectionLimiter`] separately bounds QUIC connections per IP, which cost
//! an unauthenticated attacker nothing above the transport.
//!
//! No source IP is held: maps key on [`IpKey`], an HMAC under a per-boot
//! in-memory key, so a memory dump can't be reversed by enumerating addresses.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// A per-boot pseudonym for a source IP. The only constructor hashes, so a key
/// carrying a raw IP cannot exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IpKey([u8; 16]);

impl IpKey {
    /// Hash at the boundary (the accept loop) and pass the key, not the address.
    pub fn of(ip: IpAddr) -> IpKey {
        static BOOT: OnceLock<IpHasher> = OnceLock::new();
        BOOT.get_or_init(IpHasher::new).key(ip)
    }
}

/// One per boot; separate instances exist only so tests can show keys disagree.
struct IpHasher(HmacSha256);

impl IpHasher {
    fn new() -> Self {
        let mut key = [0u8; 32];
        getrandom::getrandom(&mut key).expect("OS CSPRNG unavailable");
        let mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
        key.fill(0);
        IpHasher(mac)
    }

    fn key(&self, ip: IpAddr) -> IpKey {
        let mut mac = self.0.clone();
        // Family tag so v4 and v6 inputs never collide.
        match ip {
            IpAddr::V4(a) => {
                mac.update(&[4]);
                mac.update(&a.octets());
            }
            IpAddr::V6(a) => {
                mac.update(&[6]);
                mac.update(&a.octets());
            }
        }
        let digest = mac.finalize().into_bytes();
        let mut out = [0u8; 16];
        out.copy_from_slice(&digest[..16]);
        IpKey(out)
    }
}

/// Per-key limits. Defaults are generous: the allowlist already closes the
/// open-proxy surface (§1.2); these blunt one misbehaving device.
#[derive(Debug, Clone, PartialEq)]
pub struct RateLimitConfig {
    pub new_circuits_per_min_per_ip: u32,
    pub new_circuits_per_min_per_account: u32,
    pub max_concurrent_per_ip: u32,
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

/// Capacity is the per-minute allowance, so a full minute can burst at once.
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

pub struct RateLimiter {
    cfg: RateLimitConfig,
    ip_buckets: Mutex<HashMap<IpKey, TokenBucket>>,
    account_buckets: Mutex<HashMap<[u8; 32], TokenBucket>>,
    ip_active: Mutex<HashMap<IpKey, u32>>,
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

    /// Admit a new circuit, or `None` if a limit is tripped. Hold the guard for
    /// the circuit's lifetime. Concurrency is reserved before tokens are spent,
    /// so a concurrency reject never burns a token.
    pub fn admit(self: &Arc<Self>, ip: IpKey, account: [u8; 32]) -> Option<CircuitGuard> {
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

    fn reserve_ip(&self, ip: IpKey) -> bool {
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

    fn release_ip(&self, ip: IpKey) {
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

/// Frees the concurrency slots on drop. Every guard holds a per-IP slot, so a
/// circuit no per-IP limit counts is unrepresentable.
pub struct CircuitGuard {
    limiter: Arc<RateLimiter>,
    ip: IpKey,
    account: [u8; 32],
}

impl Drop for CircuitGuard {
    fn drop(&mut self) {
        self.limiter.release_ip(self.ip);
        self.limiter.release_account(self.account);
    }
}

/// Bounds open QUIC connections per source IP. Applied at accept, after QUIC
/// address validation (so a spoofed Initial never reaches it) and before the
/// handshake.
pub struct ConnectionLimiter {
    max_per_ip: u32,
    active: Mutex<HashMap<IpKey, u32>>,
}

impl ConnectionLimiter {
    pub fn new(max_per_ip: u32) -> Arc<Self> {
        Arc::new(ConnectionLimiter {
            max_per_ip: max_per_ip.max(1),
            active: Mutex::new(HashMap::new()),
        })
    }

    pub fn acquire(self: &Arc<Self>, ip: IpKey) -> Option<ConnectionSlot> {
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

    pub fn active(&self, ip: IpKey) -> u32 {
        self.active.lock().unwrap().get(&ip).copied().unwrap_or(0)
    }

    fn release(&self, ip: IpKey) {
        let mut map = self.active.lock().unwrap();
        if let Some(n) = map.get_mut(&ip) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                map.remove(&ip);
            }
        }
    }
}

pub struct ConnectionSlot {
    limiter: Arc<ConnectionLimiter>,
    ip: IpKey,
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

    fn ip() -> IpKey {
        IpKey::of(IpAddr::V4(Ipv4Addr::LOCALHOST))
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
        assert!(rl.admit(ip(), acct).is_none(), "second must be rate-limited");
        drop(g1);
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
        assert!(rl.admit(ip(), [2u8; 32]).is_none(), "IP cap must not be escapable by account");
    }

    #[test]
    fn connection_limiter_caps_per_ip_and_frees_on_drop() {
        let cl = ConnectionLimiter::new(2);
        let other = IpKey::of(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)));
        let s1 = cl.acquire(ip()).expect("1st");
        let _s2 = cl.acquire(ip()).expect("2nd");
        assert!(cl.acquire(ip()).is_none(), "3rd connection from one IP is over the cap");
        assert_eq!(cl.active(ip()), 2);
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

    #[test]
    fn ip_keys_are_stable_pseudonyms_not_addresses() {
        let v4 = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let v6: IpAddr = "2001:db8::1".parse().unwrap();
        assert_eq!(IpKey::of(v4), IpKey::of(v4));
        assert_ne!(IpKey::of(v4), IpKey::of(v6));
        for ip in [v4, v6] {
            let key = IpKey::of(ip);
            let debug = format!("{key:?}");
            assert!(!debug.contains(&ip.to_string()), "{debug} names {ip}");
            let octets: Vec<u8> = match ip {
                IpAddr::V4(a) => a.octets().to_vec(),
                IpAddr::V6(a) => a.octets().to_vec(),
            };
            assert!(
                !key.0.windows(octets.len().min(16)).any(|w| w == &octets[..octets.len().min(16)]),
                "the raw address bytes appear in the key"
            );
        }
        assert_ne!(IpHasher::new().key(v4), IpHasher::new().key(v4));
    }

    #[test]
    fn limiting_is_still_per_ip_through_the_hash() {
        let cl = ConnectionLimiter::new(1);
        let a = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 1));
        let b = IpAddr::V4(Ipv4Addr::new(198, 51, 100, 2));
        let _held = cl.acquire(IpKey::of(a)).expect("first");
        assert!(cl.acquire(IpKey::of(a)).is_none());
        assert!(cl.acquire(IpKey::of(b)).is_some());
    }
}
