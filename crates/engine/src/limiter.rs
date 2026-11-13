//! The global speed limit (`Settings::speed_limit_bps`): one token bucket
//! shared by every connection of every download.
//!
//! Unlimited costs one atomic load per chunk. Limited, a connection
//! reserves tokens for each chunk it received (going into debt if needed)
//! and sleeps off the debt, so the long-run rate is exact and connections
//! share it in proportion to what they receive.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

#[derive(Debug)]
pub(crate) struct RateLimiter {
    /// Bytes per second; 0 = unlimited.
    rate: AtomicU64,
    bucket: Mutex<Bucket>,
}

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl RateLimiter {
    pub(crate) fn new(rate: Option<u64>) -> Self {
        Self {
            rate: AtomicU64::new(rate.unwrap_or(0)),
            bucket: Mutex::new(Bucket {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub(crate) fn set_rate(&self, rate: Option<u64>) {
        self.rate.store(rate.unwrap_or(0), Ordering::Relaxed);
    }

    /// True when a limit is set.
    pub(crate) fn limited(&self) -> bool {
        self.rate.load(Ordering::Relaxed) != 0
    }

    /// Account for `n` received bytes, waiting as long as the limit needs.
    pub(crate) async fn acquire(&self, n: u64) {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return;
        }
        let wait = {
            let mut b = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            let rate_f = rate as f64;
            // Allow a burst of 50 ms worth of bytes, at least one 64 KiB chunk.
            let burst = (rate_f * 0.05).max(64.0 * 1024.0);
            let elapsed = now.duration_since(b.last).as_secs_f64();
            b.tokens = (b.tokens + elapsed * rate_f).min(burst);
            b.last = now;
            b.tokens -= n as f64;
            (b.tokens < 0.0).then(|| Duration::from_secs_f64(-b.tokens / rate_f))
        };
        if let Some(w) = wait {
            tokio::time::sleep(w).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn enforces_the_rate() {
        let l = RateLimiter::new(Some(1_000_000));
        let t0 = Instant::now();
        for _ in 0..100 {
            l.acquire(100_000).await; // 10 MB in total
        }
        let secs = t0.elapsed().as_secs_f64();
        assert!((9.8..=10.2).contains(&secs), "{secs}");
        l.set_rate(None);
        assert!(!l.limited());
        let t1 = Instant::now();
        l.acquire(u64::MAX / 4).await;
        assert_eq!(t1.elapsed(), Duration::ZERO);
    }
}
