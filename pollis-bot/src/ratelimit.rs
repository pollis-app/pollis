//! The per-installation webhook rate limit: a token bucket.
//!
//! Default: a burst of [`DEFAULT_BURST`] calls, refilled at
//! [`DEFAULT_PER_MINUTE`] calls a minute. A call over the limit is answered
//! `429` with `Retry-After`, and nothing is posted.

use std::time::{Duration, Instant};

pub const DEFAULT_PER_MINUTE: u32 = 30;
pub const DEFAULT_BURST: u32 = 10;

#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: f64,
    per_second: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    /// Starts full. A `per_minute` or `burst` of 0 is taken as 1, so a
    /// misconfiguration slows the webhook down rather than silently closing it.
    pub fn new(per_minute: u32, burst: u32, now: Instant) -> Self {
        let capacity = f64::from(burst.max(1));
        Self {
            capacity,
            per_second: f64::from(per_minute.max(1)) / 60.0,
            tokens: capacity,
            last: now,
        }
    }

    /// Take one token, or say how long until one is available.
    pub fn try_take(&mut self, now: Instant) -> Result<(), Duration> {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            return Ok(());
        }
        let wait = (1.0 - self.tokens) / self.per_second;
        Err(Duration::from_secs_f64(wait))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_allowed_then_refused_then_refilled() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(60, 3, t0);
        for _ in 0..3 {
            assert!(b.try_take(t0).is_ok());
        }
        let wait = b.try_take(t0).unwrap_err();
        assert!(wait <= Duration::from_secs(1) && wait > Duration::ZERO, "{wait:?}");
        // One a second at 60/min.
        assert!(b.try_take(t0 + Duration::from_millis(1001)).is_ok());
        assert!(b.try_take(t0 + Duration::from_millis(1002)).is_err());
    }

    #[test]
    fn idle_time_never_banks_more_than_the_burst() {
        let t0 = Instant::now();
        let mut b = TokenBucket::new(60, 2, t0);
        let later = t0 + Duration::from_secs(3600);
        assert!(b.try_take(later).is_ok());
        assert!(b.try_take(later).is_ok());
        assert!(b.try_take(later).is_err());
    }
}
