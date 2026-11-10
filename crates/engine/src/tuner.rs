//! Learned connection count (Speed Doctrine #3).
//!
//! A download starts at its host's `HostProfile::best_connections`, or at
//! `Tuning::initial_connections`, and then hill-climbs in measurement
//! windows:
//!
//! 1. Measure throughput `T(n)` once all `n` connections are streaming.
//! 2. Try `2n` (up to the ceiling). Keep it only if `T(2n) ≥ T(n) × (1 +
//!    climb_gain)`; while doubling pays, keep doubling.
//! 3. If the very first doubling did not pay, more connections don't help:
//!    try `n/2`, `n/4`, … down to 1, keeping each step as long as it still
//!    delivers `keep_ratio` of the best throughput seen. Fewer connections
//!    for the same speed cost less CPU and are politer, so this settles at
//!    1–2 connections when the bottleneck is not per connection.
//! 4. Settle.
//!
//! `Throttled` (429/503, connection refused under load) and connections
//! failing before their first byte while others work set a ceiling at the
//! number of connections that were working and settle. Measurement stops
//! when the remaining work is too small to measure, and while a global
//! speed limit is set (it would hide every difference).

use std::time::Duration;

use tokio::time::Instant;
use vayu_core::HostProfile;

use crate::config::Tuning;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    First,
    Up,
    Down,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Measure {
        dir: Dir,
        /// When every connection was streaming (the warm-up starts).
        ready: Option<Instant>,
        start: Option<(Instant, u64)>,
    },
    Settled,
}

/// One observation, taken every scheduler tick.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sample {
    pub now: Instant,
    /// Bytes received by all connections so far.
    pub bytes: u64,
    /// Owning connections that have delivered their first byte.
    pub streaming: u16,
    /// Bytes not yet claimed by any connection.
    pub work_left: u64,
    /// A global speed limit is active.
    pub limited: bool,
}

#[derive(Debug)]
pub(crate) struct Tuner {
    fixed: bool,
    /// `AddParams::connections`: this count, not capped by the settings
    /// (only lowered by throttling).
    pinned: bool,
    target: u16,
    max: u16,
    ceiling: u16,
    gain: f64,
    keep: f64,
    window: Duration,
    state: State,
    best: Option<(u16, f64)>,
    peak: f64,
    climbed: bool,
    throttled: bool,
}

impl Tuner {
    pub(crate) fn new(
        t: &Tuning,
        max: u16,
        profile: Option<&HostProfile>,
        pinned: Option<u16>,
    ) -> Self {
        let max = pinned.unwrap_or(max).max(1);
        let ceiling = profile
            .and_then(|p| p.max_ok_connections)
            .unwrap_or(u16::MAX)
            .max(1);
        let (fixed, start) = match pinned.or(t.fixed_connections) {
            Some(n) => (true, n),
            None => (
                false,
                profile
                    .and_then(|p| p.best_connections)
                    .unwrap_or(t.initial_connections),
            ),
        };
        let mut tuner = Self {
            fixed,
            pinned: pinned.is_some(),
            target: start.clamp(1, max),
            max,
            ceiling,
            gain: t.climb_gain,
            keep: t.keep_ratio,
            window: t.tune_window,
            state: if fixed {
                State::Settled
            } else {
                State::Measure {
                    dir: Dir::First,
                    ready: None,
                    start: None,
                }
            },
            best: None,
            peak: 0.0,
            climbed: false,
            throttled: false,
        };
        if !fixed {
            tuner.target = tuner.target.min(ceiling);
        }
        tuner
    }

    /// The number of owning connections to run now.
    pub(crate) fn target(&self) -> u16 {
        self.target
    }

    /// The count that gave the best throughput (for the host profile).
    pub(crate) fn best(&self) -> u16 {
        self.best.map_or(self.target, |b| b.0)
    }

    /// The highest count known not to trigger throttling.
    pub(crate) fn max_ok(&self) -> Option<u16> {
        self.throttled.then_some(self.ceiling)
    }

    pub(crate) fn settled(&self) -> bool {
        self.state == State::Settled
    }

    /// `Settings::max_connections_per_download` changed.
    pub(crate) fn set_max(&mut self, max: u16) {
        if self.pinned {
            return;
        }
        self.max = max.max(1);
        self.target = self.target.min(self.max);
    }

    fn cap(&self) -> u16 {
        self.max.min(self.ceiling)
    }

    pub(crate) fn tick(&mut self, s: Sample) {
        let State::Measure { dir, ready, start } = self.state else {
            return;
        };
        if s.limited {
            self.settle();
            return;
        }
        let Some((t0, b0)) = start else {
            // Wait until every connection streams, then half a window more
            // so new connections ramp up and cancelled ones drain.
            self.state = match ready {
                _ if s.streaming < self.target => State::Measure {
                    dir,
                    ready: None,
                    start: None,
                },
                None => State::Measure {
                    dir,
                    ready: Some(s.now),
                    start: None,
                },
                Some(r) if s.now.duration_since(r) >= self.window / 2 => State::Measure {
                    dir,
                    ready,
                    start: Some((s.now, s.bytes)),
                },
                Some(_) => return,
            };
            return;
        };
        let elapsed = s.now.duration_since(t0);
        if elapsed < self.window {
            return;
        }
        let tput = s.bytes.saturating_sub(b0) as f64 / elapsed.as_secs_f64();
        // Too little left to measure the next step reliably.
        if (s.work_left as f64) < tput * self.window.as_secs_f64() * 3.0 {
            if dir == Dir::First {
                self.best = Some((self.target, tput));
            }
            self.settle();
            return;
        }
        self.peak = self.peak.max(tput);
        match dir {
            Dir::First => {
                self.best = Some((self.target, tput));
                if !self.try_up() {
                    self.try_down();
                }
            }
            Dir::Up => {
                let (bn, bt) = self.best.unwrap_or((self.target, 0.0));
                if tput >= bt * (1.0 + self.gain) {
                    self.best = Some((self.target, tput));
                    self.climbed = true;
                    if !self.try_up() {
                        self.settle();
                    }
                } else {
                    self.target = bn;
                    if self.climbed {
                        self.settle();
                    } else {
                        self.try_down();
                    }
                }
            }
            Dir::Down => {
                if tput >= self.peak * self.keep {
                    self.best = Some((self.target, tput));
                    self.try_down();
                } else {
                    self.target = self.best.map_or(self.target, |b| b.0);
                    self.settle();
                }
            }
        }
    }

    /// Start measuring twice the connections; false when at the cap.
    fn try_up(&mut self) -> bool {
        let next = self.target.saturating_mul(2).min(self.cap());
        if next > self.target {
            self.target = next;
            self.measure(Dir::Up);
            true
        } else {
            false
        }
    }

    fn try_down(&mut self) {
        if self.target > 1 {
            self.target = (self.target / 2).max(1);
            self.measure(Dir::Down);
        } else {
            self.settle();
        }
    }

    fn measure(&mut self, dir: Dir) {
        self.state = State::Measure {
            dir,
            ready: None,
            start: None,
        };
    }

    fn settle(&mut self) {
        self.state = State::Settled;
    }

    /// The server limited us while `working` connections were accepted.
    /// `repeat` is set for further failures of the same backoff round:
    /// with nothing working they do not halve the count again.
    pub(crate) fn on_throttle(&mut self, working: u16, repeat: bool) {
        let ceiling = if working > 0 {
            working
        } else if repeat {
            self.ceiling.min(self.target)
        } else {
            (self.target / 2).max(1)
        };
        self.ceiling = self.ceiling.min(ceiling).max(1);
        self.throttled = true;
        if self.target > self.ceiling {
            self.target = self.ceiling;
        }
        if self.best.is_none_or(|b| b.0 > self.ceiling) {
            self.best = Some((self.ceiling, 0.0));
        }
        if !self.fixed {
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the tuner against a throughput model `f(n)` in bytes/s.
    fn run(t: &mut Tuner, f: impl Fn(u16) -> f64) -> Vec<u16> {
        let mut now = Instant::now();
        let mut bytes = 0u64;
        let mut seen = vec![t.target()];
        let step = Duration::from_millis(50);
        for _ in 0..400 {
            now += step;
            bytes += (f(t.target()) * step.as_secs_f64()) as u64;
            t.tick(Sample {
                now,
                bytes,
                streaming: t.target(),
                work_left: u64::MAX / 2,
                limited: false,
            });
            if *seen.last().unwrap() != t.target() {
                seen.push(t.target());
            }
        }
        seen
    }

    fn tuner(max: u16) -> Tuner {
        Tuner::new(&Tuning::default(), max, None, None)
    }

    #[test]
    fn climbs_while_connections_pay() {
        let mut t = tuner(16);
        let path = run(&mut t, |n| n as f64 * 2e6); // per-connection limit
        assert_eq!(path, vec![4, 8, 16]);
        assert!(t.settled());
        assert_eq!(t.best(), 16);
    }

    #[test]
    fn settles_low_when_connections_do_not_help() {
        let mut t = tuner(16);
        let path = run(&mut t, |_| 20e6); // global limit
        assert_eq!(path, vec![4, 8, 2, 1]);
        assert_eq!((t.target(), t.best()), (1, 1));
    }

    #[test]
    fn stops_where_fewer_connections_lose_speed() {
        let mut t = tuner(16);
        // Two connections saturate the link; one gets 60 %.
        let path = run(&mut t, |n| if n >= 2 { 100e6 } else { 60e6 });
        assert_eq!(path, vec![4, 8, 2, 1, 2]);
        assert_eq!(t.target(), 2);
    }

    #[test]
    fn climbs_partway_then_stops() {
        let mut t = tuner(64);
        let path = run(&mut t, |n| (n.min(9) as f64) * 1e6);
        assert_eq!(path, vec![4, 8, 16, 8]);
        assert_eq!(t.target(), 8);
    }

    #[test]
    fn throttling_sets_a_ceiling() {
        let mut t = tuner(16);
        t.on_throttle(2, false);
        assert_eq!((t.target(), t.max_ok()), (2, Some(2)));
        assert!(t.settled());
        let mut t = tuner(16);
        t.on_throttle(0, false);
        assert_eq!(t.target(), 2);
        t.on_throttle(0, true);
        assert_eq!(t.target(), 2, "same round: no second halving");
        t.on_throttle(0, false);
        assert_eq!(t.target(), 1);
    }

    #[test]
    fn profile_and_fixed_counts() {
        let p = HostProfile {
            best_connections: Some(12),
            max_ok_connections: Some(6),
            ..HostProfile::default()
        };
        assert_eq!(
            Tuner::new(&Tuning::default(), 16, Some(&p), None).target(),
            6
        );
        let fixed = Tuning {
            fixed_connections: Some(16),
            ..Tuning::default()
        };
        let mut t = Tuner::new(&fixed, 8, None, None);
        assert_eq!(t.target(), 8);
        assert_eq!(run(&mut t, |_| 1e6), vec![8]);
        t.set_max(3);
        assert_eq!(t.target(), 3);
        // A per-download count wins over the engine-wide one and the
        // settings cap, and is still lowered by throttling.
        let mut t = Tuner::new(&fixed, 8, Some(&p), Some(32));
        assert_eq!(t.target(), 32);
        assert_eq!(run(&mut t, |_| 1e6), vec![32]);
        t.set_max(3);
        assert_eq!(t.target(), 32);
        t.on_throttle(5, false);
        assert_eq!(t.target(), 5);
    }

    #[test]
    fn a_speed_limit_freezes_learning() {
        let mut t = tuner(16);
        t.tick(Sample {
            now: Instant::now(),
            bytes: 0,
            streaming: 4,
            work_left: u64::MAX,
            limited: true,
        });
        assert!(t.settled());
        assert_eq!(t.target(), 4);
    }
}
