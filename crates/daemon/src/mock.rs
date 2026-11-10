//! [`MockService`]: an in-memory [`Service`] that behaves like a daemon with
//! a real engine, without touching the network or the disk.
//!
//! Downloads go `queued → probing → downloading → (verifying →) completed`
//! over time, split into segments that are re-split when a connection
//! finishes its range, honour `maxActiveDownloads`, `maxConnectionsPerDownload`
//! and the global speed limit, and emit `added`, `state`, `progress`
//! (≤ 4 per second), `removed`, `stats` (1 per second) and `settings`
//! events. Pause, resume, remove, refresh-URL and settings work as the
//! protocol says, including the error codes.
//!
//! Run it with `vayu-daemon --mock` so UIs, the CLI and the extension can be
//! built against a running daemon before the engine exists.
//!
//! # URL hints
//! The URL decides how a mock download behaves (query keys are optional):
//!
//! | Hint | Effect |
//! |---|---|
//! | `?size=20MiB` | file size (`B`, `K`/`KiB`, `M`/`MiB`, `G`/`GiB`; default: derived from the URL, 2 MiB – 3.5 GiB) |
//! | `?bps=50MiB` | nominal speed per download (default: derived from the URL, 4 – 48 MiB/s) |
//! | `?fail=0.3` | fails at 30 % with error code `network` (`&fail_code=remote_changed` to choose the code) |
//! | `?expire=0.5` | goes to `needs_refresh` (`link_expired`) at 50 %; `download.refreshUrl` continues it |
//! | `?corrupt=1` | with a checksum, verification fails (`checksum_mismatch`) |
//! | path segment `404` or `not-found` | probe/add fail with `-32005` (HTTP 404) |
//! | host ending in `.invalid` | probe/add fail with `-32005` (DNS) |
//! | `live`, `stream` in the URL | size unknown and not resumable |
//! | `noresume` in the URL | not resumable (one connection) |

use std::collections::HashSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::{FutureExt, StreamExt};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use vayu_proto::service::{Notification, Service, ServiceError, ServiceResult};
use vayu_proto::{
    AddParams, AddResult, DownloadDetail, DownloadState, DownloadSummary, ErrorInfo, IdParams,
    IdsParams, ListParams, PreconnectParams, ProbeParams, ProbeResult, ProgressEvent, ProgressItem,
    RefreshUrlParams, RemoveParams, RemovedEvent, SegmentInfo, Settings, StateEvent, Stats,
    category,
};

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// `daemonVersion`-style marker for the capabilities list.
pub const MOCK_CAPABILITY: &str = "mock";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// A realistic set of downloads in every state.
    Demo,
    /// No downloads.
    Empty,
}

impl std::str::FromStr for Scenario {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "demo" => Ok(Self::Demo),
            "empty" => Ok(Self::Empty),
            other => Err(format!("unknown mock scenario {other:?} (demo or empty)")),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockOptions {
    pub scenario: Scenario,
    /// Simulation step. Progress events are still capped at 4 per second.
    pub tick: Duration,
    /// Run the simulation on a Tokio timer. Off: call [`MockService::advance`].
    pub auto_tick: bool,
    /// Seeds the speed wobble.
    pub seed: u64,
    /// `Settings::default_directory`.
    pub default_directory: String,
}

impl Default for MockOptions {
    fn default() -> Self {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        Self {
            scenario: Scenario::Demo,
            tick: Duration::from_millis(250),
            auto_tick: true,
            seed: 0x5eed,
            default_directory: home.join("Downloads").to_string_lossy().into_owned(),
        }
    }
}

/// See the module docs.
pub struct MockService {
    state: Arc<Mutex<State>>,
    tick: Duration,
    ticker: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for MockService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MockService")
            .field("tick", &self.tick)
            .finish_non_exhaustive()
    }
}

impl MockService {
    /// Creates the mock. With `auto_tick` (the default) this must run inside a
    /// Tokio runtime.
    pub fn new(options: MockOptions) -> Arc<Self> {
        let mut state = State::new(&options);
        if options.scenario == Scenario::Demo {
            state.seed_demo();
        }
        let state = Arc::new(Mutex::new(state));
        let ticker = options.auto_tick.then(|| {
            let state = Arc::downgrade(&state);
            let tick = options.tick;
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(tick);
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                let mut last = tokio::time::Instant::now();
                loop {
                    interval.tick().await;
                    let Some(state) = state.upgrade() else { return };
                    let now = tokio::time::Instant::now();
                    lock(&state).step(now - last);
                    last = now;
                }
            })
        });
        Arc::new(Self {
            state,
            tick: options.tick,
            ticker: Mutex::new(ticker),
        })
    }

    /// Runs the simulation forward by `by`, in `tick` steps.
    pub fn advance(&self, by: Duration) {
        let mut left = by;
        while !left.is_zero() {
            let step = left.min(self.tick);
            lock(&self.state).step(step);
            left -= step;
        }
    }

    /// Every download with its segments, for assertions.
    pub fn snapshot(&self) -> Vec<DownloadDetail> {
        lock(&self.state).sims.iter().map(Sim::detail).collect()
    }

    fn with<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut State) -> ServiceResult<T>,
    ) -> BoxFuture<'_, ServiceResult<T>> {
        let result = {
            let mut state = lock(&self.state);
            if state.shut_down {
                Err(ServiceError::invalid_state("the daemon is shutting down"))
            } else {
                f(&mut state)
            }
        };
        futures::future::ready(result).boxed()
    }
}

impl Drop for MockService {
    fn drop(&mut self) {
        if let Some(ticker) = self.ticker.get_mut().ok().and_then(Option::take) {
            ticker.abort();
        }
    }
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

impl Service for MockService {
    fn capabilities(&self) -> Vec<String> {
        ["http", "https", "preconnect", MOCK_CAPABILITY]
            .map(String::from)
            .to_vec()
    }

    fn probe(&self, params: ProbeParams) -> BoxFuture<'_, ServiceResult<ProbeResult>> {
        self.with(move |_| Ok(probe(&params.url)?.result))
    }

    fn add(&self, params: AddParams) -> BoxFuture<'_, ServiceResult<AddResult>> {
        self.with(move |s| s.add(params).map(|id| AddResult { id }))
    }

    fn list(&self, params: ListParams) -> BoxFuture<'_, ServiceResult<Vec<DownloadSummary>>> {
        self.with(move |s| Ok(s.list(&params)))
    }

    fn get(&self, params: IdParams) -> BoxFuture<'_, ServiceResult<DownloadDetail>> {
        self.with(move |s| s.find(&params.id).map(|i| s.sims[i].detail()))
    }

    fn pause(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.with(move |s| s.pause(&params.ids))
    }

    fn resume(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.with(move |s| s.resume(&params.ids))
    }

    fn remove(&self, params: RemoveParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.with(move |s| s.remove(&params.ids))
    }

    fn refresh_url(&self, params: RefreshUrlParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.with(move |s| s.refresh_url(params))
    }

    fn preconnect(&self, params: PreconnectParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.with(move |_| parse_url(&params.url).map(|_| ()))
    }

    fn stats(&self) -> BoxFuture<'_, ServiceResult<Stats>> {
        self.with(|s| Ok(s.stats()))
    }

    fn settings(&self) -> BoxFuture<'_, ServiceResult<Settings>> {
        self.with(|s| Ok(s.settings.clone()))
    }

    fn set_settings(&self, settings: Settings) -> BoxFuture<'_, ServiceResult<Settings>> {
        self.with(move |s| {
            let settings = validate_settings(settings)?;
            s.settings = settings.clone();
            s.emit(Notification::Settings(settings.clone()));
            Ok(settings)
        })
    }

    fn subscribe(&self) -> BoxStream<'static, Notification> {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut state = lock(&self.state);
        if !state.shut_down {
            state.subscribers.push(tx);
        }
        futures::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|n| (n, rx)) })
            .boxed()
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        if let Some(ticker) = lock_ticker(&self.ticker).take() {
            ticker.abort();
        }
        let mut state = lock(&self.state);
        if !state.shut_down {
            // "Pause transfers": what a real engine does before exiting.
            let active: Vec<String> = state
                .sims
                .iter()
                .filter(|s| s.is_active())
                .map(|s| s.s.id.clone())
                .collect();
            for id in active {
                if let Ok(i) = state.find(&id) {
                    state.set_state(i, DownloadState::Paused, None);
                }
            }
            state.shut_down = true;
            // Dropping the senders ends every subscription stream.
            state.subscribers.clear();
        }
        futures::future::ready(()).boxed()
    }
}

fn lock_ticker(m: &Mutex<Option<JoinHandle<()>>>) -> MutexGuard<'_, Option<JoinHandle<()>>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ state

struct Sim {
    s: DownloadSummary,
    segments: Vec<SegmentInfo>,
    /// The real size, also for downloads whose size is "unknown".
    size: u64,
    nominal_bps: u64,
    fail_at: Option<f64>,
    fail_code: String,
    expire_at: Option<f64>,
    corrupt: bool,
    has_checksum: bool,
    /// Time spent in `probing` or `verifying`.
    phase: Duration,
    since_segments: Duration,
    /// Progress changed since the last progress event.
    dirty: bool,
}

impl Sim {
    fn is_active(&self) -> bool {
        matches!(
            self.s.state,
            DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying
        )
    }

    fn detail(&self) -> DownloadDetail {
        DownloadDetail {
            summary: self.s.clone(),
            segments: self.segments.clone(),
        }
    }

    fn source(&self) -> String {
        parse_url(&self.s.url)
            .map(|u| u.host)
            .unwrap_or_else(|_| "mock".into())
    }

    fn fraction(&self) -> f64 {
        if self.size == 0 {
            1.0
        } else {
            self.s.downloaded_bytes as f64 / self.size as f64
        }
    }

    fn progress_item(&mut self, with_segments: bool) -> ProgressItem {
        ProgressItem {
            id: self.s.id.clone(),
            downloaded_bytes: self.s.downloaded_bytes,
            total_bytes: self.s.total_bytes,
            speed_bps: self.s.speed_bps,
            eta_secs: self.s.eta_secs,
            connections: self.s.connections,
            segments: with_segments.then(|| {
                self.segments
                    .iter()
                    .map(|g| (g.start, g.end, g.downloaded_bytes))
                    .collect()
            }),
        }
    }
}

struct State {
    sims: Vec<Sim>,
    settings: Settings,
    subscribers: Vec<mpsc::UnboundedSender<Notification>>,
    next_id: u64,
    rng: u64,
    since_progress: Duration,
    since_stats: Duration,
    shut_down: bool,
}

impl State {
    fn new(options: &MockOptions) -> Self {
        Self {
            sims: Vec::new(),
            settings: Settings {
                default_directory: options.default_directory.clone(),
                max_active_downloads: 3,
                max_connections_per_download: 8,
                speed_limit_bps: None,
                language: "en".into(),
                theme: "system".into(),
            },
            subscribers: Vec::new(),
            next_id: 1,
            rng: options.seed | 1,
            since_progress: Duration::ZERO,
            since_stats: Duration::ZERO,
            shut_down: false,
        }
    }

    fn random(&mut self) -> f64 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let x = self.rng.wrapping_mul(0x2545_f491_4f6c_dd1d);
        (x >> 11) as f64 / (1u64 << 53) as f64
    }

    fn emit(&mut self, n: Notification) {
        self.subscribers.retain(|tx| tx.send(n.clone()).is_ok());
    }

    fn find(&self, id: &str) -> ServiceResult<usize> {
        self.sims
            .iter()
            .position(|s| s.s.id == id)
            .ok_or_else(|| ServiceError::not_found(id))
    }

    fn find_all(&self, ids: &[String]) -> ServiceResult<Vec<usize>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in ids {
            let i = self.find(id)?;
            if seen.insert(i) {
                out.push(i);
            }
        }
        Ok(out)
    }

    fn new_id(&mut self) -> String {
        let n = self.next_id;
        self.next_id += 1;
        // A bijection of the counter: unique, but not obviously sequential,
        // so nobody is tempted to parse ids.
        let mut z = n.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        format!("dl_{z:016x}")
    }

    fn set_state(&mut self, i: usize, state: DownloadState, error: Option<ErrorInfo>) {
        let sim = &mut self.sims[i];
        if sim.s.state == state && sim.s.error == error {
            return;
        }
        sim.s.state = state;
        sim.s.error = error.clone();
        sim.phase = Duration::ZERO;
        if !matches!(state, DownloadState::Downloading) {
            sim.s.speed_bps = 0;
            sim.s.eta_secs = None;
            sim.s.connections = 0;
            for g in &mut sim.segments {
                g.speed_bps = 0;
            }
        }
        if state == DownloadState::Completed {
            sim.s.completed_at = Some(now_ms());
        }
        let event = StateEvent {
            id: sim.s.id.clone(),
            state,
            error,
        };
        self.emit(Notification::DownloadState(event));
    }

    fn unique_filename(&self, directory: &str, wanted: &str) -> String {
        let taken = |name: &str| {
            self.sims
                .iter()
                .any(|s| s.s.directory == directory && s.s.filename == name)
        };
        if !taken(wanted) {
            return wanted.to_owned();
        }
        let (stem, ext) = split_extension(wanted);
        (1..)
            .map(|n| format!("{stem} ({n}){ext}"))
            .find(|name| !taken(name))
            .unwrap_or_else(|| wanted.to_owned())
    }

    fn insert(&mut self, mut sim: Sim) -> String {
        sim.s.id = self.new_id();
        let id = sim.s.id.clone();
        self.sims.push(sim);
        id
    }

    fn add(&mut self, params: AddParams) -> ServiceResult<String> {
        let probed = probe(&params.url)?;
        let directory = match params.directory.as_deref().map(str::trim) {
            Some(dir) if !dir.is_empty() => {
                if !is_absolute(dir) {
                    return Err(ServiceError::invalid_params(
                        "directory must be an absolute path",
                    ));
                }
                dir.to_owned()
            }
            _ => self.settings.default_directory.clone(),
        };
        let has_checksum = match &params.checksum {
            Some(c) => {
                validate_checksum(&c.algorithm, &c.value)?;
                true
            }
            None => false,
        };
        for mirror in &params.mirrors {
            parse_url(mirror)?;
        }
        let wanted = match params.filename.as_deref() {
            Some(name) => sanitize_filename(name),
            None => probed.result.filename.clone(),
        };
        let filename = self.unique_filename(&directory, &wanted);
        let category = params
            .category
            .clone()
            .unwrap_or_else(|| category_for(&filename).to_owned());
        let nominal_bps = probed
            .bps
            .unwrap_or_else(|| 4 * MIB + (44.0 * MIB as f64 * self.random()) as u64);
        let sim = Sim {
            s: DownloadSummary {
                id: String::new(),
                url: probed.result.final_url.clone(),
                filename,
                directory,
                state: if params.start {
                    DownloadState::Queued
                } else {
                    DownloadState::Paused
                },
                total_bytes: probed.result.total_bytes,
                downloaded_bytes: 0,
                speed_bps: 0,
                eta_secs: None,
                connections: 0,
                resumable: probed.result.resumable,
                category: Some(category),
                queue: None,
                added_at: now_ms(),
                completed_at: None,
                error: None,
            },
            segments: Vec::new(),
            size: probed.size,
            nominal_bps,
            fail_at: probed.fail_at,
            fail_code: probed.fail_code,
            expire_at: probed.expire_at,
            corrupt: probed.corrupt,
            has_checksum,
            phase: Duration::ZERO,
            since_segments: Duration::ZERO,
            dirty: false,
        };
        let id = self.insert(sim);
        let summary = self.sims.last().map(|s| s.s.clone());
        if let Some(summary) = summary {
            self.emit(Notification::DownloadAdded(summary));
        }
        Ok(id)
    }

    fn list(&self, params: &ListParams) -> Vec<DownloadSummary> {
        let mut out: Vec<&Sim> = self
            .sims
            .iter()
            .filter(|s| params.states.is_empty() || params.states.contains(&s.s.state))
            .filter(|s| {
                params
                    .category
                    .as_ref()
                    .is_none_or(|c| s.s.category.as_ref() == Some(c))
            })
            .collect();
        // Newest first; insertion order breaks ties.
        out.reverse();
        out.sort_by_key(|s| std::cmp::Reverse(s.s.added_at));
        out.into_iter()
            .skip(params.offset.unwrap_or(0) as usize)
            .take(params.limit.map_or(usize::MAX, |l| l as usize))
            .map(|s| s.s.clone())
            .collect()
    }

    fn pause(&mut self, ids: &[String]) -> ServiceResult<()> {
        let targets = self.find_all(ids)?;
        if let Some(&bad) = targets.iter().find(|&&i| {
            !matches!(
                self.sims[i].s.state,
                DownloadState::Queued
                    | DownloadState::Probing
                    | DownloadState::Downloading
                    | DownloadState::Paused
            )
        }) {
            return Err(ServiceError::invalid_state(format!(
                "cannot pause a {} download",
                state_name(self.sims[bad].s.state)
            )));
        }
        for i in targets {
            self.set_state(i, DownloadState::Paused, None);
        }
        Ok(())
    }

    fn resume(&mut self, ids: &[String]) -> ServiceResult<()> {
        let targets = self.find_all(ids)?;
        if let Some(&bad) = targets.iter().find(|&&i| {
            matches!(
                self.sims[i].s.state,
                DownloadState::Completed | DownloadState::NeedsRefresh | DownloadState::Verifying
            )
        }) {
            let state = self.sims[bad].s.state;
            return Err(ServiceError::invalid_state(match state {
                DownloadState::NeedsRefresh => {
                    "the link expired; refresh it first (download.refreshUrl)".to_owned()
                }
                other => format!("cannot resume a {} download", state_name(other)),
            }));
        }
        for i in targets {
            let sim = &mut self.sims[i];
            match sim.s.state {
                DownloadState::Paused => {}
                DownloadState::Failed => {
                    if !sim.s.resumable {
                        sim.s.downloaded_bytes = 0;
                        sim.segments.clear();
                    }
                    // A retry does not fail at the same place again.
                    sim.fail_at = None;
                }
                _ => continue,
            }
            self.set_state(i, DownloadState::Queued, None);
        }
        Ok(())
    }

    fn remove(&mut self, ids: &[String]) -> ServiceResult<()> {
        let targets = self.find_all(ids)?;
        if targets.is_empty() {
            return Ok(());
        }
        let removed: Vec<String> = targets.iter().map(|&i| self.sims[i].s.id.clone()).collect();
        self.sims.retain(|s| !removed.contains(&s.s.id));
        self.emit(Notification::DownloadRemoved(RemovedEvent { ids: removed }));
        Ok(())
    }

    fn refresh_url(&mut self, params: RefreshUrlParams) -> ServiceResult<()> {
        let i = self.find(&params.id)?;
        let url = parse_url(&params.url)?;
        if self.sims[i].s.state == DownloadState::Completed {
            return Err(ServiceError::invalid_state(
                "the download is already complete",
            ));
        }
        let sim = &mut self.sims[i];
        sim.s.url = url.href;
        sim.expire_at = None;
        if matches!(
            sim.s.state,
            DownloadState::NeedsRefresh | DownloadState::Failed
        ) {
            self.set_state(i, DownloadState::Queued, None);
        }
        Ok(())
    }

    fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for sim in &self.sims {
            match sim.s.state {
                DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying => {
                    stats.active += 1;
                    stats.download_bps += sim.s.speed_bps;
                }
                DownloadState::Queued => stats.queued += 1,
                _ => {}
            }
        }
        stats
    }

    // -------------------------------------------------------------- simulation

    fn step(&mut self, dt: Duration) {
        if self.shut_down || dt.is_zero() {
            return;
        }
        let secs = dt.as_secs_f64();

        // Phases that just take a moment.
        for i in 0..self.sims.len() {
            let state = self.sims[i].s.state;
            self.sims[i].phase += dt;
            let phase = self.sims[i].phase;
            if state == DownloadState::Probing && phase >= Duration::from_millis(300) {
                self.start_transfer(i);
            } else if state == DownloadState::Verifying && phase >= Duration::from_millis(500) {
                if self.sims[i].corrupt {
                    let error = ErrorInfo {
                        code: "checksum_mismatch".into(),
                        message: "The file does not match the expected checksum.".into(),
                    };
                    self.set_state(i, DownloadState::Failed, Some(error));
                } else {
                    self.set_state(i, DownloadState::Completed, None);
                }
            }
        }

        // Start queued downloads, oldest first, up to the active limit.
        let mut active = self.sims.iter().filter(|s| s.is_active()).count();
        let max_active = self.settings.max_active_downloads.max(1) as usize;
        for i in 0..self.sims.len() {
            if active >= max_active {
                break;
            }
            if self.sims[i].s.state == DownloadState::Queued {
                self.set_state(i, DownloadState::Probing, None);
                active += 1;
            }
        }

        // Transfer.
        let downloading: Vec<usize> = (0..self.sims.len())
            .filter(|&i| self.sims[i].s.state == DownloadState::Downloading)
            .collect();
        let desired: Vec<f64> = downloading
            .iter()
            .map(|&i| {
                let wobble = 0.8 + 0.4 * self.random();
                self.sims[i].nominal_bps as f64 * wobble
            })
            .collect();
        let total: f64 = desired.iter().sum();
        let scale = match self.settings.speed_limit_bps {
            Some(limit) if total > limit as f64 => limit as f64 / total,
            _ => 1.0,
        };
        let max_conns = self.settings.max_connections_per_download.clamp(1, 64) as usize;
        let mut finished = Vec::new();
        for (&i, want) in downloading.iter().zip(desired) {
            if let Some(end) = self.transfer(i, (want * scale * secs) as u64, secs, max_conns) {
                finished.push((i, end));
            }
        }

        // Events. A download's last progress goes out before its final state.
        self.since_progress += dt;
        self.since_stats += dt;
        for sim in &mut self.sims {
            sim.since_segments += dt;
        }
        if self.since_progress >= Duration::from_millis(250) || !finished.is_empty() {
            self.since_progress = Duration::ZERO;
            let mut items = Vec::new();
            for sim in &mut self.sims {
                if !sim.dirty {
                    continue;
                }
                sim.dirty = false;
                let with_segments =
                    sim.since_segments >= Duration::from_secs(1) && !sim.segments.is_empty();
                if with_segments {
                    sim.since_segments = Duration::ZERO;
                }
                items.push(sim.progress_item(with_segments));
            }
            if !items.is_empty() {
                self.emit(Notification::DownloadProgress(ProgressEvent { items }));
            }
        }
        for (i, (state, error)) in finished {
            self.set_state(i, state, error);
        }
        if self.since_stats >= Duration::from_secs(1) {
            self.since_stats = Duration::ZERO;
            let stats = self.stats();
            self.emit(Notification::Stats(stats));
        }
    }

    fn start_transfer(&mut self, i: usize) {
        let max_conns = self.settings.max_connections_per_download.clamp(1, 64) as u64;
        let source = self.sims[i].source();
        let sim = &mut self.sims[i];
        if sim.segments.is_empty() && sim.s.total_bytes.is_some() {
            let n = if sim.s.resumable {
                max_conns.min(8).min((sim.size / MIB).max(1))
            } else {
                1
            };
            let chunk = sim.size.div_ceil(n);
            sim.segments = (0..n)
                .map(|k| (k * chunk, ((k + 1) * chunk).min(sim.size)))
                .filter(|(start, end)| start < end)
                .map(|(start, end)| SegmentInfo {
                    start,
                    end,
                    downloaded_bytes: 0,
                    speed_bps: 0,
                    source: source.clone(),
                })
                .collect();
            // Continue from bytes already "on disk" (e.g. a resumed demo).
            let mut left = sim.s.downloaded_bytes;
            for g in &mut sim.segments {
                let take = left.min(g.end - g.start);
                g.downloaded_bytes = take;
                left -= take;
            }
        }
        self.set_state(i, DownloadState::Downloading, None);
    }

    /// Moves bytes for one download. Returns the state it must move to when
    /// it finished, failed or expired (applied by the caller after the
    /// progress event).
    fn transfer(
        &mut self,
        i: usize,
        bytes: u64,
        secs: f64,
        max_conns: usize,
    ) -> Option<(DownloadState, Option<ErrorInfo>)> {
        let (code, size, fail_at, expire_at) = {
            let s = &self.sims[i];
            (s.fail_code.clone(), s.size, s.fail_at, s.expire_at)
        };
        let sim = &mut self.sims[i];
        let remaining = size.saturating_sub(sim.s.downloaded_bytes);
        let bytes = bytes.min(remaining);
        if sim.segments.is_empty() {
            sim.s.downloaded_bytes += bytes;
            sim.s.connections = 1;
        } else {
            distribute(&mut sim.segments, bytes, secs);
            split_segments(&mut sim.segments, max_conns);
            sim.s.downloaded_bytes = sim.segments.iter().map(|g| g.downloaded_bytes).sum();
            sim.s.connections = sim
                .segments
                .iter()
                .filter(|g| g.downloaded_bytes < g.end - g.start)
                .count() as u32;
        }
        let instant = bytes as f64 / secs.max(1e-3);
        sim.s.speed_bps = if sim.s.speed_bps == 0 {
            instant as u64
        } else {
            (sim.s.speed_bps as f64 * 0.6 + instant * 0.4) as u64
        };
        sim.s.eta_secs = match (sim.s.total_bytes, sim.s.speed_bps) {
            (Some(total), speed) if speed > 0 => {
                Some(total.saturating_sub(sim.s.downloaded_bytes).div_ceil(speed))
            }
            _ => None,
        };
        sim.dirty = true;

        let fraction = sim.fraction();
        if fail_at.is_some_and(|f| fraction >= f) {
            let error = ErrorInfo {
                code,
                message: "The connection was reset by the server.".into(),
            };
            Some((DownloadState::Failed, Some(error)))
        } else if expire_at.is_some_and(|f| fraction >= f) {
            let error = ErrorInfo {
                code: "link_expired".into(),
                message: "The download link expired; open the page again to refresh it.".into(),
            };
            Some((DownloadState::NeedsRefresh, Some(error)))
        } else if sim.s.downloaded_bytes >= size {
            sim.s.total_bytes = Some(size);
            let next = if sim.has_checksum {
                DownloadState::Verifying
            } else {
                DownloadState::Completed
            };
            Some((next, None))
        } else {
            None
        }
    }

    fn seed_demo(&mut self) {
        let now = now_ms();
        for d in DEMO {
            let probed = match probe(d.url) {
                Ok(p) => p,
                Err(_) => continue,
            };
            let size = probed.size;
            let downloaded = (size as f64 * d.fraction) as u64;
            let filename = probed.result.filename.clone();
            let category = category_for(&filename).to_owned();
            let error = d.error.map(|(code, message)| ErrorInfo {
                code: code.into(),
                message: message.into(),
            });
            let added_at = now - d.age_min * 60_000;
            let sim = Sim {
                s: DownloadSummary {
                    id: String::new(),
                    url: probed.result.final_url.clone(),
                    filename,
                    directory: self.settings.default_directory.clone(),
                    state: d.state,
                    total_bytes: probed.result.total_bytes,
                    downloaded_bytes: downloaded,
                    speed_bps: 0,
                    eta_secs: None,
                    connections: 0,
                    resumable: probed.result.resumable,
                    category: Some(category),
                    queue: None,
                    added_at,
                    completed_at: (d.state == DownloadState::Completed)
                        .then_some(added_at + 90_000),
                    error,
                },
                segments: Vec::new(),
                size,
                nominal_bps: probed.bps.unwrap_or(12 * MIB),
                fail_at: None,
                fail_code: "network".into(),
                expire_at: None,
                corrupt: false,
                has_checksum: false,
                phase: Duration::ZERO,
                since_segments: Duration::ZERO,
                dirty: false,
            };
            self.insert(sim);
        }
        // Downloads shown as "downloading" start from their saved progress.
        for i in 0..self.sims.len() {
            if self.sims[i].s.state == DownloadState::Downloading {
                self.sims[i].s.state = DownloadState::Probing;
                self.start_transfer(i);
            }
        }
    }
}

/// Splits `bytes` over the unfinished segments as evenly as possible.
fn distribute(segments: &mut [SegmentInfo], mut bytes: u64, secs: f64) {
    let mut moved = vec![0u64; segments.len()];
    loop {
        let open: Vec<usize> = (0..segments.len())
            .filter(|&k| segments[k].downloaded_bytes < segments[k].end - segments[k].start)
            .collect();
        if open.is_empty() || bytes == 0 {
            break;
        }
        let share = (bytes / open.len() as u64).max(1);
        for k in open {
            let g = &mut segments[k];
            let take = share.min(g.end - g.start - g.downloaded_bytes).min(bytes);
            g.downloaded_bytes += take;
            moved[k] += take;
            bytes -= take;
            if bytes == 0 {
                break;
            }
        }
    }
    for (g, m) in segments.iter_mut().zip(moved) {
        g.speed_bps = (m as f64 / secs.max(1e-3)) as u64;
    }
}

/// Work stealing, simplified: while fewer than `max` connections are busy,
/// the largest remaining range (if ≥ 2 MiB) is cut in half and the second
/// half becomes a new segment.
fn split_segments(segments: &mut Vec<SegmentInfo>, max: usize) {
    loop {
        let open = segments
            .iter()
            .filter(|g| g.downloaded_bytes < g.end - g.start)
            .count();
        if open >= max || open == 0 {
            return;
        }
        let Some((k, left)) = segments
            .iter()
            .enumerate()
            .map(|(k, g)| (k, g.end - g.start - g.downloaded_bytes))
            .max_by_key(|&(_, left)| left)
        else {
            return;
        };
        if left < 2 * MIB {
            return;
        }
        let g = &mut segments[k];
        let mid = g.start + g.downloaded_bytes + left / 2;
        let new = SegmentInfo {
            start: mid,
            end: g.end,
            downloaded_bytes: 0,
            speed_bps: 0,
            source: g.source.clone(),
        };
        g.end = mid;
        segments.push(new);
    }
}

// ------------------------------------------------------------------ URLs

struct Url {
    href: String,
    host: String,
    path: String,
    query: Vec<(String, String)>,
}

fn parse_url(raw: &str) -> ServiceResult<Url> {
    let raw = raw.trim();
    let bad = || ServiceError::invalid_params(format!("not a valid URL: {raw:?}"));
    let (scheme, rest) = raw.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(ServiceError::unsupported(format!(
            "{scheme}: URLs are not supported (http and https are)"
        )));
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let (authority, path_query) = match rest.find(['/', '?']) {
        Some(at) => rest.split_at(at),
        None => (rest, ""),
    };
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if host_port.starts_with('[') {
        host_port.split_inclusive(']').next().unwrap_or_default()
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    if host.is_empty() || host.contains(char::is_whitespace) {
        return Err(bad());
    }
    let (path, query) = path_query.split_once('?').unwrap_or((path_query, ""));
    let query = query
        .split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect();
    Ok(Url {
        href: raw.to_owned(),
        host: host.to_ascii_lowercase(),
        path: if path.is_empty() {
            "/".into()
        } else {
            path.into()
        },
        query,
    })
}

fn percent_decode(s: &str) -> String {
    fn hex(b: u8) -> Option<u8> {
        (b as char).to_digit(16).map(|d| d as u8)
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(if bytes[i] == b'+' { b' ' } else { bytes[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Probed {
    result: ProbeResult,
    size: u64,
    bps: Option<u64>,
    fail_at: Option<f64>,
    fail_code: String,
    expire_at: Option<f64>,
    corrupt: bool,
}

fn probe(raw: &str) -> ServiceResult<Probed> {
    let url = parse_url(raw)?;
    if url.host.ends_with(".invalid") {
        return Err(ServiceError::network(format!(
            "could not resolve host {}",
            url.host
        )));
    }
    if url
        .path
        .split('/')
        .any(|seg| seg == "404" || seg == "not-found")
    {
        return Err(ServiceError::network(
            "the server says this file does not exist (HTTP 404)",
        ));
    }
    let hint = |key: &str| {
        url.query
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    };
    let bad_hint = |key: &str| ServiceError::invalid_params(format!("bad mock hint {key}"));
    let fraction = |key: &str| -> ServiceResult<Option<f64>> {
        hint(key)
            .map(|v| {
                v.parse::<f64>()
                    .ok()
                    .filter(|f| (0.0..=1.0).contains(f))
                    .ok_or_else(|| bad_hint(key))
            })
            .transpose()
    };
    let lower = url.href.to_ascii_lowercase();
    let live = lower.contains("live") || lower.contains("stream");
    let resumable = !live && !lower.contains("noresume");
    let size = match hint("size") {
        Some(v) => parse_size(v).ok_or_else(|| bad_hint("size"))?,
        None => 2 * MIB + (hash(&url.href) as f64 / u64::MAX as f64 * 3.5 * GIB as f64) as u64,
    };
    let bps = hint("bps")
        .map(|v| {
            parse_size(v)
                .filter(|&b| b > 0)
                .ok_or_else(|| bad_hint("bps"))
        })
        .transpose()?;
    let filename = filename_from_path(&url.path);
    let content_type = content_type_for(category_for(&filename)).map(String::from);
    Ok(Probed {
        result: ProbeResult {
            total_bytes: (!live).then_some(size),
            resumable,
            filename,
            content_type,
            final_url: url.href.clone(),
        },
        size,
        bps,
        fail_at: fraction("fail")?,
        fail_code: hint("fail_code").unwrap_or("network").to_owned(),
        expire_at: fraction("expire")?,
        corrupt: hint("corrupt").is_some_and(|v| v != "0"),
    })
}

/// `"10MiB"` → 10485760. Binary multiples; `K`, `KB` and `KiB` all mean 1024.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => KIB,
        "m" | "mb" | "mib" => MIB,
        "g" | "gb" | "gib" => GIB,
        _ => return None,
    };
    let bytes = number * multiplier as f64;
    (bytes.is_finite() && bytes >= 0.0 && bytes < u64::MAX as f64).then_some(bytes as u64)
}

fn hash(s: &str) -> u64 {
    // FNV-1a: stable across runs and platforms.
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

fn filename_from_path(path: &str) -> String {
    let last = path.rsplit('/').find(|s| !s.is_empty()).unwrap_or_default();
    let name = sanitize_filename(&percent_decode(last));
    if name.is_empty() {
        "download".into()
    } else {
        name
    }
}

/// Path separators, control characters and characters Windows forbids
/// become `_`; leading dots and trailing dots/spaces are dropped.
pub fn sanitize_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' ']);
    let name: String = trimmed.chars().take(200).collect();
    if name.is_empty() {
        "download".into()
    } else {
        name
    }
}

fn split_extension(name: &str) -> (&str, &str) {
    for compound in [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2"] {
        if name.len() > compound.len() && name.to_ascii_lowercase().ends_with(compound) {
            return name.split_at(name.len() - compound.len());
        }
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => name.split_at(dot),
        _ => (name, ""),
    }
}

/// The built-in category for a filename (same table as the UI's
/// `categoryForFilename`).
pub fn category_for(filename: &str) -> &'static str {
    let lower = filename.to_ascii_lowercase();
    if [".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return category::COMPRESSED;
    }
    let Some((_, ext)) = lower.rsplit_once('.') else {
        return category::OTHER;
    };
    const TABLE: [(&str, &str); 6] = [
        (
            category::COMPRESSED,
            "zip rar 7z tar gz tgz xz bz2 zst iso dmg img",
        ),
        (
            category::DOCUMENTS,
            "pdf doc docx xls xlsx ppt pptx odt ods odp txt rtf epub csv md",
        ),
        (category::MUSIC, "mp3 flac wav ogg opus m4a aac wma"),
        (category::VIDEO, "mp4 mkv webm avi mov m4v ts m3u8 wmv flv"),
        (
            category::PROGRAMS,
            "exe msi deb rpm appimage apk pkg flatpak snap",
        ),
        (
            category::IMAGES,
            "jpg jpeg png gif webp svg avif heic bmp tif tiff",
        ),
    ];
    TABLE
        .iter()
        .find(|(_, exts)| exts.split(' ').any(|e| e == ext))
        .map_or(category::OTHER, |(cat, _)| cat)
}

fn content_type_for(category: &str) -> Option<&'static str> {
    Some(match category {
        category::VIDEO => "video/mp4",
        category::MUSIC => "audio/mpeg",
        category::DOCUMENTS => "application/pdf",
        category::COMPRESSED => "application/zip",
        category::PROGRAMS => "application/octet-stream",
        category::IMAGES => "image/jpeg",
        _ => return None,
    })
}

fn is_absolute(dir: &str) -> bool {
    // Either platform's notion: the daemon may serve a client on Windows
    // paths in tests run on Linux, and vice versa.
    dir.starts_with('/')
        || dir.starts_with("\\\\")
        || (dir.len() >= 3
            && dir.as_bytes()[0].is_ascii_alphabetic()
            && &dir[1..2] == ":"
            && (dir.as_bytes()[2] == b'\\' || dir.as_bytes()[2] == b'/'))
}

fn validate_checksum(algorithm: &str, value: &str) -> ServiceResult<()> {
    let len = match algorithm {
        "md5" => 32,
        "sha1" => 40,
        "sha256" => 64,
        "sha512" => 128,
        other => {
            return Err(ServiceError::invalid_params(format!(
                "unknown checksum algorithm {other:?} (md5, sha1, sha256, sha512)"
            )));
        }
    };
    if value.len() != len
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(ServiceError::invalid_params(format!(
            "a {algorithm} checksum is {len} lowercase hex digits"
        )));
    }
    Ok(())
}

fn validate_settings(s: Settings) -> ServiceResult<Settings> {
    let bad = |m: &str| Err(ServiceError::invalid_params(m.to_owned()));
    if s.default_directory.trim().is_empty() {
        return bad("defaultDirectory is required");
    }
    if !(1..=64).contains(&s.max_active_downloads) {
        return bad("maxActiveDownloads must be 1–64");
    }
    if !(1..=64).contains(&s.max_connections_per_download) {
        return bad("maxConnectionsPerDownload must be 1–64");
    }
    if s.speed_limit_bps == Some(0) {
        return bad("speedLimitBps must be a positive integer (omit it for no limit)");
    }
    if s.language.is_empty() {
        return bad("language is required");
    }
    if !["system", "light", "dark"].contains(&s.theme.as_str()) {
        return bad("theme must be system, light or dark");
    }
    Ok(Settings {
        default_directory: s.default_directory.trim().to_owned(),
        ..s
    })
}

fn state_name(state: DownloadState) -> &'static str {
    match state {
        DownloadState::Queued => "queued",
        DownloadState::Probing => "probing",
        DownloadState::Downloading => "downloading",
        DownloadState::Paused => "paused",
        DownloadState::NeedsRefresh => "needs_refresh",
        DownloadState::Verifying => "verifying",
        DownloadState::Completed => "completed",
        DownloadState::Failed => "failed",
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

struct Demo {
    url: &'static str,
    state: DownloadState,
    fraction: f64,
    age_min: i64,
    error: Option<(&'static str, &'static str)>,
}

const DEMO: &[Demo] = &[
    Demo {
        url: "https://releases.example.org/linux/distro-26.04-desktop-amd64.iso?size=5.8GiB&bps=38MiB",
        state: DownloadState::Downloading,
        fraction: 0.37,
        age_min: 4,
        error: None,
    },
    Demo {
        url: "https://media.example.net/films/Open%20Movie%20(2160p).mkv?size=2.1GiB&bps=21MiB",
        state: DownloadState::Downloading,
        fraction: 0.12,
        age_min: 2,
        error: None,
    },
    Demo {
        url: "https://cdn.example.com/podcasts/episode-142.mp3?size=86MiB",
        state: DownloadState::Paused,
        fraction: 0.62,
        age_min: 35,
        error: None,
    },
    Demo {
        url: "https://downloads.example.com/tools/toolchain-1.99.1-x86_64-linux.tar.xz?size=312MiB",
        state: DownloadState::Completed,
        fraction: 1.0,
        age_min: 180,
        error: None,
    },
    Demo {
        url: "https://docs.example.org/reports/annual-report-2026.pdf?size=14MiB",
        state: DownloadState::Completed,
        fraction: 1.0,
        age_min: 1440,
        error: None,
    },
    Demo {
        url: "https://data.example.net/datasets/images-v3.zip?size=1.4GiB",
        state: DownloadState::Failed,
        fraction: 0.48,
        age_min: 60,
        error: Some(("network", "The connection was reset by the server.")),
    },
    Demo {
        url: "https://files.example.com/signed/installer-setup.exe?size=148MiB",
        state: DownloadState::NeedsRefresh,
        fraction: 0.21,
        age_min: 25,
        error: Some((
            "link_expired",
            "The download link expired; open the page again to refresh it.",
        )),
    },
    Demo {
        url: "https://images.example.org/packs/wallpaper-pack.7z?size=640MiB",
        state: DownloadState::Queued,
        fraction: 0.0,
        age_min: 1,
        error: None,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    fn manual() -> Arc<MockService> {
        MockService::new(MockOptions {
            scenario: Scenario::Empty,
            auto_tick: false,
            default_directory: "/downloads".into(),
            ..MockOptions::default()
        })
    }

    fn add(url: &str) -> AddParams {
        serde_json::from_value(serde_json::json!({ "url": url })).unwrap()
    }

    fn drain(events: &mut BoxStream<'static, Notification>) -> Vec<Notification> {
        let mut out = Vec::new();
        while let Some(Some(n)) = events.next().now_or_never() {
            out.push(n);
        }
        out
    }

    #[tokio::test]
    async fn a_download_runs_to_completion_with_events() {
        let mock = manual();
        let mut events = mock.subscribe();
        let id = mock
            .add(add("https://example.com/files/a.bin?size=8MiB&bps=16MiB"))
            .await
            .unwrap()
            .id;
        mock.advance(Duration::from_secs(2));
        let detail = mock.get(IdParams { id: id.clone() }).await.unwrap();
        assert_eq!(detail.summary.state, DownloadState::Completed);
        assert_eq!(detail.summary.downloaded_bytes, 8 * MIB);
        assert_eq!(detail.summary.directory, "/downloads");
        assert_eq!(detail.summary.filename, "a.bin");
        assert!(detail.summary.completed_at.is_some());
        let covered: u64 = detail.segments.iter().map(|g| g.end - g.start).sum();
        assert_eq!(covered, 8 * MIB, "segments tile the file");

        let events = drain(&mut events);
        let states: Vec<_> = events
            .iter()
            .filter_map(|n| match n {
                Notification::DownloadState(s) => Some(s.state),
                _ => None,
            })
            .collect();
        assert_eq!(
            states,
            [
                DownloadState::Probing,
                DownloadState::Downloading,
                DownloadState::Completed
            ]
        );
        assert!(matches!(events[0], Notification::DownloadAdded(_)));
        assert!(
            events
                .iter()
                .any(|n| matches!(n, Notification::DownloadProgress(_)))
        );
        assert!(events.iter().any(|n| matches!(n, Notification::Stats(_))));
    }

    #[tokio::test]
    async fn failures_expiry_and_refresh() {
        let mock = manual();
        let fail = mock
            .add(add(
                "https://example.com/f.bin?size=4MiB&bps=4MiB&fail=0.5&fail_code=remote_changed",
            ))
            .await
            .unwrap()
            .id;
        let expire = mock
            .add(add(
                "https://example.com/e.bin?size=4MiB&bps=4MiB&expire=0.5",
            ))
            .await
            .unwrap()
            .id;
        mock.advance(Duration::from_secs(3));
        let f = mock
            .get(IdParams { id: fail.clone() })
            .await
            .unwrap()
            .summary;
        assert_eq!(f.state, DownloadState::Failed);
        assert_eq!(f.error.unwrap().code, "remote_changed");
        let e = mock
            .get(IdParams { id: expire.clone() })
            .await
            .unwrap()
            .summary;
        assert_eq!(e.state, DownloadState::NeedsRefresh);
        assert_eq!(e.error.unwrap().code, "link_expired");

        // A needs_refresh download cannot just be resumed...
        let err = mock
            .resume(IdsParams {
                ids: vec![expire.clone()],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, vayu_proto::error_code::INVALID_STATE);
        // ...but continues after refreshUrl.
        mock.refresh_url(RefreshUrlParams {
            id: expire.clone(),
            url: "https://example.com/e.bin?size=4MiB&bps=4MiB&token=new".into(),
            request: None,
        })
        .await
        .unwrap();
        mock.resume(IdsParams {
            ids: vec![fail.clone()],
        })
        .await
        .unwrap();
        mock.advance(Duration::from_secs(5));
        for id in [fail, expire] {
            let s = mock.get(IdParams { id }).await.unwrap().summary;
            assert_eq!(s.state, DownloadState::Completed, "{s:?}");
            assert_eq!(s.error, None);
        }
    }

    #[tokio::test]
    async fn pause_resume_remove_and_errors() {
        let mock = manual();
        let id = mock
            .add(add("https://example.com/p.bin?size=64MiB&bps=8MiB"))
            .await
            .unwrap()
            .id;
        mock.advance(Duration::from_secs(1));
        mock.pause(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
        let before = mock.get(IdParams { id: id.clone() }).await.unwrap().summary;
        assert_eq!(before.state, DownloadState::Paused);
        assert_eq!(before.speed_bps, 0);
        mock.advance(Duration::from_secs(1));
        let after = mock.get(IdParams { id: id.clone() }).await.unwrap().summary;
        assert_eq!(after.downloaded_bytes, before.downloaded_bytes);

        let missing = mock
            .pause(IdsParams {
                ids: vec![id.clone(), "dl_nope".into()],
            })
            .await
            .unwrap_err();
        assert_eq!(missing.code, vayu_proto::error_code::NOT_FOUND);

        mock.resume(IdsParams {
            ids: vec![id.clone()],
        })
        .await
        .unwrap();
        mock.advance(Duration::from_secs(20));
        let done = mock.get(IdParams { id: id.clone() }).await.unwrap().summary;
        assert_eq!(done.state, DownloadState::Completed);
        let err = mock
            .resume(IdsParams {
                ids: vec![id.clone()],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, vayu_proto::error_code::INVALID_STATE);
        let err = mock
            .pause(IdsParams {
                ids: vec![id.clone()],
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, vayu_proto::error_code::INVALID_STATE);

        let mut events = mock.subscribe();
        mock.remove(RemoveParams {
            ids: vec![id.clone()],
            delete_files: true,
        })
        .await
        .unwrap();
        assert!(matches!(
            drain(&mut events).as_slice(),
            [Notification::DownloadRemoved(RemovedEvent { ids })] if ids == std::slice::from_ref(&id)
        ));
        assert_eq!(
            mock.get(IdParams { id }).await.unwrap_err().code,
            vayu_proto::error_code::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn respects_active_limit_speed_limit_and_validation() {
        let mock = manual();
        let mut settings = mock.settings().await.unwrap();
        settings.max_active_downloads = 1;
        settings.speed_limit_bps = Some(MIB);
        let mut events = mock.subscribe();
        mock.set_settings(settings.clone()).await.unwrap();
        assert!(matches!(
            drain(&mut events).as_slice(),
            [Notification::Settings(s)] if s == &settings
        ));
        for name in ["a", "b"] {
            mock.add(add(&format!(
                "https://example.com/{name}.bin?size=100MiB&bps=50MiB"
            )))
            .await
            .unwrap();
        }
        mock.advance(Duration::from_secs(3));
        let stats = mock.stats().await.unwrap();
        assert_eq!((stats.active, stats.queued), (1, 1));
        let total: u64 = mock
            .list(ListParams::default())
            .await
            .unwrap()
            .iter()
            .map(|s| s.downloaded_bytes)
            .sum();
        assert!(total <= 3 * MIB + MIB / 2, "speed limit ignored: {total}");

        let mut bad = settings.clone();
        bad.theme = "neon".into();
        assert_eq!(mock.set_settings(bad).await.unwrap_err().code, -32602);
        let mut bad = settings;
        bad.speed_limit_bps = Some(0);
        assert_eq!(mock.set_settings(bad).await.unwrap_err().code, -32602);
    }

    #[tokio::test]
    async fn probe_add_and_list() {
        let mock = manual();
        let err = mock
            .probe(ProbeParams {
                url: "https://example.com/404/x.zip".into(),
                request: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, vayu_proto::error_code::NETWORK);
        let err = mock.add(add("not a url")).await.unwrap_err();
        assert_eq!(err.code, -32602);
        let err = mock.add(add("ftp://example.com/x")).await.unwrap_err();
        assert_eq!(err.code, vayu_proto::error_code::UNSUPPORTED);
        let live = mock
            .probe(ProbeParams {
                url: "https://example.com/live/feed.ts".into(),
                request: None,
            })
            .await
            .unwrap();
        assert_eq!((live.total_bytes, live.resumable), (None, false));
        assert_eq!(live.filename, "feed.ts");

        let mut p = add("https://example.com/a/My%20File.PDF");
        p.directory = Some("relative/dir".into());
        assert_eq!(mock.add(p).await.unwrap_err().code, -32602);
        let mut p = add("https://example.com/a/My%20File.PDF");
        p.start = false;
        let first = mock.add(p.clone()).await.unwrap().id;
        let second = mock.add(p).await.unwrap().id;
        let list = mock.list(ListParams::default()).await.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, second, "newest first");
        assert_eq!(list[1].id, first);
        assert_eq!(list[1].filename, "My File.PDF");
        assert_eq!(list[0].filename, "My File (1).PDF");
        assert_eq!(list[0].category.as_deref(), Some("documents"));
        assert_eq!(list[0].state, DownloadState::Paused);
        let paged = mock
            .list(ListParams {
                offset: Some(1),
                limit: Some(5),
                ..ListParams::default()
            })
            .await
            .unwrap();
        assert_eq!(paged.len(), 1);
        assert_eq!(paged[0].id, first);
        let completed = mock
            .list(ListParams {
                states: vec![DownloadState::Completed],
                ..ListParams::default()
            })
            .await
            .unwrap();
        assert!(completed.is_empty());
    }

    #[tokio::test]
    async fn demo_scenario_has_every_state_and_shutdown_ends_streams() {
        let mock = MockService::new(MockOptions {
            auto_tick: false,
            ..MockOptions::default()
        });
        let list = mock.list(ListParams::default()).await.unwrap();
        for state in [
            DownloadState::Downloading,
            DownloadState::Paused,
            DownloadState::Completed,
            DownloadState::Failed,
            DownloadState::NeedsRefresh,
            DownloadState::Queued,
        ] {
            assert!(list.iter().any(|s| s.state == state), "{state:?} missing");
        }
        let mut events = mock.subscribe();
        mock.shutdown().await;
        // Active downloads were paused, then the stream ended.
        let mut paused = 0;
        while let Some(n) = events.next().await {
            if matches!(
                n,
                Notification::DownloadState(StateEvent {
                    state: DownloadState::Paused,
                    ..
                })
            ) {
                paused += 1;
            }
        }
        assert_eq!(paused, 2);
        assert!(mock.stats().await.is_err());
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_size("10MiB"), Some(10 * MIB));
        assert_eq!(parse_size("1.5k"), Some(1536));
        assert_eq!(parse_size("7"), Some(7));
        assert_eq!(parse_size("x"), None);
        assert_eq!(sanitize_filename("../../etc/passwd"), "_.._etc_passwd");
        assert_eq!(sanitize_filename("a<b>:c"), "a_b__c");
        assert_eq!(sanitize_filename("..."), "download");
        assert_eq!(category_for("x.tar.gz"), "compressed");
        assert_eq!(category_for("Movie.MKV"), "video");
        assert_eq!(category_for("README"), "other");
        assert_eq!(split_extension("a.tar.gz"), ("a", ".tar.gz"));
        assert_eq!(split_extension(".bashrc"), (".bashrc", ""));
        assert!(is_absolute("/x") && is_absolute("C:\\x") && !is_absolute("x/y"));
        assert_eq!(percent_decode("a%20b+c%zz"), "a b c%zz");
    }
}
