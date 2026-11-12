//! Learned connection count (Speed Doctrine #3), robust to real networks.
//!
//! A download starts at its host's `HostProfile::best_connections`, or at
//! `Tuning::initial_connections` (8), and hill-climbs:
//!
//! 1. Measure throughput at the current count `n` (the base).
//! 2. Try `2n` (up to the cap). Keep it if the gain is clearly at least
//!    `climb_gain` (4 %); while doubling pays, keep doubling.
//! 3. If the very first doubling added (about) nothing, more connections
//!    don't help: try `n/2`, `n/4`, … while each still delivers `keep_ratio`
//!    (97 %) of the count above it. Fewer connections for the same speed
//!    cost less CPU and are politer. After a small but real gain the count
//!    just stays (fewer would be slower).
//! 4. Settle. Every `tune_reprobe` (doubling while nothing changes, up to
//!    5 min) measure the base again and try one step, alternately up and
//!    down: conditions change during a long download, and a step that
//!    noise faked is taken back.
//!
//! What makes it robust on a real link, where a new connection needs
//! several round trips of slow start and throughput swings ±30 % from one
//! second to the next:
//!
//! - **Windows sized from the run.** A window lasts at least
//!   `max(tune_window, tune_rtt_windows × median TTFB)` (≈ 1–4 s on the
//!   internet, 200 ms on loopback) and is extended, up to `tune_window_max`,
//!   until the relative standard error (RSE) of its `tune_sample`-long
//!   throughput samples is at most `tune_target_rse`.
//! - **Warm-up.** Measuring starts only once every connection streams, at
//!   least `max(tune_window / 2, 4 × TTFB)` later, and not while throughput
//!   is still rising (slow start), for at most `tune_window_max`.
//! - **Paired, alternating comparisons.** After the first base and trial
//!   windows, they alternate (B T B T B …, up to `tune_rounds` trials).
//!   Every trial window is compared with the mean of the base windows on
//!   either side of it, which cancels drift; the error of the mean ratio is
//!   the larger of the windows' own and the spread of the ratios. A climb
//!   is taken when its lower confidence bound (`tune_z` = 2 standard
//!   errors) clears `climb_gain` after at least two such pairs, or at once
//!   for an obvious gain (≥ 25 % beyond the noise); an unclear climb after
//!   the last round is not taken. A step down needs alternating windows
//!   too and is taken when it clearly holds `keep_ratio`, or, after the last
//!   round, when its estimate does.
//! - **Fast first climb.** A short download ends before careful windows
//!   decide anything, yet the commonest reason more connections pay — a
//!   per-connection limit (a server throttle, a long thin TCP path) — is
//!   also the easiest to see: doubling the count (almost) doubles the
//!   throughput. So a download's first climb measures short windows
//!   (`tune_fast_window`, at least 4 TTFBs, warm-up half that) and takes a
//!   doubling from one trial window when it delivers at least
//!   `tune_fast_scaling` (half) of linear scaling beyond 2 standard errors,
//!   then tries the next doubling at once. The first doubling that scales
//!   less ends the fast climb; its comparison continues as above, from the
//!   windows already measured.
//!
//! - **Back to one.** A fresh download's first connection measured its rate
//!   alone before the others started (`solo_rate`). When the first window
//!   at the starting count is under `1 + solo_collapse_gain` times that,
//!   the others add nothing, and the count drops to one at once, judged
//!   from one fast window against that first window: kept within
//!   `solo_collapse_slack` of `keep_ratio`; far short (under half) goes back
//!   and climbs as usual; anything between is settled in alternating
//!   windows (the link may have drifted meanwhile).
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

/// A gain this large (beyond the noise margin) is taken from a single
/// base/trial pair of windows; smaller ones need alternating windows,
/// because drift between two windows can fake them.
const OBVIOUS_GAIN: f64 = 0.25;

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

#[derive(Debug, Clone, PartialEq)]
enum Goal {
    /// Measure the current count (first, or before a re-probe).
    Base { reprobe: Option<Dir> },
    /// Measure the current count against `base`.
    Trial { dir: Dir, base: Stat },
    /// Base and trial windows alternate until the comparison is clear.
    Alternate {
        dir: Dir,
        base_n: u16,
        trial_n: u16,
        bases: Vec<Stat>,
        trials: Vec<Stat>,
    },
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
    fast_window: Duration,
    fast_sample: Duration,
    fast_scaling: f64,
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
    rounds: u32,
    reprobe: Duration,
    collapse_gain: f64,
    collapse_slack: f64,
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
    climbed: bool,
    /// The first climb, while every doubling scales (almost) linearly:
    /// short windows, one trial window per doubling.
    fast: bool,
    /// A jump from the first connection's solo rate is being measured
    /// (from this count).
    jump_from: Option<u16>,
    /// What a fresh download's first connection delivered alone (bytes/s),
    /// until the first window at the starting count is compared with it.
    solo: Option<f64>,
    /// The drop to one connection that `solo` suggested is being measured.
    collapsing: bool,
    throttled: bool,
    /// Has settled at least once: later climbs are re-probes.
    settled_once: bool,
    reprobe_every: Duration,
    /// Re-probes alternate: up (conditions improved?), then down (a climb
    /// that noise faked, or conditions worsened?).
    next_reprobe: Dir,
}

/// Mean of several windows and the relative standard error of that mean,
/// from the windows' own samples.
fn pool(v: &[Stat]) -> (f64, f64) {
    let k = v.len().max(1) as f64;
    let mean = v.iter().map(|s| s.mean).sum::<f64>() / k;
    let within = v.iter().map(|s| s.rse.powi(2)).sum::<f64>().sqrt() / k;
    (mean, within)
}

/// Trial over base, from alternating windows `B0 T1 B1 T2 B2 …`: every
/// trial window is compared with the mean of the base windows on either
/// side of it, so a drift of the link (linear over the three windows)
/// cancels. Returns the mean of those ratios and its relative standard
/// error: from the spread of the ratios once there are two or more, never
/// less than what the windows' own samples say. Right after the first
/// trial window (`B0 T1`) it is the plain ratio.
fn paired(bases: &[Stat], trials: &[Stat]) -> (f64, f64) {
    let sq = |x: f64| x * x;
    let mut ratios = Vec::new();
    let mut within = Vec::new();
    for (i, t) in trials.iter().enumerate() {
        let (b, rb) = match (bases.get(i), bases.get(i + 1)) {
            (Some(a), Some(c)) => (
                (a.mean + c.mean) / 2.0,
                (sq(a.rse) + sq(c.rse)).sqrt() / 2.0,
            ),
            (Some(a), None) => (a.mean, a.rse),
            _ => continue,
        };
        ratios.push(t.mean / b.max(1.0));
        within.push((sq(t.rse) + sq(rb)).sqrt());
    }
    let k = ratios.len().max(1) as f64;
    let mean = ratios.iter().sum::<f64>() / k;
    let within = within.iter().map(|w| sq(*w)).sum::<f64>().sqrt() / k;
    let spread = if ratios.len() >= 2 {
        let var = ratios.iter().map(|r| sq(r - mean)).sum::<f64>() / (k - 1.0);
        var.sqrt() / k.sqrt()
    } else {
        0.0
    };
    (mean, within.max(spread))
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
                fast_window: t.tune_fast_window,
                fast_sample: t.tune_fast_sample,
                fast_scaling: t.tune_fast_scaling,
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
                rounds: t.tune_rounds.max(1),
                reprobe: t.tune_reprobe,
                collapse_gain: t.solo_collapse_gain,
                collapse_slack: t.solo_collapse_slack,
            },
            state: if fixed {
                State::Settled { since: None }
            } else {
                State::Running {
                    goal: Goal::Base { reprobe: None },
                    phase: Phase::warm(),
                }
            },
            best: None,
            climbed: false,
            fast: !fixed && t.tune_fast_scaling.is_finite(),
            jump_from: None,
            solo: None,
            collapsing: false,
            throttled: false,
            settled_once: false,
            reprobe_every: t.tune_reprobe,
            next_reprobe: Dir::Up,
        };
        if !fixed {
            tuner.target = tuner.target.min(ceiling);
        }
        tuner
    }

    /// A fresh download's first connection, alone, delivered far less than
    /// a round trip's worth per round trip (`rate`, bytes/s): the server
    /// limits it, probably every connection alike. Jump to `n` (up to the
    /// cap) at once, as a trial against that solo rate: kept when `n`
    /// deliver at least `max(2, n / 4)` times as much (a per-connection
    /// limit gives about n×, a shared one about 1×); otherwise judged
    /// carefully from there, fewer first.
    pub(crate) fn limited_per_connection(&mut self, n: u16, rate: f64) {
        if self.fixed || !matches!(self.state, State::Running { .. }) || !rate.is_finite() {
            return;
        }
        let n = n.min(self.cap()).max(1);
        if n <= self.target || rate <= 0.0 {
            return;
        }
        self.solo = None;
        self.jump_from = Some(self.target);
        self.target = n;
        self.state = State::Running {
            goal: Goal::Trial {
                dir: Dir::Up,
                base: Stat {
                    n: 1,
                    mean: rate,
                    rse: 1.0,
                },
            },
            phase: Phase::warm(),
        };
    }

    /// A fresh download's first connection, alone, delivered `rate`
    /// bytes/s (past its first burst) before the others started. If the
    /// first window at the starting count is not clearly faster, they add
    /// nothing, and the download drops to one connection (see `decide`).
    pub(crate) fn solo_rate(&mut self, rate: f64) {
        let first_base = matches!(
            self.state,
            State::Running {
                goal: Goal::Base { reprobe: None },
                ..
            }
        );
        if !self.fixed && first_base && self.best.is_none() && rate.is_finite() && rate > 0.0 {
            self.solo = Some(rate);
        }
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
        let fast = self.fast;
        let (min_len, warm_min, period) = if fast {
            let len = k
                .fast_window
                .max(scaled(s.ttfb, k.warm_rtts))
                .min(k.window_max);
            (len, (len / 2).max(k.fast_sample), k.fast_sample)
        } else {
            (
                k.window
                    .max(scaled(s.ttfb, k.rtt_windows))
                    .min(k.window_max),
                (k.window / 2)
                    .max(scaled(s.ttfb, k.warm_rtts))
                    .min(k.window_max),
                k.sample,
            )
        };
        let target = self.target;
        let can_up = self.target < self.cap() && !self.throttled;
        let can_down = self.target > 1;
        let reprobe_every = self.reprobe_every;
        let next_reprobe = self.next_reprobe;
        let (goal, stat) = match &mut self.state {
            State::Settled { since } => {
                let since = *since.get_or_insert(s.now);
                if s.now.duration_since(since) < reprobe_every {
                    return;
                }
                let dir = match next_reprobe {
                    Dir::Up if can_up => Dir::Up,
                    _ if can_down => Dir::Down,
                    _ if can_up => Dir::Up,
                    _ => return,
                };
                self.next_reprobe = match dir {
                    Dir::Up => Dir::Down,
                    Dir::Down => Dir::Up,
                };
                self.state = State::Running {
                    goal: Goal::Base { reprobe: Some(dir) },
                    phase: Phase::warm(),
                };
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
                    take_sample(l, rates, s.now, s.bytes, period);
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
                    take_sample(last, rates, s.now, s.bytes, period);
                    let len = s.now.duration_since(start.0);
                    let r = rse(rates);
                    // A fast window needs no target error: its comparison
                    // allows for it.
                    let precise = fast || r <= k.target_rse || len >= k.window_max;
                    if len < min_len || rates.len() < 2 || !precise {
                        return;
                    }
                    let stat = Stat {
                        n: target,
                        mean: s.bytes.saturating_sub(start.1) as f64 / len.as_secs_f64(),
                        rse: r.min(1.0),
                    };
                    (goal.clone(), stat)
                }
            },
        };
        // Too little left to measure another step reliably (a fast climb
        // decides from one trial window, a careful one needs alternation).
        let next_step = (min_len + warm_min).as_secs_f64() * if fast { 1.0 } else { 2.0 };
        if (s.work_left as f64) < stat.mean * next_step {
            if self.best.is_none() {
                self.best = Some(stat);
            }
            self.settle();
            return;
        }
        self.decide(goal, stat);
    }

    fn decide(&mut self, goal: Goal, stat: Stat) {
        match goal {
            Goal::Base { reprobe } => {
                self.best = Some(stat);
                if let Some(solo) = self.solo
                    && reprobe.is_none()
                {
                    tracing::debug!(
                        n = stat.n,
                        rate = stat.mean,
                        solo,
                        "first window against the first connection alone"
                    );
                }
                if let Some(solo) = self.solo.take()
                    && reprobe.is_none()
                    && stat.n > 1
                    && stat.mean < solo * (1.0 + self.k.collapse_gain)
                {
                    // All of them deliver about what the first connection
                    // did alone: try it alone again, from one fast window.
                    self.collapsing = true;
                    self.target = 1;
                    self.state = State::Running {
                        goal: Goal::Trial {
                            dir: Dir::Down,
                            base: stat,
                        },
                        phase: Phase::warm(),
                    };
                    return;
                }
                let started = match reprobe {
                    Some(dir) => self.start_trial(dir, stat),
                    // First: up, or (already at the cap) down.
                    None => self.start_trial(Dir::Up, stat) || self.start_trial(Dir::Down, stat),
                };
                if !started {
                    self.settle();
                }
            }
            Goal::Trial { dir, base } => {
                if std::mem::take(&mut self.collapsing) {
                    let ratio = stat.mean / base.mean.max(1.0);
                    tracing::debug!(
                        ratio,
                        rse = stat.rse,
                        "one connection against the first window"
                    );
                    let se = ratio * (stat.rse.powi(2) + base.rse.powi(2)).sqrt();
                    let slack = (self.k.z * se).min(self.k.collapse_slack);
                    if ratio + slack >= self.k.keep {
                        // One connection does as well: stay there.
                        self.accept(Dir::Down, stat);
                    } else if ratio < 0.5 {
                        // Far short: the solo look flattered it (a limit
                        // per connection after all). Back to the starting
                        // count, and climb from there as usual.
                        self.target = base.n;
                        self.best = Some(base);
                        if !self.start_trial(Dir::Up, base) {
                            self.settle();
                        }
                    } else {
                        // Somewhat short, measured later than the others:
                        // the link may have drifted (a disk that starts
                        // throttling writes). Settle it the careful way,
                        // with the starting count measured again around it.
                        self.fast = false;
                        self.alternate(Dir::Down, base.n, 1, vec![base], vec![stat]);
                    }
                    return;
                }
                if self.jump_from.take().is_some() {
                    // The jump from the first connection's solo rate.
                    let linear = f64::from(stat.n) / f64::from(base.n.max(1));
                    if stat.mean >= base.mean * (linear / 4.0).max(2.0) {
                        self.accept(dir, stat);
                    } else {
                        // A shared limit: fewer connections would not be
                        // slower, but cutting them now (their ranges handed
                        // on mid-flight) costs more than it saves on a
                        // short download. Judge carefully from here, fewer
                        // first; a long download comes down.
                        self.fast = false;
                        self.best = Some(stat);
                        self.state = State::Running {
                            goal: Goal::Base {
                                reprobe: Some(Dir::Down),
                            },
                            phase: Phase::warm(),
                        };
                    }
                    return;
                }
                if self.fast && dir == Dir::Up && self.scales(base, stat) {
                    self.accept(dir, stat);
                    return;
                }
                self.fast = false;
                self.weigh(dir, base.n, stat.n, vec![base], vec![stat]);
            }
            Goal::Alternate {
                dir,
                base_n,
                trial_n,
                mut bases,
                mut trials,
            } => {
                if stat.n == trial_n {
                    // A trial window: measure the base again next.
                    trials.push(stat);
                    self.alternate(dir, base_n, trial_n, bases, trials);
                } else {
                    bases.push(stat);
                    self.weigh(dir, base_n, trial_n, bases, trials);
                }
            }
        }
    }

    /// Continue alternating: the base after a trial window, the trial after
    /// a base window.
    fn alternate(
        &mut self,
        dir: Dir,
        base_n: u16,
        trial_n: u16,
        bases: Vec<Stat>,
        trials: Vec<Stat>,
    ) {
        self.target = if trials.len() >= bases.len() {
            base_n
        } else {
            trial_n
        };
        self.state = State::Running {
            goal: Goal::Alternate {
                dir,
                base_n,
                trial_n,
                bases,
                trials,
            },
            phase: Phase::warm(),
        };
    }

    /// Judge a trial against its base from every window measured so far
    /// (base and trial windows alternate, so drift cancels in the means):
    /// decide when the pooled estimate clears the bar by `tune_z` standard
    /// errors, measure another round when not, and after `tune_rounds`
    /// decide on the estimate itself.
    fn weigh(&mut self, dir: Dir, base_n: u16, trial_n: u16, bases: Vec<Stat>, trials: Vec<Stat>) {
        let (mb, rb) = pool(&bases);
        let (mt, rt) = pool(&trials);
        let (ratio, se) = paired(&bases, &trials);
        let z = self.k.z;
        let last_round = trials.len() as u32 >= self.k.rounds;
        // From a single base/trial pair, drift between the two windows can
        // fake a gain (or an equal speed): only an obvious gain is taken
        // without alternating windows. Rejections are cheap and may be
        // immediate.
        let alternated = bases.len() >= 2;
        // A climb on a small gain needs two paired comparisons (B T B T B),
        // whose spread shows how much drift is left after pairing.
        let paired_twice = bases.len() >= 3;
        let verdict = match dir {
            Dir::Up => {
                let gain = ratio - 1.0;
                let bar = if paired_twice {
                    self.k.gain
                } else {
                    self.k.gain.max(OBVIOUS_GAIN)
                };
                if gain - z * se >= bar {
                    Some(true)
                } else if gain + z * se < self.k.gain {
                    Some(false)
                } else if last_round {
                    // Still unclear: stay. A climb needs its lower
                    // confidence bound above the bar.
                    Some(false)
                } else {
                    None
                }
            }
            Dir::Down => {
                if alternated && ratio - z * se >= self.k.keep {
                    Some(true)
                } else if ratio + z * se < self.k.keep {
                    Some(false)
                } else if last_round {
                    // Still unclear after alternating: the estimate decides
                    // (a link where the counts are equal must be able to
                    // come down; one that loses 7 % per halving rarely
                    // shows 97 % in pooled windows).
                    Some(ratio >= self.k.keep)
                } else {
                    None
                }
            }
        };
        let trial = Stat {
            n: trial_n,
            mean: mt,
            rse: rt,
        };
        match verdict {
            Some(true) => self.accept(dir, trial),
            Some(false) => self.reject(
                dir,
                ratio - 1.0,
                Stat {
                    n: base_n,
                    mean: mb,
                    rse: rb,
                },
            ),
            None => self.alternate(dir, base_n, trial_n, bases, trials),
        }
    }

    /// A fast climb's test: `trial` delivers, beyond `tune_z` standard
    /// errors, at least `tune_fast_scaling` of linear scaling over `base`.
    fn scales(&self, base: Stat, trial: Stat) -> bool {
        let ratio = trial.mean / base.mean.max(1.0);
        let se = ratio * (trial.rse.powi(2) + base.rse.powi(2)).sqrt();
        let linear = f64::from(trial.n) / f64::from(base.n.max(1));
        ratio - self.k.z * se >= 1.0 + self.k.fast_scaling * (linear - 1.0)
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
        if dir == Dir::Down {
            // Steps down are always judged carefully.
            self.fast = false;
        }
        self.target = next;
        self.state = State::Running {
            goal: Goal::Trial { dir, base },
            phase: Phase::warm(),
        };
        true
    }

    fn accept(&mut self, dir: Dir, trial: Stat) {
        // Something changed: re-probe at the base interval again.
        self.reprobe_every = self.k.reprobe;
        self.target = trial.n;
        self.best = Some(trial);
        match dir {
            Dir::Up => {
                self.climbed = true;
                if !self.start_trial(Dir::Up, trial) {
                    self.settle();
                }
            }
            Dir::Down => {
                // Each step down is judged against the count just accepted,
                // measured in alternating windows (drift cancels there; a
                // past peak would be inflated by it).
                if !self.start_trial(Dir::Down, trial) {
                    self.settle();
                }
            }
        }
    }

    /// `gain`: the trial's measured throughput change; `base`: the base
    /// count as measured in this comparison.
    fn reject(&mut self, dir: Dir, gain: f64, base: Stat) {
        if let Some(b) = self.best {
            self.target = b.n;
        }
        match dir {
            Dir::Up if !self.climbed && !self.settled_once && gain <= self.k.gain / 2.0 => {
                // The very first doubling added (about) nothing: fewer
                // connections may do as well. After a small but real gain
                // they would not, so then just stay.
                if !self.start_trial(Dir::Down, base) {
                    self.settle();
                }
            }
            Dir::Up | Dir::Down => {
                if self.settled_once {
                    // A re-probe that did not pay: wait longer next time.
                    self.reprobe_every = (self.reprobe_every * 2).min(REPROBE_MAX);
                }
                self.settle();
            }
        }
    }

    fn settle(&mut self) {
        self.state = State::Settled { since: None };
        self.settled_once = true;
        self.fast = false;
        self.jump_from = None;
        self.solo = None;
        self.collapsing = false;
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

    /// Paths below are written for a start at 4.
    fn tuner4(max: u16) -> Tuner {
        let t = Tuning {
            initial_connections: 4,
            ..Tuning::default()
        };
        Tuner::new(&t, max, None, None)
    }

    #[test]
    fn climbs_while_connections_pay() {
        let mut t = tuner4(16);
        let path = run(&mut t, |n| n as f64 * 2e6); // per-connection limit
        assert_eq!(path, vec![4, 8, 16]);
        assert!(t.settled());
        assert_eq!(t.best(), 16);
    }

    #[test]
    fn settles_low_when_connections_do_not_help() {
        let mut t = tuner4(16);
        let path = run(&mut t, |_| 20e6); // global limit
        // Every step down is confirmed in alternating windows (2, 4, then
        // 1, 2): 8 adds nothing, 2 and 1 lose nothing.
        assert_eq!(path, vec![4, 8, 2, 4, 1, 2, 1]);
        assert_eq!((t.target(), t.best()), (1, 1));
    }

    #[test]
    fn stops_where_fewer_connections_lose_speed() {
        let mut t = tuner4(16);
        // Two connections saturate the link; one gets 60 %.
        let path = run(&mut t, |n| if n >= 2 { 100e6 } else { 60e6 });
        assert_eq!(path, vec![4, 8, 2, 4, 1, 2]);
        assert_eq!(t.target(), 2);
    }

    #[test]
    fn climbs_partway_then_stops() {
        let mut t = tuner4(64);
        // 8 → 16 adds 2.5 %, under the 4 % bar.
        let path = run(&mut t, |n| f64::from(n).min(8.2) * 1e6);
        assert_eq!(path, vec![4, 8, 16, 8]);
        assert_eq!(t.target(), 8);
    }

    #[test]
    fn throttling_sets_a_ceiling() {
        let mut t = tuner4(16);
        t.on_throttle(2, false);
        assert_eq!((t.target(), t.max_ok()), (2, Some(2)));
        assert!(t.settled());
        let mut t = tuner4(16);
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
    fn the_first_connection_alone_was_as_fast_so_it_goes_on_alone() {
        // A link the first connection filled on its own: eight deliver no
        // more (or a little less), so it is tried alone again and kept.
        for one in [100e6, 98e6] {
            let mut t = tuner(16);
            t.solo_rate(100e6);
            let path = run(&mut t, |n| if n == 1 { one } else { 100e6 });
            assert_eq!(path, vec![8, 1], "one connection at {one}");
            assert!(t.settled());
            assert_eq!(t.best(), 1);
        }
    }

    #[test]
    fn a_solo_rate_that_flattered_the_first_connection_is_corrected() {
        // Every connection is limited to 2 MB/s, but the first one's short
        // look alone read 15 MB/s: eight deliver about that, so one is
        // tried alone, falls far short, and the climb goes on from eight
        // (straight to the trial of 16, against eight's window).
        let mut t = tuner(16);
        t.solo_rate(15e6);
        let path = run(&mut t, |n| n as f64 * 2e6);
        assert_eq!(path, vec![8, 1, 16]);
        assert_eq!(t.best(), 16);
        // One that loses more than the noise allows is not kept either:
        // that is settled in alternating windows (it may be drift), which
        // go back to eight and stay there.
        let mut t = tuner(16);
        t.solo_rate(100e6);
        let path = run(&mut t, |n| if n == 1 { 85e6 } else { 100e6 });
        assert_eq!(path, vec![8, 1, 8]);
        assert!(t.settled());
    }

    #[test]
    fn a_drop_to_one_measured_while_the_link_slows_is_judged_again() {
        // The link slows by 3 % per window (a disk that starts throttling
        // writes): one connection, measured after the eight, looks 6 %
        // short. Measured again around it, the eight have slowed too, and
        // one connection is kept.
        let mut t = tuner(16);
        t.solo_rate(100e6);
        let mut now = Instant::now();
        let mut bytes = 0u64;
        let mut seen = vec![t.target()];
        let start = now;
        let step = Duration::from_millis(50);
        for _ in 0..200 {
            now += step;
            let windows = now.duration_since(start).as_secs_f64() / 0.15;
            let rate = 100e6 * 0.97f64.powf(windows);
            bytes += (rate * step.as_secs_f64()) as u64;
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
        assert_eq!(seen.first(), Some(&8));
        assert_eq!(t.target(), 1, "{seen:?}");
    }

    #[test]
    fn connections_that_beat_the_solo_rate_stay() {
        let mut t = tuner(16);
        t.solo_rate(2e6);
        assert_eq!(run(&mut t, |n| n as f64 * 2e6), vec![8, 16]);
        // Only a fresh download's first window is compared with it.
        let mut t = tuner(16);
        t.solo_rate(2e6);
        t.on_throttle(4, false);
        t.solo_rate(1e9);
        assert_eq!(run(&mut t, |_| 1e6), vec![4]);
    }

    #[test]
    fn a_speed_limit_freezes_learning() {
        let mut t = tuner4(16);
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
        /// Distinct targets in order (trials included).
        path: Vec<u16>,
        /// The lowest accepted count, and the accepted count at the end.
        lowest_best: u16,
        best: u16,
        /// Share of the run with more than 8 connections accepted.
        above_8: f64,
        /// Average delivered throughput, bytes/s.
        rate: f64,
        /// Seconds until the first change of target.
        first_change: Option<f64>,
    }

    fn simulate(t: &mut Tuner, link: &Link, secs: f64, seed: u64) -> Run {
        let mut rng = fastrand::Rng::with_seed(seed);
        let step = 0.05;
        let t0 = Instant::now();
        let mut bytes = 0f64;
        let mut path = vec![t.target()];
        let mut lowest_best = t.best();
        let mut ticks_above_8 = 0u64;
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
            lowest_best = lowest_best.min(t.best());
            ticks_above_8 += u64::from(t.best() > 8);
        }
        Run {
            above_8: ticks_above_8 as f64 / i.max(1) as f64,
            lowest_best,
            best: t.best(),
            rate: bytes / secs,
            path,
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
        // 300 ms TTFB: careful windows of at least 16 × 0.3 = 4.8 s, capped
        // at 4 s, after a warm-up of at least 4 × 0.3 s.
        let careful = Tuning {
            tune_fast_scaling: f64::INFINITY,
            ..Tuning::default()
        };
        let mut t = Tuner::new(&careful, 16, None, None);
        let r = simulate(&mut t, &wifi(|n, _| n as f64 * 3.8e6), 60.0, 1);
        assert!(r.first_change.unwrap() >= 4.0 + 1.2, "{:?}", r.first_change);
        // A fast climb's windows: 4 × 0.3 = 1.2 s after a warm-up of half.
        let mut t = tuner(16);
        let r = simulate(&mut t, &wifi(|n, _| n as f64 * 3.8e6), 60.0, 1);
        let first = r.first_change.unwrap();
        assert!((1.2 + 0.6..4.0).contains(&first), "{first}");
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
            assert!(r.lowest_best >= 4, "seed {seed}: {:?}", r.path);
            assert!(r.best >= 8, "seed {seed}: {:?}", r.path);
            at_cap += usize::from(r.best == 16);
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
            assert!(r.lowest_best >= 4, "seed {seed}: {:?}", r.path);
            assert!(r.best >= 8, "seed {seed}: {:?}", r.path);
        }
    }

    /// The old behaviour (200 ms windows, decisions on one window) on the
    /// same link: noise walks the count down. Kept to show the tests above
    /// would catch it.
    #[test]
    fn single_short_windows_wander_on_the_same_link() {
        let naive = Tuning {
            climb_gain: 0.15,
            keep_ratio: 0.93,
            initial_connections: 4,
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
            low += usize::from(r.best <= 4);
        }
        assert!(low >= 5, "only {low}/30 wandered low");
    }

    /// +8 % per doubling up to 16 connections, from 20 MB/s at one.
    fn diminishing(n: u16, _: f64) -> f64 {
        20e6 * 1.08f64.powf(f64::from(n.min(16)).log2())
    }

    #[test]
    #[ignore = "prints outcome statistics for the README"]
    fn outcome_statistics() {
        type Model = fn(u16, f64) -> f64;
        let models: [(&str, Model); 4] = [
            ("per-connection 3.8 MB/s", |n, _| n as f64 * 3.8e6),
            ("saturating 30 MB/s", |n, _| (n as f64 * 3.8e6).min(30e6)),
            ("+8 % per doubling to 16", diminishing),
            ("shared 20 MB/s", |_, _| 20e6),
        ];
        let single = Tuning {
            climb_gain: 0.15,
            keep_ratio: 0.93,
            initial_connections: 4,
            tune_window: Duration::from_millis(200),
            tune_window_max: Duration::from_millis(200),
            tune_rtt_windows: 0.0,
            tune_sample: Duration::from_millis(50),
            tune_target_rse: 1.0,
            tune_warm_rtts: 0.0,
            tune_warm_rise: f64::INFINITY,
            tune_z: 0.0,
            tune_rounds: 1,
            tune_reprobe: Duration::from_secs(1_000_000),
            ..Tuning::default()
        };
        let configs = [
            ("single 200 ms windows, 15 %, start 4", single),
            (
                "pooled, 4 %, start 4",
                Tuning {
                    initial_connections: 4,
                    ..Tuning::default()
                },
            ),
            (
                "pooled, 4 %, start 8, no fast climb",
                Tuning {
                    tune_fast_scaling: f64::INFINITY,
                    ..Tuning::default()
                },
            ),
            (
                "pooled, 4 %, start 8, fast first climb (default)",
                Tuning::default(),
            ),
        ];
        for (name, f) in models {
            for (label, tuning) in &configs {
                let mut finals = std::collections::BTreeMap::new();
                let mut rate = 0.0;
                for seed in 0..100 {
                    let mut t = Tuner::new(tuning, 16, None, None);
                    let r = simulate(&mut t, &wifi(f), 180.0, seed);
                    *finals.entry(r.best).or_insert(0) += 1;
                    rate += r.rate / 100.0;
                }
                println!("{name} | {label} | {:.1} MB/s | {finals:?}", rate / 1e6);
            }
        }
        for (label, tuning) in &configs {
            let mut finals = std::collections::BTreeMap::new();
            for seed in 0..20 {
                let mut t = Tuner::new(tuning, 16, None, None);
                let link = Link {
                    rate: Box::new(|_, _| 900e6),
                    ttfb: 0.001,
                    ramp: 0.01,
                    noise: 0.02,
                    drift: 0.0,
                };
                let r = simulate(&mut t, &link, 10.0, seed);
                *finals.entry(r.best).or_insert(0) += 1;
            }
            println!("clean loopback | {label} | {finals:?}");
        }
    }

    #[test]
    fn diminishing_returns_still_climb() {
        // +8 % per doubling (the live link) is worth taking.
        let mut at_16 = 0;
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(&mut t, &wifi(diminishing), 180.0, seed);
            assert!(r.lowest_best >= 4, "seed {seed}: {:?}", r.path);
            assert!(r.best >= 8, "seed {seed}: {:?}", r.path);
            at_16 += usize::from(r.best == 16);
        }
        assert!(at_16 >= 20, "{at_16}/30 reached 16");
    }

    #[test]
    fn a_noisy_shared_link_comes_down_over_time() {
        // Connections add nothing: starting at 8, the count must not climb
        // and must come down once alternating windows show it.
        let mut low = 0;
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(&mut t, &wifi(|_, _| 20e6), 600.0, seed);
            assert!(r.best <= 8, "seed {seed}: {:?}", r.path);
            low += usize::from(r.best <= 4);
        }
        assert!(low >= 18, "only {low}/30 came down to 4 or fewer");
    }

    #[test]
    fn a_noisy_global_limit_does_not_climb() {
        // Drift can still fake a gain now and then; down re-probes take
        // such a step back. Little time above the start, and none at the
        // end of three minutes.
        let mut share = 0.0;
        for seed in 0..30 {
            let mut t = tuner(16);
            let r = simulate(&mut t, &wifi(|_, _| 20e6), 180.0, seed);
            assert!(r.best <= 8, "seed {seed}: {:?}", r.path);
            share += r.above_8 / 30.0;
        }
        assert!(share <= 0.1, "{:.0} % of the time above 8", share * 100.0);
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
            assert!(r.best <= 2, "seed {seed}: {:?}", r.path);
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
            assert!(r.best >= 8, "seed {seed}: {:?}", r.path);
        }
    }
}
