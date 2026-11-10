//! Construction-time configuration and the scheduler's tuning knobs.

use std::time::Duration;

use vayu_proto::Settings;

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
            },
            tuning: Tuning::default(),
            capabilities: vec!["http".into(), "https".into(), "preconnect".into()],
            rng_seed: None,
        }
    }
}

/// The scheduler's knobs (MASTER-PLAN §1). Every field has a tuned default;
/// see the crate README for what each one trades off.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// Connections a download starts with when its host has no profile.
    pub initial_connections: u16,
    /// Pin every download to this many connections and disable learning
    /// (the bench's 1/4/16-connection runs). Still capped by
    /// `Settings::max_connections_per_download`.
    pub fixed_connections: Option<u16>,
    /// Never split a range into pieces smaller than this.
    pub min_split: u64,
    /// Split points are rounded down to a multiple of this when possible.
    pub split_align: u64,
    /// How often the scheduler samples throughput, checks for stalls and
    /// tunes the connection count. Finished connections are refilled
    /// immediately, not on the tick.
    pub tick: Duration,
    /// Length of one measurement window of the connection-count hill climb.
    pub tune_window: Duration,
    /// A larger connection count is kept only if it raises throughput by at
    /// least this fraction.
    pub climb_gain: f64,
    /// A smaller connection count is kept if it delivers at least this
    /// fraction of the best throughput seen.
    pub keep_ratio: f64,
    /// Time constant of the per-connection throughput EWMA.
    pub ewma_tau: Duration,
    /// A connection that delivered bytes and then went silent is stalled
    /// after `max(stall_min, stall_factor × expected gap between chunks)`.
    pub stall_min: Duration,
    pub stall_factor: f64,
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
    /// Durability cadence: `Storage::flush` then `Journal::commit` at least
    /// this often, or after this many new bytes, whichever comes first.
    pub checkpoint_interval: Duration,
    pub checkpoint_bytes: u64,
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
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            initial_connections: 4,
            fixed_connections: None,
            min_split: MIB,
            split_align: 64 * 1024,
            tick: Duration::from_millis(50),
            tune_window: Duration::from_millis(200),
            climb_gain: 0.15,
            keep_ratio: 0.93,
            ewma_tau: Duration::from_millis(400),
            stall_min: Duration::from_millis(250),
            stall_factor: 12.0,
            first_byte_factor: 4.0,
            first_byte_timeout: Duration::from_secs(5),
            hedge_gain: 2.0,
            endgame_silence: Duration::from_millis(60),
            max_hedges_per_range: 3,
            max_extra_hedges: 4,
            stream_idle_timeout: Duration::from_secs(20),
            checkpoint_interval: Duration::from_secs(2),
            checkpoint_bytes: 64 * MIB,
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
        }
    }
}
