//! Bandwidth caps.
//!
//! A GCRA-style token bucket: each caller reserves the bytes it is about to
//! send and gets back the instant it may send them. Reservations queue in
//! virtual time, so concurrent senders share the rate fairly and timer
//! lateness up to the burst tolerance is not lost.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::lock;

/// How much sending may run ahead of the ideal schedule, as time at the
/// configured rate. Also the size of the initial burst.
pub const TOLERANCE: Duration = Duration::from_millis(50);

#[derive(Debug)]
pub struct TokenBucket {
    rate: f64,
    /// Theoretical arrival time: when everything reserved so far is "paid".
    tat: Mutex<Instant>,
}

impl TokenBucket {
    /// A bucket for `rate` bytes per second (must be > 0).
    pub fn new(rate: u64) -> Self {
        Self {
            rate: rate.max(1) as f64,
            tat: Mutex::new(Instant::now()),
        }
    }

    pub fn rate(&self) -> u64 {
        self.rate as u64
    }

    /// Reserves `n` bytes; returns when they may be sent (now or later).
    pub fn reserve(&self, n: u64) -> Instant {
        let now = Instant::now();
        let cost = Duration::from_secs_f64(n as f64 / self.rate);
        let mut tat = lock(&self.tat);
        let new_tat = (*tat).max(now) + cost;
        *tat = new_tat;
        new_tat
            .checked_sub(TOLERANCE)
            .map_or(now, |allowed| allowed.max(now))
    }

    /// A chunk size that keeps pacing smooth at this rate (~10 ms of data).
    pub fn chunk_hint(rate: u64) -> usize {
        ((rate / 100) as usize).clamp(1024, crate::content::BLOCK_SIZE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schedule_matches_rate() {
        let b = TokenBucket::new(1_000_000);
        let start = Instant::now();
        let mut last = start;
        for _ in 0..100 {
            last = b.reserve(10_000);
        }
        // 1 MB at 1 MB/s, minus the tolerance head start.
        let took = last.duration_since(start).as_secs_f64();
        assert!((0.93..=1.0).contains(&took), "took {took}");
    }

    #[test]
    fn first_burst_is_free() {
        let b = TokenBucket::new(1_000_000);
        let now = Instant::now();
        assert!(b.reserve(40_000) <= now + Duration::from_millis(1));
    }

    #[test]
    fn chunk_hint_is_clamped() {
        assert_eq!(TokenBucket::chunk_hint(10), 1024);
        assert_eq!(TokenBucket::chunk_hint(2_000_000), 20_000);
        assert_eq!(TokenBucket::chunk_hint(u64::MAX / 2), 65_536);
    }
}
