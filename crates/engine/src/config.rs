//! Construction-time configuration and the scheduler's tuning knobs.

use std::sync::Arc;
use std::time::Duration;

use whoosh_proto::Settings;

use crate::clock::{Clock, SystemClock};
use crate::power::{PowerControl, SystemPower};

const MIB: u64 = 1024 * 1024;

/// How an [`Engine`](crate::Engine) is configured when it starts.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The settings used until the user saves their own (`settings.set`);
    /// after that, the ones stored under the `"settings"` key win.
    pub default_settings: Settings,
    /// Scheduler knobs. The defaults are the tuned values; tests and the
    /// bench override them.
    pub tuning: Tuning,
    /// Advertised in `HelloResult::capabilities`. The daemon sets this from
    /// what its `SourceFactory` supports.
    pub capabilities: Vec<String>,
    /// Seeds retry jitter, for reproducible tests. `None` = random.
    pub rng_seed: Option<u64>,
    /// The wall clock the scheduler reads (queue start/stop times, speed
    /// windows). [`SystemClock`] unless a test drives its own.
    pub clock: Arc<dyn Clock>,
    /// Runs after-done actions (sleep, shut down). [`SystemPower`] unless
    /// a test records them instead.
    pub power: Arc<dyn PowerControl>,
}

impl EngineConfig {
    /// Defaults with downloads going to `default_directory` (absolute).
    pub fn new(default_directory: impl Into<String>) -> Self {
        Self {
            default_settings: Settings {
                default_directory: default_directory.into(),
                max_active_downloads: 3,
                max_connections_per_download: 16,
                speed_limit_bps: None,
                language: "en".into(),
                theme: "system".into(),
                durability: Default::default(),
                speed_schedule: Vec::new(),
            },
            tuning: Tuning::default(),
            capabilities: vec!["http".into(), "https".into(), "preconnect".into()],
            rng_seed: None,
            clock: Arc::new(SystemClock),
            power: Arc::new(SystemPower),
        }
    }
}

/// The scheduler's knobs (MASTER-PLAN §1). Every field has a tuned default;
/// see the crate README for what each one trades off.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Connections a download starts with when its host has no profile
    /// (8, as IDM: on links where connections help, starting there instead
    /// of at 4 gave 3–8 % more throughput in the link model; on a shared
    /// bottleneck it only means coming down later).
    pub initial_connections: u16,
    /// Pin every download to this many connections and disable learning
    /// (the bench's 1/4/16-connection runs). Still capped by
    /// `Settings::max_connections_per_download`.
    pub fixed_connections: Option<u16>,
    /// A fresh download's first connection (the response
    /// `RangeSource::start` began) runs alone until its time to finish —
    /// the rest of the file at its rate so far — exceeds both
    /// `solo_ttfb_factor` × what `start` took (a new connection's time to
    /// first byte) and `solo_min_eta`; checked every `solo_check`, which
    /// is also the least it is watched. A file it finishes sooner never
    /// gets more connections: they could not deliver earlier, and on a
    /// fast link they cost more than they save. Silence of
    /// `max(solo_silence, 4 × start's time to first byte)` ends the solo
    /// run at once (a stalled connection alone stalls the download).
    pub solo_ttfb_factor: f64,
    pub solo_min_eta: Duration,
    pub solo_check: Duration,
    pub solo_silence: Duration,
    /// When the first connection alone delivers less than `solo_window`
    /// bytes per round trip (its rate past the first burst × what `start`
    /// took, at least 1 ms) — under TCP's initial window, so the sender
    /// paces it: 2 MB/s at 1 ms is 2 KB — the server limits each
    /// connection, and the others start `limited_connections` strong at
    /// once instead of climbing there.
    pub solo_window: u64,
    pub limited_connections: u16,
    /// When the first window measured at the starting count (all
    /// connections streaming) delivers less than `1 + solo_collapse_gain`
    /// times what the first connection did alone, the others add nothing
    /// (a fast link, a shared bottleneck): the download drops to one
    /// connection at once, judged from one fast window — kept when it
    /// holds `keep_ratio` of the starting count (within noise, at most
    /// `solo_collapse_slack` below), else the count goes back up.
    pub solo_collapse_gain: f64,
    pub solo_collapse_slack: f64,
    /// Never split a range into pieces smaller than this; on a slow link,
    /// down to `min_split_floor`, as long as a piece still takes at least
    /// `max(min_split_time, 4 × median TTFB)` at the median connection's
    /// speed (2 MB/s per connection: 256 KiB pieces, so 10 MiB can use 16
    /// connections).
    pub min_split: u64,
    pub min_split_floor: u64,
    pub min_split_time: Duration,
    /// Split points are rounded down to a multiple of this when possible.
    pub split_align: u64,
    /// How often the scheduler samples throughput, checks for stalls and
    /// tunes the connection count. Finished connections are refilled
    /// immediately, not on the tick.
    pub tick: Duration,
    /// Connection-count measurement windows (see `tuner.rs`): at least
    /// `max(tune_window, tune_rtt_windows × median time-to-first-byte)`,
    /// extended up to `tune_window_max` until the relative standard error
    /// of `tune_sample`-long throughput samples is at most
    /// `tune_target_rse`.
    pub tune_window: Duration,
    pub tune_window_max: Duration,
    pub tune_rtt_windows: f64,
    pub tune_sample: Duration,
    pub tune_target_rse: f64,
    /// Warm-up before a window: at least `max(tune_window / 2,
    /// tune_warm_rtts × median TTFB)` after every connection streams, and
    /// longer while throughput still rises by more than `tune_warm_rise`
    /// per sample (slow start).
    pub tune_warm_rtts: f64,
    pub tune_warm_rise: f64,
    /// Base and trial windows alternate (A-B-A-B-A …, so drift cancels)
    /// until the comparison clears `tune_z` standard errors, for at most
    /// `tune_rounds` trial windows. A climb needs its lower confidence bound
    /// (and two paired comparisons, unless the gain is obvious) above
    /// `climb_gain`; after the last round an unclear climb is not taken and
    /// an unclear step down is taken if its estimate holds `keep_ratio`.
    pub tune_z: f64,
    pub tune_rounds: u32,
    /// Fast climb, a download's first climb only: windows of at least
    /// `max(tune_fast_window, tune_warm_rtts × median TTFB)` (warm-up half
    /// that, and not while rising), sampled every `tune_fast_sample`. A
    /// doubling that delivers, beyond `tune_z` standard errors, at least
    /// `tune_fast_scaling` of linear scaling (1.5× for 2× at 0.5) — every
    /// connection limited on its own — is taken from one window and the
    /// next doubling tried at once. Anything less ends the fast climb: the
    /// comparison continues with alternating windows as above.
    pub tune_fast_window: Duration,
    pub tune_fast_sample: Duration,
    pub tune_fast_scaling: f64,
    /// After settling, try one step again this often, alternately up and
    /// down (doubling, up to 5 min, while that changes nothing).
    pub tune_reprobe: Duration,
    /// A doubling is kept when its gain's lower confidence bound is at
    /// least this (4 %: with cheap connections, clear small gains count).
    pub climb_gain: f64,
    /// A halving is kept when it delivers at least this fraction of the
    /// count above it (97 %; between the two bars the count stays put).
    pub keep_ratio: f64,
    /// Time constant of the per-connection throughput EWMA.
    pub ewma_tau: Duration,
    /// A connection that delivered bytes and then went silent is stalled
    /// after `max(stall_min, stall_factor × expected gap between chunks)`.
    pub stall_min: Duration,
    pub stall_factor: f64,
    /// An owning connection, accepted by the server (headers in), that has
    /// delivered under `starved_share` (5 %) of what the streaming
    /// connections typically do, since its headers and in its recent rate,
    /// for `max(starved_time, starved_rtts × median TTFB)` (at most
    /// `max(starved_time, tune_window_max)`) while others stream, is
    /// starved: a server that serves only so many connections per client
    /// and parks the rest. It counts as refused: the count is capped at the
    /// connections streaming, and the starved ones are cancelled (their
    /// ranges go to the others).
    pub starved_share: f64,
    pub starved_time: Duration,
    pub starved_rtts: f64,
    /// A connection still waiting for its first byte is stalled after
    /// `max(stall_min, first_byte_factor × median time-to-first-byte)`, or
    /// after `first_byte_timeout` while no time-to-first-byte is known yet.
    pub first_byte_factor: f64,
    pub first_byte_timeout: Duration,
    /// In the endgame (nothing left to split, a connection slot free), a
    /// connection is hedged when its estimated finish time is more than
    /// `hedge_gain` × what a fresh connection would need. A connection
    /// silent for `endgame_silence` counts as never finishing.
    pub hedge_gain: f64,
    pub endgame_silence: Duration,
    /// At most this many hedges per range, and at most this many hedges in
    /// flight above the connection target.
    pub max_hedges_per_range: u32,
    pub max_extra_hedges: u16,
    /// Without ranges nothing can be hedged: a response silent this long
    /// is dropped and the download starts again from zero.
    pub stream_idle_timeout: Duration,
    /// Each connection gathers contiguous bytes and hands them to storage
    /// in one `write_at` of up to this many bytes (0 = one write per network
    /// chunk), or sooner once the oldest gathered byte waited
    /// `write_coalesce_delay` (a timer: also when the source goes quiet),
    /// at the end of a range, and before a connection leaves. One write per
    /// ~10 KiB network chunk costs a writer-thread wake-up and a `pwrite`
    /// each; gathering cuts both.
    pub write_coalesce: usize,
    pub write_coalesce_delay: Duration,
    /// Durability cadence: `Storage::flush` then `Journal::commit` every
    /// `checkpoint_interval` (or after `checkpoint_bytes` new bytes; off by
    /// default), but never sooner than the last checkpoint took divided by
    /// `checkpoint_share`, up to `checkpoint_interval_max`. A flush is a
    /// stall: the store's writer syncs while writes queue behind it, and on
    /// a disk-bound machine one of 64 MiB took 60–250 ms. So a flush that
    /// took 250 ms is next due 5 s later (5 % of the time), a 5 ms one after
    /// 2 s. The journal still never claims an unflushed byte; a crash only
    /// loses (re-downloads) more of the last seconds.
    pub checkpoint_interval: Duration,
    pub checkpoint_bytes: u64,
    pub checkpoint_share: f64,
    pub checkpoint_interval_max: Duration,
    /// While a transfer runs, flush (and, when resumable, commit) once this
    /// many new bytes are written and no flush is in flight, checked every
    /// `writeback_check`: the store syncs on a thread of its own, so the
    /// disk works alongside the network and the flush `finish` needs
    /// before the rename has little left to do.
    pub writeback_bytes: u64,
    pub writeback_check: Duration,
    /// Also flush (no commit) once `writeback_interval` has passed since
    /// the last flush started and at least `writeback_min_bytes` are new:
    /// a download slower than `writeback_bytes` per interval (one link
    /// throttled to 10 MB/s takes 1.6 s for 16 MiB) would otherwise leave
    /// everything to the flush between its last byte and `completed`.
    pub writeback_interval: Duration,
    pub writeback_min_bytes: u64,
    /// The download record (progress) is saved this often while running.
    pub record_interval: Duration,
    /// Retry backoff: `min(retry_cap, retry_base × 2^(n−1))` with jitter in
    /// the upper half, `n` = failure rounds since bytes last flowed.
    pub retry_base: Duration,
    pub retry_cap: Duration,
    /// A download fails after this many failure rounds in a row without a
    /// single byte arriving. Failures during one backoff window count once.
    pub error_budget: u32,
    /// Resume: bytes re-fetched from the end of every journalled range and
    /// compared with storage, and how many further windows to walk back
    /// when one differs before dropping the whole range.
    pub tail_verify_len: u64,
    pub tail_walkback: u32,
    /// Restarts from zero allowed when the remote file changes
    /// (`RemoteChanged`) before the download fails with `remote_changed`.
    pub max_remote_restarts: u32,
    /// Event cadence (PROTOCOL.md §4).
    pub progress_interval: Duration,
    pub segments_interval: Duration,
    pub stats_interval: Duration,
    /// Read size when hashing the finished file for a checksum.
    pub verify_chunk: usize,
    /// How often the scheduler looks at the wall clock: queue start/stop
    /// times fire and speed windows open within this of their minute.
    pub clock_tick: Duration,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            initial_connections: 8,
            fixed_connections: None,
            solo_ttfb_factor: 2.0,
            solo_min_eta: Duration::from_millis(100),
            solo_check: Duration::from_millis(5),
            solo_silence: Duration::from_millis(20),
            solo_window: 16 * 1024,
            limited_connections: 16,
            solo_collapse_gain: 0.25,
            solo_collapse_slack: 0.07,
            min_split: MIB,
            min_split_floor: 256 * 1024,
            min_split_time: Duration::from_millis(100),
            split_align: 64 * 1024,
            tick: Duration::from_millis(25),
            tune_window: Duration::from_millis(200),
            tune_window_max: Duration::from_secs(4),
            tune_rtt_windows: 16.0,
            tune_sample: Duration::from_millis(100),
            tune_target_rse: 0.02,
            tune_warm_rtts: 4.0,
            tune_warm_rise: 0.10,
            tune_z: 2.0,
            tune_rounds: 4,
            tune_fast_window: Duration::from_millis(50),
            tune_fast_sample: Duration::from_millis(25),
            tune_fast_scaling: 0.5,
            tune_reprobe: Duration::from_secs(30),
            climb_gain: 0.04,
            keep_ratio: 0.97,
            ewma_tau: Duration::from_millis(400),
            starved_share: 0.05,
            starved_time: Duration::from_secs(1),
            starved_rtts: 8.0,
            stall_min: Duration::from_millis(250),
            stall_factor: 12.0,
            first_byte_factor: 4.0,
            first_byte_timeout: Duration::from_secs(5),
            hedge_gain: 2.0,
            endgame_silence: Duration::from_millis(60),
            max_hedges_per_range: 3,
            max_extra_hedges: 4,
            stream_idle_timeout: Duration::from_secs(20),
            write_coalesce: 256 * 1024,
            write_coalesce_delay: Duration::from_millis(200),
            checkpoint_interval: Duration::from_secs(2),
            checkpoint_bytes: u64::MAX,
            checkpoint_share: 0.05,
            checkpoint_interval_max: Duration::from_secs(30),
            writeback_bytes: 16 * MIB,
            writeback_check: Duration::from_millis(10),
            writeback_interval: Duration::from_millis(100),
            writeback_min_bytes: MIB,
            record_interval: Duration::from_secs(3),
            retry_base: Duration::from_millis(50),
            retry_cap: Duration::from_secs(30),
            error_budget: 10,
            tail_verify_len: 64 * 1024,
            tail_walkback: 16,
            max_remote_restarts: 1,
            progress_interval: Duration::from_millis(250),
            segments_interval: Duration::from_secs(1),
            stats_interval: Duration::from_secs(1),
            verify_chunk: 1024 * 1024,
            clock_tick: Duration::from_secs(1),
        }
    }
}
