//! Learned connection count (Speed Doctrine #3), robust to real networks.
//!
//! A download starts at its host's `HostProfile::best_connections`, or at
//! `Tuning::initial_connections`, and hill-climbs:
//!
//! 1. Measure throughput at the current count `n` (the base).
//! 2. Try `2n` (up to the cap). Keep it if it adds at least `climb_gain`;
//!    while doubling pays, keep doubling.
//! 3. If the very first doubling did not pay, more connections don't help:
//!    try `n/2`, `n/4`, … while each still delivers `keep_ratio` of the best
//!    throughput seen. Fewer connections for the same speed cost less CPU
//!    and are politer.
//! 4. Settle. Every `tune_reprobe` (doubling while it does not pay, up to
//!    5 min) measure the base again and try one step up: conditions change
//!    during a long download.
//!
//! What makes it robust on a real link, where a new connection needs
//! several round trips of slow start and throughput swings ±30 % from one
//! second to the next:
//!
//! - **Windows sized from the run.** A window lasts at least
//!   `max(tune_window, tune_rtt_windows × median TTFB)` (≈ 1–2 s on the
//!   internet, 400 ms on loopback) and is extended, up to `tune_window_max`,
//!   until the relative standard error (RSE) of its `tune_sample`-long
//!   throughput samples is at most `tune_target_rse`.
//! - **Warm-up.** Measuring starts only once every connection streams, at
//!   least `max(tune_window / 2, 4 × TTFB)` later, and not while throughput
//!   is still rising (slow start), for at most `tune_window_max`.
//! - **Decisions beyond noise.** A step is taken (or refused) at once only
//!   when its effect clears `tune_z` × the combined RSE of the two windows.
//!   Otherwise the base count is measured again and the trial is compared
//!   with the mean of the base windows before and after it (A-B-A), which
//!   cancels drift. A step down after such a confirmation must also hold up
//!   beyond one standard error, so noise alone never takes the count below
//!   the best one seen.
//!
//! `Throttled` (429/503, refused under load), or connections failing before
//! their first byte while others work, set a ceiling at the number of
//! accepted connections and settle. Learning pauses while a global speed
//! limit is set and when too little is left to measure. A count pinned by
//! `AddParams::connections` or `Tuning::fixed_connections` is not learned
//! (but still lowered by throttling).

use std::time::Duration;

use tokio::time::Instant;
use vayu_core::HostProfile;

use crate::config::Tuning;

/// Longest gap between re-probes.
const REPROBE_MAX: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Down,
}

/// One measured window.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Stat {
    n: u16,
    /// Bytes per second over the window.
    mean: f64,
    /// Relative standard error of `mean`.
    rse: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Goal {
    /// Measure the current count (first, or before a re-probe).
    Base { reprobe: bool },
    /// Measure the current count against `base`.
    Trial { dir: Dir, base: Stat },
    /// Back at `base.n` after an inconclusive trial.
    Confirm { dir: Dir, base: Stat, trial: Stat },
}

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Warm {
        /// When every connection was streaming.
        ready: Option<Instant>,
        last: Option<(Instant, u64)>,
        rates: Vec<f64>,
    },
    Window {
        start: (Instant, u64),
        last: (Instant, u64),
        rates: Vec<f64>,
    },
}

impl Phase {
    fn warm() -> Self {
        Phase::Warm {
            ready: None,
            last: None,
            rates: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum State {
    Running { goal: Goal, phase: Phase },
    Settled { since: Option<Instant> },
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
    /// Median time to first byte of this run's connections, seconds.
    pub ttfb: Option<f64>,
}

#[derive(Debug, Clone)]
struct Knobs {
    gain: f64,
    keep: f64,
    window: Duration,
    window_max: Duration,
    rtt_windows: f64,
    sample: Duration,
    target_rse: f64,
    warm_rtts: f64,
    warm_rise: f64,
    z: f64,
    reprobe: Duration,
}

#[derive(Debug)]
pub(crate) struct Tuner {
    fixed: bool,
    pinned: bool,
    target: u16,
    max: u16,
    ceiling: u16,
    k: Knobs,
    state: State,
    /// The accepted count and what it measured.
    best: Option<Stat>,
    /// The highest throughput measured (steps down are judged against it).
    peak: Option<Stat>,
    climbed: bool,
    throttled: bool,
    /// Has settled at least once: later climbs are re-probes.
    settled_once: bool,
    reprobe_every: Duration,
}

/// Relative standard error of the mean of `v`.
fn rse(v: &[f64]) -> f64 {
    let n = v.len();
    if n < 2 {
        return f64::INFINITY;
    }
    let mean = v.iter().sum::<f64>() / n as f64;
    if mean <= 0.0 {
        return f64::INFINITY;
    }
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;
    var.sqrt() / mean / (n as f64).sqrt()
}

/// Record a throughput sample once `period` has passed since the last one.
fn take_sample(
    last: &mut (Instant, u64),
    rates: &mut Vec<f64>,
    now: Instant,
    bytes: u64,
    period: Duration,
) {
    let dt = now.duration_since(last.0);
    if dt >= period {
        rates.push(bytes.saturating_sub(last.1) as f64 / dt.as_secs_f64());
        *last = (now, bytes);
    }
}

fn scaled(ttfb: Option<f64>, factor: f64) -> Duration {
    Duration::try_from_secs_f64(ttfb.unwrap_or(0.0).max(0.0) * factor).unwrap_or(Duration::MAX)
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
            k: Knobs {
                gain: t.climb_gain,
                keep: t.keep_ratio,
                window: t.tune_window,
                window_max: t.tune_window_max.max(t.tune_window),
                rtt_windows: t.tune_rtt_windows,
                sample: t.tune_sample,
                target_rse: t.tune_target_rse,
                warm_rtts: t.tune_warm_rtts,
                warm_rise: t.tune_warm_rise,
                z: t.tune_z,
                reprobe: t.tune_reprobe,
            },
            state: if fixed {
                State::Settled { since: None }
            } else {
                State::Running {
                    goal: Goal::Base { reprobe: false },
                    phase: Phase::warm(),
                }
            },
            best: None,
            peak: None,
            climbed: false,
            throttled: false,
            settled_once: false,
            reprobe_every: t.tune_reprobe,
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
        self.best.map_or(self.target, |b| b.n)
    }

    /// The highest count known not to trigger throttling.
    pub(crate) fn max_ok(&self) -> Option<u16> {
        self.throttled.then_some(self.ceiling)
    }

    pub(crate) fn settled(&self) -> bool {
        matches!(self.state, State::Settled { .. })
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
        if self.fixed {
            return;
        }
        if s.limited {
            self.settle();
            return;
        }
        let k = self.k.clone();
        let min_len = k
            .window
            .max(scaled(s.ttfb, k.rtt_windows))
            .min(k.window_max);
        let warm_min = (k.window / 2)
            .max(scaled(s.ttfb, k.warm_rtts))
            .min(k.window_max);
        let target = self.target;
        let can_up = self.target < self.cap();
        let reprobe_every = self.reprobe_every;
        let (goal, stat) = match &mut self.state {
            State::Settled { since } => {
                let since = *since.get_or_insert(s.now);
                if can_up && !self.throttled && s.now.duration_since(since) >= reprobe_every {
                    self.state = State::Running {
                        goal: Goal::Base { reprobe: true },
                        phase: Phase::warm(),
                    };
                }
                return;
            }
            State::Running { goal, phase } => match phase {
                Phase::Warm { ready, last, rates } => {
                    if s.streaming < target {
                        *ready = None;
                        *last = None;
                        rates.clear();
                        return;
                    }
                    let ready_at = *ready.get_or_insert(s.now);
                    let l = last.get_or_insert((s.now, s.bytes));
                    take_sample(l, rates, s.now, s.bytes, k.sample);
                    let waited = s.now.duration_since(ready_at);
                    let rising = rates.len() >= 2
                        && rates[rates.len() - 1] > rates[rates.len() - 2] * (1.0 + k.warm_rise);
                    if waited >= warm_min && (!rising || waited >= k.window_max) {
                        *phase = Phase::Window {
                            start: (s.now, s.bytes),
                            last: (s.now, s.bytes),
                            rates: Vec::new(),
                        };
                    }
                    return;
                }
                Phase::Window { start, last, rates } => {
                    take_sample(last, rates, s.now, s.bytes, k.sample);
                    let len = s.now.duration_since(start.0);
                    let r = rse(rates);
                    if len < min_len || rates.len() < 2 || (r > k.target_rse && len < k.window_max)
                    {
                        return;
                    }
                    let stat = Stat {
                        n: target,
                        mean: s.bytes.saturating_sub(start.1) as f64 / len.as_secs_f64(),
                        rse: r.min(1.0),
                    };
                    (*goal, stat)
                }
            },
        };
        // Too little left to measure another step reliably.
        let next_step = (min_len + warm_min).as_secs_f64() * 2.0;
        if (s.work_left as f64) < stat.mean * next_step {
            if self.best.is_none() {
                self.best = Some(stat);
            }
            self.settle();
            return;
        }
        self.decide(goal, stat);
    }

    /// The noise margin for comparing two windows (relative).
    fn margin(&self, a: Stat, b: Stat) -> f64 {
        self.k.z * (a.rse.powi(2) + b.rse.powi(2)).sqrt()
    }

    fn decide(&mut self, goal: Goal, stat: Stat) {
        match goal {
            Goal::Base { reprobe } => {
                self.best = Some(stat);
                self.peak = Some(stat);
                if self.start_trial(Dir::Up, stat) {
                    return;
                }
                // Already at the cap: a first run looks downward instead.
                if reprobe || !self.start_trial(Dir::Down, stat) {
                    self.settle();
                }
            }
            Goal::Trial { dir, base } => {
                let m = self.margin(stat, base);
                let ratio = stat.mean / base.mean.max(1.0);
                let (accept_at, reject_at) = match dir {
                    Dir::Up => (1.0 + self.k.gain + m, 1.0 + self.k.gain - m),
                    Dir::Down => (self.k.keep + m, self.k.keep - m),
                };
                if ratio >= accept_at {
                    self.accept(dir, stat);
                } else if ratio <= reject_at {
                    self.reject(dir);
                } else {
                    // Inconclusive: measure the base again (A-B-A).
                    self.target = base.n;
                    self.state = State::Running {
                        goal: Goal::Confirm {
                            dir,
                            base,
                            trial: stat,
                        },
                        phase: Phase::warm(),
                    };
                }
            }
            Goal::Confirm { dir, base, trial } => {
                let reference = (base.mean + stat.mean) / 2.0;
                let ratio = trial.mean / reference.max(1.0);
                let ok = match dir {
                    Dir::Up => ratio >= 1.0 + self.k.gain,
                    // Stepping down must hold up beyond one standard error.
                    Dir::Down => {
                        let ref_rse = (base.rse.powi(2) + stat.rse.powi(2)).sqrt() / 2.0;
                        ratio >= self.k.keep + (trial.rse.powi(2) + ref_rse.powi(2)).sqrt()
                    }
                };
                if ok {
                    self.accept(dir, trial);
                } else {
                    self.reject(dir);
                }
            }
        }
    }

    /// Start measuring the next count in `dir` against `base`; false when
    /// there is none.
    fn start_trial(&mut self, dir: Dir, base: Stat) -> bool {
        let next = match dir {
            Dir::Up => self.target.saturating_mul(2).min(self.cap()),
            Dir::Down => (self.target / 2).max(1),
        };
        if next == self.target {
            return false;
        }
        self.target = next;
        self.state = State::Running {
            goal: Goal::Trial { dir, base },
            phase: Phase::warm(),
        };
        true
    }

    /// Steps down are judged against the best throughput seen, returning to
    /// the accepted count when inconclusive.
    fn down_base(&self) -> Option<Stat> {
        let best = self.best?;
        let peak = self.peak.unwrap_or(best);
        Some(Stat { n: best.n, ..peak })
    }

    fn accept(&mut self, dir: Dir, trial: Stat) {
        self.target = trial.n;
        self.best = Some(trial);
        if self.peak.is_none_or(|p| trial.mean > p.mean) {
            self.peak = Some(trial);
        }
        match dir {
            Dir::Up => {
                self.climbed = true;
                self.reprobe_every = self.k.reprobe;
                if !self.start_trial(Dir::Up, trial) {
                    self.settle();
                }
            }
            Dir::Down => {
                let base = self.down_base().unwrap_or(trial);
                if !self.start_trial(Dir::Down, base) {
                    self.settle();
                }
            }
        }
    }

    fn reject(&mut self, dir: Dir) {
        if let Some(b) = self.best {
            self.target = b.n;
        }
        match dir {
            Dir::Up if !self.climbed && !self.settled_once => {
                // The very first doubling did not pay: look downward.
                match self.down_base() {
                    Some(base) if self.start_trial(Dir::Down, base) => {}
                    _ => self.settle(),
                }
            }
            Dir::Up => {
                if self.settled_once {
                    // A re-probe that did not pay: wait longer next time.
                    self.reprobe_every = (self.reprobe_every * 2).min(REPROBE_MAX);
                }
                self.settle();
            }
            Dir::Down => self.settle(),
        }
    }

    fn settle(&mut self) {
        self.state = State::Settled { since: None };
        self.settled_once = true;
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
        if self.best.is_none_or(|b| b.n > self.ceiling) {
            self.best = Some(Stat {
                n: self.ceiling,
                mean: self.best.map_or(0.0, |b| b.mean),
                rse: 1.0,
            });
        }
        if !self.fixed {
            self.settle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the tuner against an ideal throughput model `f(n)` in bytes/s
    /// (no noise, no ramp-up, no TTFB): the decisions are immediate.
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
                ttfb: None,
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
            ttfb: None,
        });
        assert!(t.settled());
        assert_eq!(t.target(), 4);
    }

    // ------------------------------------------------- a real-ish link

    /// A link model: `rate(n, t)` bytes/s at `n` connections and time `t`;
    /// new connections stream after `ttfb` and ramp up with time constant
    /// `ramp` (slow start); every 50 ms tick is scaled by uniform noise of
    /// ±`noise` and a slow ±`drift` swing (period 40 s).
    struct Link {
        rate: Box<dyn Fn(u16, f64) -> f64>,
        ttfb: f64,
        ramp: f64,
        noise: f64,
        drift: f64,
    }

    struct Run {
        /// Distinct targets in order.
        path: Vec<u16>,
        lowest: u16,
        last: u16,
        /// Seconds until the first change of target.
        first_change: Option<f64>,
    }

    fn simulate(t: &mut Tuner, link: &Link, secs: f64, seed: u64) -> Run {
        let mut rng = fastrand::Rng::with_seed(seed);
        let step = 0.05;
        let t0 = Instant::now();
        let mut bytes = 0f64;
        let mut path = vec![t.target()];
        let mut lowest = t.target();
        let mut first_change = None;
        // The count currently delivering, and the ramp of an increase.
        let mut prev = t.target();
        let mut before = prev;
        let mut changed_at = 0.0;
        let mut from_rate = (link.rate)(prev, 0.0);
        let mut i = 0u64;
        loop {
            i += 1;
            let now_s = i as f64 * step;
            if now_s > secs {
                break;
            }
            let n = t.target();
            if n != prev {
                from_rate = (link.rate)(prev, now_s);
                before = prev;
                prev = n;
                changed_at = now_s;
            }
            let since = now_s - changed_at;
            let full = (link.rate)(n, now_s);
            let ideal = if full > from_rate {
                if since < link.ttfb {
                    from_rate
                } else {
                    from_rate
                        + (full - from_rate) * (1.0 - (-(since - link.ttfb) / link.ramp).exp())
                }
            } else {
                full
            };
            let drift = 1.0 + link.drift * (std::f64::consts::TAU * now_s / 40.0).sin();
            let noise = 1.0 + link.noise * (2.0 * rng.f64() - 1.0);
            bytes += ideal * drift * noise * step;
            // New connections stream only after their first byte.
            let streaming = if since < link.ttfb { n.min(before) } else { n };
            t.tick(Sample {
                now: t0 + Duration::from_secs_f64(now_s),
                bytes: bytes as u64,
                streaming,
                work_left: u64::MAX / 2,
                limited: false,
                ttfb: Some(link.ttfb),
            });
            if t.target() != *path.last().unwrap() {
                path.push(t.target());
                first_change.get_or_insert(now_s);
            }
            lowest = lowest.min(t.target());
        }
        Run {
            last: t.target(),
            path,
            lowest,
            first_change,
        }
    }

    /// The reported link: 3.8 MB/s per connection, ±30 % noise, a slow
    /// ±25 % drift, 300 ms to the first byte (TCP + TLS over Wi-Fi) and a
    /// slow start with a 0.8 s time constant.
    fn wifi(rate: impl Fn(u16, f64) -> f64 + 'static) -> Link {
        Link {
            rate: Box::new(rate),
            ttfb: 0.3,
            ramp: 0.8,
            noise: 0.3,
            drift: 0.25,
        }
    }

    #[test]
    fn windows_scale_with_time_to_first_byte() {
        // 300 ms TTFB: windows of at least 16 × 0.3 = 4.8 s, capped at
        // 4 s, after a warm-up of at least 4 × 0.3 s.
        let mut t = tuner(16);
        let r = simulate(&mut t, &wifi(|n, _| n as f64 * 3.8e6), 60.0, 1);
        assert!(r.first_change.unwrap() >= 4.0 + 1.2, "{:?}", r.first_change);
        // Loopback-like (no TTFB, no noise): the first step within a second.
        let mut t = tuner(16);
        let mut now = Instant::now();
        let mut bytes = 0u64;
        let mut first = None;
        for i in 1..100 {
            now += Duration::from_millis(50);
            bytes += 2_000_000 * 4 / 20;
            t.tick(Sample {
                now,
                bytes,
                streaming: t.target(),
                work_left: u64::MAX / 2,
                limited: false,
                ttfb: Some(0.0005),
            });
            if t.target() != 4 {
                first = Some(i as f64 * 0.05);
                break;
            }
        }
        assert!(first.unwrap() <= 1.0, "{first:?}");
    }

    #[test]
    fn a_noisy_per_connection_link_climbs_and_never_drops_below_the_start() {
        let mut at_cap = 0;
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(&mut t, &wifi(|n, _| n as f64 * 3.8e6), 120.0, seed);
            assert!(r.lowest >= 4, "seed {seed}: {:?}", r.path);
            assert!(r.last >= 8, "seed {seed}: {:?}", r.path);
            at_cap += usize::from(r.last == 16);
        }
        assert!(at_cap >= 27, "{at_cap}/30 reached 16");
    }

    #[test]
    fn a_noisy_saturated_link_stays_high() {
        // The live case: about 8 connections fill a ~30 MB/s link. Noise
        // must never walk the count down to 1–2.
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(
                &mut t,
                &wifi(|n, _| (n as f64 * 3.8e6).min(30e6)),
                180.0,
                seed,
            );
            assert!(r.lowest >= 4, "seed {seed}: {:?}", r.path);
            assert!(r.last >= 8, "seed {seed}: {:?}", r.path);
        }
    }

    /// The old behaviour (200 ms windows, decisions on one window) on the
    /// same link: noise walks the count down. Kept to show the tests above
    /// would catch it.
    #[test]
    fn single_short_windows_wander_on_the_same_link() {
        let naive = Tuning {
            tune_window: Duration::from_millis(200),
            tune_window_max: Duration::from_millis(200),
            tune_rtt_windows: 0.0,
            tune_sample: Duration::from_millis(50),
            tune_target_rse: 1.0,
            tune_warm_rtts: 0.0,
            tune_warm_rise: f64::INFINITY,
            tune_z: 0.0,
            tune_reprobe: Duration::from_secs(1_000_000),
            ..Tuning::default()
        };
        let mut low = 0;
        for seed in 0..30 {
            let mut t = Tuner::new(&naive, 16, None, None);
            let r = simulate(
                &mut t,
                &wifi(|n, _| (n as f64 * 3.8e6).min(30e6)),
                180.0,
                seed,
            );
            low += usize::from(r.last <= 4);
        }
        assert!(low >= 5, "only {low}/30 wandered low");
    }

    #[test]
    #[ignore = "prints outcome statistics for the README"]
    fn outcome_statistics() {
        type Model = fn(u16, f64) -> f64;
        let models: [(&str, Model); 3] = [
            ("per-connection 3.8 MB/s", |n, _| n as f64 * 3.8e6),
            ("saturating 30 MB/s", |n, _| (n as f64 * 3.8e6).min(30e6)),
            ("shared 20 MB/s", |_, _| 20e6),
        ];
        for (name, f) in models {
            for (label, tuning) in [
                ("robust", Tuning::default()),
                (
                    "single 200 ms windows",
                    Tuning {
                        tune_window: Duration::from_millis(200),
                        tune_window_max: Duration::from_millis(200),
                        tune_rtt_windows: 0.0,
                        tune_sample: Duration::from_millis(50),
                        tune_target_rse: 1.0,
                        tune_warm_rtts: 0.0,
                        tune_warm_rise: f64::INFINITY,
                        tune_z: 0.0,
                        tune_reprobe: Duration::from_secs(1_000_000),
                        ..Tuning::default()
                    },
                ),
            ] {
                let mut finals = std::collections::BTreeMap::new();
                for seed in 0..100 {
                    let mut t = Tuner::new(&tuning, 16, None, None);
                    let r = simulate(&mut t, &wifi(f), 180.0, seed);
                    *finals.entry(r.last).or_insert(0) += 1;
                }
                println!("{name} | {label} | {finals:?}");
            }
        }
    }

    #[test]
    fn a_noisy_global_limit_does_not_climb() {
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(&mut t, &wifi(|_, _| 20e6), 120.0, seed);
            assert!(r.last <= 8, "seed {seed}: {:?}", r.path);
        }
    }

    #[test]
    fn a_clean_link_where_connections_do_not_help_settles_on_one_or_two() {
        // Loopback: steady, fast, connections add nothing.
        for seed in 0..10 {
            let mut t = tuner(16);
            let link = Link {
                rate: Box::new(|_, _| 900e6),
                ttfb: 0.001,
                ramp: 0.01,
                noise: 0.02,
                drift: 0.0,
            };
            let r = simulate(&mut t, &link, 10.0, seed);
            assert!(r.last <= 2, "seed {seed}: {:?}", r.path);
        }
    }

    #[test]
    fn a_reprobe_follows_a_link_that_opens_up() {
        // Shared bottleneck for the first minute, then per-connection.
        for seed in 0..10 {
            let mut t = tuner(16);
            let link = Link {
                noise: 0.1,
                ..wifi(|n, s| if s < 60.0 { 20e6 } else { n as f64 * 3.8e6 })
            };
            let r = simulate(&mut t, &link, 240.0, seed);
            assert!(r.last >= 8, "seed {seed}: {:?}", r.path);
        }
    }
}
