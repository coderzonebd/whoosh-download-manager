//! The engine: the download list, queues, settings, events, and the
//! [`Service`] implementation.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use futures::future::{BoxFuture, join_all};
use futures::stream::BoxStream;
use tokio::sync::watch;
use tokio::time::{Instant, MissedTickBehavior};
use vayu_core::{DownloadRecord, Journal, SourceFactory, StateStore, StorageProvider};
use vayu_proto::service::{Notification, Service, ServiceError, ServiceResult};
use vayu_proto::{
    AddParams, AddResult, DownloadDetail, DownloadId, DownloadState, DownloadSummary, ErrorInfo,
    IdParams, IdsParams, ListParams, PreconnectParams, ProbeParams, ProbeResult, ProgressEvent,
    ProgressItem, RefreshUrlParams, RemoveParams, RemovedEvent, SegmentInfo, Settings, StateEvent,
    Stats,
};

use crate::bus::Bus;
use crate::category::category_for;
use crate::checksum;
use crate::config::EngineConfig;
use crate::limiter::RateLimiter;
use crate::names;
use crate::run::{self, Outcome, RunSpec, StopReason};
use crate::util::{new_id, unix_millis};

/// The `StateStore` key under which settings are persisted.
pub const SETTINGS_KEY: &str = "settings";

/// The `StateStore` key holding per-download connection counts
/// (`AddParams::connections`) as a JSON object `{ id: count }`.
pub const CONNECTIONS_KEY: &str = "download_connections";

/// Bounds of `AddParams::connections` (PROTOCOL.md).
const CONNECTIONS_RANGE: std::ops::RangeInclusive<u32> = 1..=64;

/// What the engine is built on. Every part is a `vayu-core` trait object,
/// so the engine runs against `vayu-net`/`vayu-store` in the daemon and
/// against `vayu_core::testing` doubles in tests.
#[derive(Clone)]
pub struct EngineDeps {
    pub sources: Arc<dyn SourceFactory>,
    pub storage: Arc<dyn StorageProvider>,
    pub journal: Arc<dyn Journal>,
    pub state: Arc<dyn StateStore>,
}

impl fmt::Debug for EngineDeps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EngineDeps").finish_non_exhaustive()
    }
}

/// The download engine. Implements [`Service`]; see the crate docs.
pub struct Engine {
    inner: Arc<Inner>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Engine").finish_non_exhaustive()
    }
}

/// Live progress of a running download, written by its driver.
#[derive(Debug)]
pub(crate) struct Live {
    pub downloaded: AtomicU64,
    /// `u64::MAX` = unknown.
    pub total: AtomicU64,
    pub speed: AtomicU64,
    pub connections: AtomicU32,
    pub segments: Mutex<Vec<SegmentInfo>>,
}

impl Live {
    fn new(rec: &DownloadRecord) -> Self {
        Self {
            downloaded: AtomicU64::new(rec.downloaded_bytes),
            total: AtomicU64::new(rec.total_bytes.unwrap_or(u64::MAX)),
            speed: AtomicU64::new(0),
            connections: AtomicU32::new(0),
            segments: Mutex::new(Vec::new()),
        }
    }
}

struct RunHandle {
    stop: watch::Sender<Option<StopReason>>,
    done: watch::Receiver<bool>,
}

struct Entry {
    record: DownloadRecord,
    /// Insertion order, for stable FIFO and newest-first ordering.
    seq: u64,
    /// `AddParams::connections`, persisted under [`CONNECTIONS_KEY`].
    connections: Option<u16>,
    generation: u64,
    run: Option<RunHandle>,
    live: Option<Arc<Live>>,
    last_segments: Vec<SegmentInfo>,
    last_progress: Option<(u64, u64, u32)>,
    last_segments_sent: Option<Instant>,
    /// `remove` is in progress: never start this download again.
    removing: bool,
}

struct St {
    settings: Settings,
    downloads: HashMap<DownloadId, Entry>,
    seq: u64,
}

pub(crate) struct Inner {
    pub(crate) deps: EngineDeps,
    pub(crate) config: EngineConfig,
    pub(crate) limiter: Arc<RateLimiter>,
    bus: Bus,
    st: Mutex<St>,
    /// Serialises `StateStore` writes so the last write is always the
    /// latest record (and a removed record is never written back).
    save_lock: tokio::sync::Mutex<()>,
    max_conns: AtomicU32,
    shutting_down: AtomicBool,
    closed: AtomicBool,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn is_active(s: DownloadState) -> bool {
    matches!(
        s,
        DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying
    )
}

/// Forget what was captured for this download only: the cookie, auth-like
/// headers, a POST body (CSRF tokens, credentials) and the referer.
fn scrub(req: &mut vayu_proto::RequestContext) {
    req.cookie = None;
    req.body_base64 = None;
    req.referer = None;
    req.headers.retain(|h| !is_secret_header(&h.name));
}

/// Header names cleared from the stored request once a download completes.
fn is_secret_header(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n == "cookie"
        || n.contains("auth")
        || n.contains("token")
        || n.contains("api-key")
        || n.contains("apikey")
        || n.contains("session")
        || n.contains("secret")
        || n.contains("csrf")
}

impl Engine {
    /// Load settings and the download list from the `StateStore`, re-queue
    /// downloads that were interrupted (running when the process stopped),
    /// and start as many as the queues allow.
    pub async fn start(deps: EngineDeps, config: EngineConfig) -> io::Result<Arc<Engine>> {
        let settings = match deps.state.get_value(SETTINGS_KEY).await? {
            Some(json) => match serde_json::from_str::<Settings>(&json) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("stored settings unreadable, using defaults: {e}");
                    config.default_settings.clone()
                }
            },
            None => config.default_settings.clone(),
        };
        let pinned: HashMap<DownloadId, u16> = match deps.state.get_value(CONNECTIONS_KEY).await? {
            Some(json) => serde_json::from_str(&json).unwrap_or_else(|e| {
                tracing::warn!("stored connection counts unreadable: {e}");
                HashMap::new()
            }),
            None => HashMap::new(),
        };
        let mut records = deps.state.load_downloads().await?;
        records.sort_by(|a, b| a.added_at.cmp(&b.added_at).then(a.id.cmp(&b.id)));
        let mut requeued = Vec::new();
        let mut downloads = HashMap::new();
        for (seq, mut rec) in records.into_iter().enumerate() {
            if is_active(rec.state) {
                rec.state = DownloadState::Queued;
                requeued.push(rec.id.clone());
            }
            let connections = pinned.get(&rec.id).copied();
            downloads.insert(
                rec.id.clone(),
                Entry {
                    record: rec,
                    seq: seq as u64,
                    connections,
                    generation: 0,
                    run: None,
                    live: None,
                    last_segments: Vec::new(),
                    last_progress: None,
                    last_segments_sent: None,
                    removing: false,
                },
            );
        }
        let seq = downloads.len() as u64;
        let inner = Arc::new(Inner {
            limiter: Arc::new(RateLimiter::new(settings.speed_limit_bps)),
            max_conns: AtomicU32::new(settings.max_connections_per_download),
            st: Mutex::new(St {
                settings,
                downloads,
                seq,
            }),
            deps,
            config,
            bus: Bus::default(),
            save_lock: tokio::sync::Mutex::new(()),
            shutting_down: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        });
        for id in requeued {
            inner.save(&id).await;
        }
        tokio::spawn(ticker(Arc::downgrade(&inner)));
        inner.pump();
        Ok(Arc::new(Engine { inner }))
    }

    /// The download currently known by `id`, as stored (for tests and the
    /// daemon's diagnostics).
    pub fn record(&self, id: &str) -> Option<DownloadRecord> {
        self.inner.record(id)
    }

    /// Put a download in a named queue (`None` = the main queue). Named
    /// queues run one download at a time, oldest first; the main queue
    /// runs up to `max_active_downloads`. Not yet part of the API (see the
    /// README's contract proposals).
    pub async fn set_queue(&self, id: &str, queue: Option<String>) -> ServiceResult<()> {
        {
            let mut st = lock(&self.inner.st);
            let e = st
                .downloads
                .get_mut(id)
                .ok_or_else(|| ServiceError::not_found(id))?;
            e.record.queue = queue.filter(|q| !q.is_empty());
        }
        self.inner.save(id).await;
        self.inner.pump();
        Ok(())
    }
}

impl Inner {
    pub(crate) fn record(&self, id: &str) -> Option<DownloadRecord> {
        lock(&self.st).downloads.get(id).map(|e| e.record.clone())
    }

    pub(crate) fn max_connections(&self) -> u16 {
        self.max_conns
            .load(Ordering::Relaxed)
            .clamp(1, u32::from(u16::MAX)) as u16
    }

    /// Reserve a unique name for a running download (a probe improved
    /// it): computed and stored in the same critical section, so two
    /// downloads can never end up with the same in-progress file. Returns
    /// the name now in the record, or `None` if this run is stale.
    pub(crate) fn reserve_name(
        &self,
        id: &str,
        generation: u64,
        wanted: &str,
        category_auto: bool,
    ) -> Option<String> {
        let mut st = lock(&self.st);
        let e = st.downloads.get(id)?;
        if e.generation != generation {
            return None;
        }
        let dir = PathBuf::from(&e.record.directory);
        let name = unique_in(&st, &dir, wanted, id);
        let e = st.downloads.get_mut(id)?;
        if e.record.filename != name && category_auto {
            e.record.category = Some(category_for(&name).to_string());
        }
        e.record.filename = name.clone();
        Some(name)
    }

    /// Persist the per-download connection counts (small; rewritten whole).
    async fn save_connections(&self) {
        let _g = self.save_lock.lock().await;
        let map: HashMap<DownloadId, u16> = lock(&self.st)
            .downloads
            .iter()
            .filter_map(|(id, e)| e.connections.map(|n| (id.clone(), n)))
            .collect();
        let json = serde_json::to_string(&map).unwrap_or_else(|_| "{}".into());
        if let Err(e) = self.deps.state.set_value(CONNECTIONS_KEY, &json).await {
            tracing::warn!("saving connection counts failed: {e}");
        }
    }

    /// Persist the current record of `id` (no-op if it was removed).
    /// Returns whether it was saved.
    async fn save(&self, id: &str) -> bool {
        let _g = self.save_lock.lock().await;
        let Some(r) = self.record(id) else {
            return false;
        };
        match self.deps.state.save_download(&r).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(id, "saving download record failed: {e}");
                false
            }
        }
    }

    /// A running download changed state (only its current run may do so).
    pub(crate) async fn set_state(
        &self,
        id: &str,
        generation: u64,
        state: DownloadState,
        error: Option<ErrorInfo>,
    ) -> bool {
        {
            let mut st = lock(&self.st);
            let Some(e) = st.downloads.get_mut(id) else {
                return false;
            };
            if e.generation != generation {
                return false;
            }
            if e.record.state == state && e.record.error == error {
                return true;
            }
            e.record.state = state;
            e.record.error = error.clone();
        }
        self.bus.emit(Notification::DownloadState(StateEvent {
            id: id.to_string(),
            state,
            error,
        }));
        self.save(id).await;
        true
    }

    /// Change and persist a running download's record.
    pub(crate) async fn update(
        &self,
        id: &str,
        generation: u64,
        f: impl FnOnce(&mut DownloadRecord),
    ) {
        {
            let mut st = lock(&self.st);
            let Some(e) = st.downloads.get_mut(id) else {
                return;
            };
            if e.generation != generation {
                return;
            }
            f(&mut e.record);
        }
        self.save(id).await;
    }

    /// A run ended: apply its outcome, persist, notify, start what's next.
    pub(crate) async fn run_ended(
        self: &Arc<Self>,
        id: &str,
        generation: u64,
        outcome: Outcome,
        segments: Vec<SegmentInfo>,
    ) {
        let event = {
            let mut st = lock(&self.st);
            let Some(e) = st.downloads.get_mut(id) else {
                drop(st);
                self.pump();
                return;
            };
            if e.generation != generation {
                return;
            }
            // The latest stop request wins (a resume that arrived while the
            // run was stopping turned Pause into Restart). Read under the
            // lock that `resume`/`pause`/`refresh_url` also hold.
            let outcome = match (&outcome, e.run.as_ref().and_then(|r| *r.stop.borrow())) {
                (Outcome::Stopped(_), Some(latest)) => Outcome::Stopped(latest),
                _ => outcome,
            };
            e.run = None;
            e.last_segments = segments;
            if let Some(l) = &e.live {
                e.record.downloaded_bytes = l.downloaded.load(Ordering::Relaxed);
                l.speed.store(0, Ordering::Relaxed);
                l.connections.store(0, Ordering::Relaxed);
            }
            let (state, error) = match outcome {
                Outcome::Completed { filename, total } => {
                    e.record.filename = filename;
                    e.record.total_bytes = Some(total);
                    e.record.downloaded_bytes = total;
                    e.record.completed_at = Some(unix_millis());
                    // Secrets were captured for this download only.
                    if let Some(req) = e.record.request.as_mut() {
                        scrub(req);
                    }
                    (DownloadState::Completed, None)
                }
                Outcome::Failed(info) => (DownloadState::Failed, Some(info)),
                Outcome::NeedsRefresh(info) => (DownloadState::NeedsRefresh, Some(info)),
                Outcome::Stopped(StopReason::Pause) => (DownloadState::Paused, None),
                Outcome::Stopped(StopReason::Shutdown | StopReason::Restart) => {
                    (DownloadState::Queued, None)
                }
                Outcome::Stopped(StopReason::Remove) => return,
            };
            e.record.state = state;
            e.record.error = error.clone();
            let progress = ProgressItem {
                id: id.to_string(),
                downloaded_bytes: e.record.downloaded_bytes,
                total_bytes: e.record.total_bytes,
                speed_bps: 0,
                eta_secs: None,
                connections: 0,
                segments: None,
            };
            (
                progress,
                StateEvent {
                    id: id.to_string(),
                    state,
                    error,
                },
            )
        };
        let (progress, state) = event;
        let completed = state.state == DownloadState::Completed;
        self.bus.emit(Notification::DownloadProgress(ProgressEvent {
            items: vec![progress],
        }));
        self.bus.emit(Notification::DownloadState(state));
        let saved = self.save(id).await;
        if completed {
            // Only once the completed record is durable (saved, then synced
            // against power loss): until then the journal's finish intent is
            // what lets a restart recognise the file. A failed save or sync
            // keeps the entry; it is harmless next to a completed record.
            let id = id.to_string();
            if !saved {
                tracing::warn!(id, "completed record not saved; keeping its journal entry");
            } else if let Err(e) = self.deps.state.sync().await {
                tracing::warn!(id, "state sync failed, keeping the journal entry: {e}");
            } else if let Err(e) = self.deps.journal.remove(&id).await {
                tracing::warn!(id, "journal remove failed: {e}");
            }
        }
        self.pump();
    }

    /// Start queued downloads while the queues allow: the main queue up to
    /// `max_active_downloads`, each named queue one at a time; oldest first.
    fn pump(self: &Arc<Self>) {
        if self.shutting_down.load(Ordering::Acquire) {
            return;
        }
        let mut st = lock(&self.st);
        let max_active = st.settings.max_active_downloads.max(1) as usize;
        let mut active_main = 0usize;
        let mut busy_queues = HashSet::new();
        for e in st.downloads.values().filter(|e| e.run.is_some()) {
            match &e.record.queue {
                None => active_main += 1,
                Some(q) => {
                    busy_queues.insert(q.clone());
                }
            }
        }
        let mut queued: Vec<(i64, u64, DownloadId)> = st
            .downloads
            .values()
            .filter(|e| e.run.is_none() && !e.removing && e.record.state == DownloadState::Queued)
            .map(|e| (e.record.added_at, e.seq, e.record.id.clone()))
            .collect();
        queued.sort();
        for (_, _, id) in queued {
            let Some(e) = st.downloads.get_mut(&id) else {
                continue;
            };
            let start = match &e.record.queue {
                None if active_main < max_active => {
                    active_main += 1;
                    true
                }
                None => false,
                Some(q) => busy_queues.insert(q.clone()),
            };
            if !start {
                continue;
            }
            e.generation += 1;
            let live = Arc::new(Live::new(&e.record));
            e.live = Some(live.clone());
            e.last_progress = None;
            e.last_segments_sent = None;
            let (stop_tx, stop_rx) = watch::channel(None);
            let (done_tx, done_rx) = watch::channel(false);
            e.run = Some(RunHandle {
                stop: stop_tx,
                done: done_rx,
            });
            let spec = RunSpec {
                record: e.record.clone(),
                generation: e.generation,
                live,
                name_locked: e.record.filename_locked,
                connections: e.connections,
            };
            tokio::spawn(run::supervise(self.clone(), spec, stop_rx, done_tx));
        }
    }

    /// Ask a running download to stop; returns a receiver that turns true
    /// once its driver has flushed, committed and applied its final state.
    fn signal_stop(&self, id: &str, reason: StopReason) -> Option<watch::Receiver<bool>> {
        let st = lock(&self.st);
        let run = st.downloads.get(id)?.run.as_ref()?;
        run.stop.send_replace(Some(reason));
        Some(run.done.clone())
    }

    async fn emit_state_and_save(&self, id: &str) {
        let ev = lock(&self.st).downloads.get(id).map(|e| StateEvent {
            id: id.to_string(),
            state: e.record.state,
            error: e.record.error.clone(),
        });
        if let Some(ev) = ev {
            self.bus.emit(Notification::DownloadState(ev));
        }
        self.save(id).await;
    }

    fn summary(&self, e: &Entry) -> DownloadSummary {
        let r = &e.record;
        let running = e.run.is_some();
        let (downloaded, speed, connections, total) = match (&e.live, running) {
            (Some(l), true) => {
                let t = l.total.load(Ordering::Relaxed);
                (
                    l.downloaded.load(Ordering::Relaxed),
                    l.speed.load(Ordering::Relaxed),
                    l.connections.load(Ordering::Relaxed),
                    if t == u64::MAX {
                        r.total_bytes
                    } else {
                        Some(t)
                    },
                )
            }
            _ => (r.downloaded_bytes, 0, 0, r.total_bytes),
        };
        DownloadSummary {
            id: r.id.clone(),
            url: r.url.clone(),
            filename: r.filename.clone(),
            directory: r.directory.clone(),
            state: r.state,
            total_bytes: total,
            downloaded_bytes: downloaded,
            speed_bps: speed,
            eta_secs: eta(downloaded, total, speed),
            connections,
            resumable: r.resumable,
            category: r.category.clone(),
            queue: r.queue.clone(),
            added_at: r.added_at,
            completed_at: r.completed_at,
            error: r.error.clone(),
        }
    }

    fn emit_progress(&self) {
        let now = Instant::now();
        let every = self.config.tuning.segments_interval;
        let mut items = Vec::new();
        {
            let mut st = lock(&self.st);
            for e in st.downloads.values_mut() {
                if e.run.is_none() {
                    continue;
                }
                let Some(l) = e.live.clone() else { continue };
                let downloaded = l.downloaded.load(Ordering::Relaxed);
                let speed = l.speed.load(Ordering::Relaxed);
                let connections = l.connections.load(Ordering::Relaxed);
                let segments_due = e
                    .last_segments_sent
                    .is_none_or(|t| now.duration_since(t) >= every);
                let changed = e.last_progress != Some((downloaded, speed, connections));
                if !changed && !segments_due {
                    continue;
                }
                e.last_progress = Some((downloaded, speed, connections));
                let segments = if segments_due {
                    e.last_segments_sent = Some(now);
                    Some(
                        lock(&l.segments)
                            .iter()
                            .map(|s| (s.start, s.end, s.downloaded_bytes))
                            .collect(),
                    )
                } else {
                    None
                };
                let t = l.total.load(Ordering::Relaxed);
                let total = if t == u64::MAX {
                    e.record.total_bytes
                } else {
                    Some(t)
                };
                items.push(ProgressItem {
                    id: e.record.id.clone(),
                    downloaded_bytes: downloaded,
                    total_bytes: total,
                    speed_bps: speed,
                    eta_secs: eta(downloaded, total, speed),
                    connections,
                    segments,
                });
            }
        }
        if !items.is_empty() {
            items.sort_by(|a, b| a.id.cmp(&b.id));
            self.bus
                .emit(Notification::DownloadProgress(ProgressEvent { items }));
        }
    }

    fn stats(&self) -> Stats {
        let st = lock(&self.st);
        let mut s = Stats::default();
        for e in st.downloads.values() {
            if e.run.is_some() {
                s.active += 1;
                if let Some(l) = &e.live {
                    s.download_bps += l.speed.load(Ordering::Relaxed);
                }
            } else if e.record.state == DownloadState::Queued {
                s.queued += 1;
            }
        }
        s
    }

    fn check_open(&self) -> ServiceResult<()> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(ServiceError::invalid_state("the engine is shutting down"));
        }
        Ok(())
    }

    // ------------------------------------------------------- service ops

    async fn probe(&self, p: ProbeParams) -> ServiceResult<ProbeResult> {
        let src = self
            .deps
            .sources
            .source(&p.url, p.request.as_ref())
            .map_err(|e| ServiceError::invalid_params(e.to_string()))?;
        let meta = src
            .probe()
            .await
            .map_err(|e| ServiceError::network(e.to_string()))?;
        let final_url = meta.final_url.clone().unwrap_or_else(|| p.url.clone());
        Ok(ProbeResult {
            total_bytes: meta.size,
            resumable: meta.accepts_ranges && meta.size.is_some(),
            filename: names::resolve(
                None,
                meta.filename.as_deref(),
                &final_url,
                meta.content_type.as_deref(),
            ),
            content_type: meta.content_type,
            final_url,
        })
    }

    async fn add(self: &Arc<Self>, p: AddParams) -> ServiceResult<AddResult> {
        self.check_open()?;
        if p.url.trim().is_empty() {
            return Err(ServiceError::invalid_params("url is empty"));
        }
        self.deps
            .sources
            .source(&p.url, p.request.as_ref())
            .map_err(|e| ServiceError::invalid_params(e.to_string()))?;
        let user_name = match &p.filename {
            Some(f) => Some(
                names::sanitize(f)
                    .ok_or_else(|| ServiceError::invalid_params("filename is not usable"))?,
            ),
            None => None,
        };
        if let Some(c) = &p.checksum {
            checksum::parse(c).map_err(ServiceError::invalid_params)?;
        }
        let directory = match &p.directory {
            Some(d) => d.clone(),
            None => lock(&self.st).settings.default_directory.clone(),
        };
        if !Path::new(&directory).is_absolute() {
            return Err(ServiceError::invalid_params("directory must be absolute"));
        }
        let connections = match p.connections {
            Some(n) if CONNECTIONS_RANGE.contains(&n) => Some(n as u16),
            Some(n) => {
                return Err(ServiceError::invalid_params(format!(
                    "connections must be between 1 and 64, not {n}"
                )));
            }
            None => None,
        };
        let queue = p.queue.clone().filter(|q| !q.trim().is_empty());
        let name_locked = user_name.is_some();
        let name = names::resolve(user_name.as_deref(), None, &p.url, None);
        let id = new_id();
        if names::part_path(Path::new(&directory), &name).is_none() {
            return Err(ServiceError::invalid_params("filename is not usable"));
        }
        let user_category = p.category.clone().filter(|c| !c.trim().is_empty());
        let mut record = DownloadRecord {
            id: id.clone(),
            url: p.url,
            request: p.request,
            mirrors: p.mirrors,
            filename: name,
            filename_locked: name_locked,
            directory,
            category: None,
            queue,
            state: if p.start {
                DownloadState::Queued
            } else {
                DownloadState::Paused
            },
            total_bytes: None,
            downloaded_bytes: 0,
            resumable: false,
            checksum: p.checksum,
            added_at: unix_millis(),
            completed_at: None,
            error: None,
        };
        let summary = {
            let mut st = lock(&self.st);
            // Pick the name and store it in one critical section.
            record.filename = unique_in(&st, Path::new(&record.directory), &record.filename, &id);
            record.category =
                Some(user_category.unwrap_or_else(|| category_for(&record.filename).to_string()));
            let seq = st.seq;
            st.seq += 1;
            let entry = Entry {
                record,
                seq,
                connections,
                generation: 0,
                run: None,
                live: None,
                last_segments: Vec::new(),
                last_progress: None,
                last_segments_sent: None,
                removing: false,
            };
            let summary = self.summary(&entry);
            st.downloads.insert(id.clone(), entry);
            summary
        };
        self.save(&id).await;
        if connections.is_some() {
            self.save_connections().await;
        }
        self.bus.emit(Notification::DownloadAdded(summary));
        self.pump();
        Ok(AddResult { id })
    }

    fn list(&self, p: ListParams) -> Vec<DownloadSummary> {
        let st = lock(&self.st);
        let mut v: Vec<&Entry> = st
            .downloads
            .values()
            .filter(|e| p.states.is_empty() || p.states.contains(&e.record.state))
            .filter(|e| {
                p.category
                    .as_ref()
                    .is_none_or(|c| e.record.category.as_ref() == Some(c))
            })
            .collect();
        v.sort_by(|a, b| {
            b.record
                .added_at
                .cmp(&a.record.added_at)
                .then(b.seq.cmp(&a.seq))
        });
        let offset = p.offset.unwrap_or(0) as usize;
        let limit = p.limit.map_or(usize::MAX, |l| l as usize);
        v.into_iter()
            .skip(offset)
            .take(limit)
            .map(|e| self.summary(e))
            .collect()
    }

    fn get(&self, id: &str) -> ServiceResult<DownloadDetail> {
        let st = lock(&self.st);
        let e = st
            .downloads
            .get(id)
            .ok_or_else(|| ServiceError::not_found(id))?;
        let segments = match (&e.live, e.run.is_some()) {
            (Some(l), true) => lock(&l.segments).clone(),
            _ => e.last_segments.clone(),
        };
        Ok(DownloadDetail {
            summary: self.summary(e),
            segments,
        })
    }

    fn validate_ids(&self, ids: &[DownloadId]) -> ServiceResult<()> {
        let st = lock(&self.st);
        for id in ids {
            if !st.downloads.contains_key(id) {
                return Err(ServiceError::not_found(id));
            }
        }
        Ok(())
    }

    async fn pause(self: &Arc<Self>, ids: Vec<DownloadId>) -> ServiceResult<()> {
        self.validate_ids(&ids)?;
        {
            let st = lock(&self.st);
            for id in &ids {
                let s = st.downloads[id].record.state;
                if matches!(s, DownloadState::Completed | DownloadState::Failed) {
                    return Err(ServiceError::invalid_state(format!(
                        "cannot pause a download that is {s:?}"
                    )));
                }
            }
        }
        let mut waits = Vec::new();
        let mut changed = Vec::new();
        {
            // One critical section: pump cannot start a download between
            // "not running" and "paused".
            let mut st = lock(&self.st);
            for id in &ids {
                let Some(e) = st.downloads.get_mut(id) else {
                    continue;
                };
                if let Some(run) = &e.run {
                    run.stop.send_replace(Some(StopReason::Pause));
                    waits.push(run.done.clone());
                } else if matches!(
                    e.record.state,
                    DownloadState::Queued | DownloadState::NeedsRefresh
                ) {
                    e.record.state = DownloadState::Paused;
                    e.record.error = None;
                    changed.push(id.clone());
                }
            }
        }
        for id in &changed {
            self.emit_state_and_save(id).await;
        }
        wait_all(waits).await;
        Ok(())
    }

    async fn resume(self: &Arc<Self>, ids: Vec<DownloadId>) -> ServiceResult<()> {
        self.check_open()?;
        self.validate_ids(&ids)?;
        let mut changed = Vec::new();
        {
            let mut st = lock(&self.st);
            for id in &ids {
                if st.downloads[id].record.state == DownloadState::Completed {
                    return Err(ServiceError::invalid_state(
                        "cannot resume a completed download",
                    ));
                }
            }
            for id in &ids {
                let e = st.downloads.get_mut(id).expect("validated");
                // A pause still flushing: let the run restart instead.
                if let Some(run) = &e.run
                    && *run.stop.borrow() == Some(StopReason::Pause)
                {
                    run.stop.send_replace(Some(StopReason::Restart));
                    continue;
                }
                if e.run.is_none()
                    && matches!(
                        e.record.state,
                        DownloadState::Paused | DownloadState::Failed | DownloadState::NeedsRefresh
                    )
                {
                    e.record.state = DownloadState::Queued;
                    e.record.error = None;
                    changed.push(id.clone());
                }
            }
        }
        for id in &changed {
            self.emit_state_and_save(id).await;
        }
        self.pump();
        Ok(())
    }

    async fn remove(self: &Arc<Self>, p: RemoveParams) -> ServiceResult<()> {
        self.validate_ids(&p.ids)?;
        let waits: Vec<_> = {
            // Mark and stop in one critical section, so pump can never
            // start a download that is being removed.
            let mut st = lock(&self.st);
            p.ids
                .iter()
                .filter_map(|id| {
                    let e = st.downloads.get_mut(id)?;
                    e.removing = true;
                    let run = e.run.as_ref()?;
                    run.stop.send_replace(Some(StopReason::Remove));
                    Some(run.done.clone())
                })
                .collect()
        };
        wait_all(waits).await;
        let removed: Vec<DownloadRecord> = {
            let _g = self.save_lock.lock().await;
            let removed: Vec<DownloadRecord> = {
                let mut st = lock(&self.st);
                p.ids
                    .iter()
                    .filter_map(|id| st.downloads.remove(id).map(|e| e.record))
                    .collect()
            };
            let ids: Vec<DownloadId> = removed.iter().map(|r| r.id.clone()).collect();
            if let Err(e) = self.deps.state.delete_downloads(&ids).await {
                tracing::warn!("deleting download records failed: {e}");
            }
            removed
        };
        for r in &removed {
            if let Err(e) = self.deps.journal.remove(&r.id).await {
                tracing::warn!(id = r.id, "journal remove failed: {e}");
            }
            if p.delete_files {
                let dir = PathBuf::from(&r.directory);
                let mut paths = Vec::new();
                if let Some(part) = names::part_path(&dir, &r.filename) {
                    paths.push(part);
                }
                if r.state == DownloadState::Completed
                    && let Some(f) = names::inside(&dir, &r.filename)
                {
                    paths.push(f);
                }
                for path in paths {
                    if let Err(e) = self.deps.storage.remove(&path).await {
                        tracing::warn!(id = r.id, "deleting {} failed: {e}", path.display());
                    }
                }
            }
        }
        if !removed.is_empty() {
            self.save_connections().await;
            self.bus.emit(Notification::DownloadRemoved(RemovedEvent {
                ids: removed.into_iter().map(|r| r.id).collect(),
            }));
        }
        self.pump();
        Ok(())
    }

    async fn refresh_url(self: &Arc<Self>, p: RefreshUrlParams) -> ServiceResult<()> {
        self.check_open()?;
        self.validate_ids(std::slice::from_ref(&p.id))?;
        let state = lock(&self.st).downloads[&p.id].record.state;
        if matches!(state, DownloadState::Completed | DownloadState::Verifying) {
            return Err(ServiceError::invalid_state(format!(
                "cannot refresh the URL of a download that is {state:?}"
            )));
        }
        if p.url.trim().is_empty() {
            return Err(ServiceError::invalid_params("url is empty"));
        }
        self.deps
            .sources
            .source(&p.url, p.request.as_ref())
            .map_err(|e| ServiceError::invalid_params(e.to_string()))?;
        // Store the new URL first, then stop a running download: its
        // restart is spawned from the record, so it must already hold it.
        let (wait, state_changed) = {
            let mut st = lock(&self.st);
            let Some(e) = st.downloads.get_mut(&p.id) else {
                return Err(ServiceError::not_found(&p.id));
            };
            e.record.url = p.url;
            if p.request.is_some() {
                e.record.request = p.request;
            }
            if let Some(run) = &e.run {
                run.stop.send_replace(Some(StopReason::Restart));
                (Some(run.done.clone()), false)
            } else if matches!(
                e.record.state,
                DownloadState::NeedsRefresh | DownloadState::Failed
            ) {
                e.record.state = DownloadState::Queued;
                e.record.error = None;
                (None, true)
            } else {
                (None, false)
            }
        };
        if state_changed {
            self.emit_state_and_save(&p.id).await;
        } else {
            self.save(&p.id).await;
        }
        if let Some(rx) = wait {
            wait_all(vec![rx]).await;
        }
        self.pump();
        Ok(())
    }

    async fn set_settings(self: &Arc<Self>, mut s: Settings) -> ServiceResult<Settings> {
        if !Path::new(&s.default_directory).is_absolute() {
            return Err(ServiceError::invalid_params(
                "defaultDirectory must be absolute",
            ));
        }
        if s.max_active_downloads == 0 || s.max_active_downloads > 1000 {
            return Err(ServiceError::invalid_params(
                "maxActiveDownloads must be between 1 and 1000",
            ));
        }
        if s.max_connections_per_download == 0 || s.max_connections_per_download > 256 {
            return Err(ServiceError::invalid_params(
                "maxConnectionsPerDownload must be between 1 and 256",
            ));
        }
        if !matches!(s.theme.as_str(), "system" | "light" | "dark") {
            return Err(ServiceError::invalid_params(
                "theme must be system, light or dark",
            ));
        }
        if s.language.trim().is_empty() {
            return Err(ServiceError::invalid_params("language is empty"));
        }
        if s.speed_limit_bps == Some(0) {
            s.speed_limit_bps = None;
        }
        let json = serde_json::to_string(&s).map_err(|e| ServiceError::io(e.to_string()))?;
        self.deps
            .state
            .set_value(SETTINGS_KEY, &json)
            .await
            .map_err(|e| ServiceError::io(e.to_string()))?;
        lock(&self.st).settings = s.clone();
        self.limiter.set_rate(s.speed_limit_bps);
        self.max_conns
            .store(s.max_connections_per_download, Ordering::Relaxed);
        self.bus.emit(Notification::Settings(s.clone()));
        self.pump();
        Ok(s)
    }

    async fn shutdown(self: &Arc<Self>) {
        self.shutting_down.store(true, Ordering::Release);
        let ids: Vec<DownloadId> = lock(&self.st)
            .downloads
            .values()
            .filter(|e| e.run.is_some())
            .map(|e| e.record.id.clone())
            .collect();
        let waits: Vec<_> = ids
            .iter()
            .filter_map(|id| self.signal_stop(id, StopReason::Shutdown))
            .collect();
        wait_all(waits).await;
        // Persist every record once more (cheap, and covers anything a
        // failed save left behind).
        let all: Vec<DownloadId> = lock(&self.st).downloads.keys().cloned().collect();
        for id in all {
            self.save(&id).await;
        }
        self.closed.store(true, Ordering::Release);
        self.bus.close();
    }
}

/// `name`, or `name (n).ext`, so no other unfinished download in `dir`
/// uses the same in-progress file. Callers hold the state lock until the
/// name is stored.
fn unique_in(st: &St, dir: &Path, name: &str, except: &str) -> String {
    let taken: HashSet<String> = st
        .downloads
        .values()
        .filter(|e| e.record.id != except && e.record.state != DownloadState::Completed)
        .filter(|e| Path::new(&e.record.directory) == dir)
        .map(|e| e.record.filename.to_lowercase())
        .collect();
    if !taken.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    (1..)
        .map(|n| names::numbered(name, n))
        .find(|c| !taken.contains(&c.to_lowercase()))
        .unwrap_or_else(|| name.to_string())
}

fn eta(downloaded: u64, total: Option<u64>, speed: u64) -> Option<u64> {
    let total = total?;
    (speed > 0 && total >= downloaded).then(|| (total - downloaded).div_ceil(speed))
}

async fn wait_all(waits: Vec<watch::Receiver<bool>>) {
    join_all(waits.into_iter().map(|mut rx| async move {
        let _ = rx.wait_for(|d| *d).await;
    }))
    .await;
}

/// Batched progress (≤ 4/s) and stats (1/s) for every subscriber.
async fn ticker(weak: Weak<Inner>) {
    let (progress_every, stats_every) = match weak.upgrade() {
        Some(i) => (
            i.config.tuning.progress_interval,
            i.config.tuning.stats_interval,
        ),
        None => return,
    };
    let mut tick = tokio::time::interval(progress_every);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_stats = Instant::now();
    loop {
        tick.tick().await;
        let Some(inner) = weak.upgrade() else { return };
        if inner.closed.load(Ordering::Acquire) {
            return;
        }
        inner.emit_progress();
        let now = Instant::now();
        if now.duration_since(last_stats) >= stats_every {
            last_stats = now;
            let s = inner.stats();
            inner.bus.emit(Notification::Stats(s));
        }
    }
}

impl Service for Engine {
    fn capabilities(&self) -> Vec<String> {
        self.inner.config.capabilities.clone()
    }

    fn probe(&self, params: ProbeParams) -> BoxFuture<'_, ServiceResult<ProbeResult>> {
        Box::pin(self.inner.probe(params))
    }

    fn add(&self, params: AddParams) -> BoxFuture<'_, ServiceResult<AddResult>> {
        Box::pin(self.inner.add(params))
    }

    fn list(&self, params: ListParams) -> BoxFuture<'_, ServiceResult<Vec<DownloadSummary>>> {
        let v = self.inner.list(params);
        Box::pin(async move { Ok(v) })
    }

    fn get(&self, params: IdParams) -> BoxFuture<'_, ServiceResult<DownloadDetail>> {
        let v = self.inner.get(&params.id);
        Box::pin(async move { v })
    }

    fn pause(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.inner.pause(params.ids))
    }

    fn resume(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.inner.resume(params.ids))
    }

    fn remove(&self, params: RemoveParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.inner.remove(params))
    }

    fn refresh_url(&self, params: RefreshUrlParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.inner.refresh_url(params))
    }

    fn preconnect(&self, params: PreconnectParams) -> BoxFuture<'_, ServiceResult<()>> {
        let sources = self.inner.deps.sources.clone();
        Box::pin(async move {
            if params.url.trim().is_empty() {
                return Err(ServiceError::invalid_params("url is empty"));
            }
            tokio::spawn(async move { sources.preconnect(&params.url).await });
            Ok(())
        })
    }

    fn stats(&self) -> BoxFuture<'_, ServiceResult<Stats>> {
        let s = self.inner.stats();
        Box::pin(async move { Ok(s) })
    }

    fn settings(&self) -> BoxFuture<'_, ServiceResult<Settings>> {
        let s = lock(&self.inner.st).settings.clone();
        Box::pin(async move { Ok(s) })
    }

    fn set_settings(&self, settings: Settings) -> BoxFuture<'_, ServiceResult<Settings>> {
        Box::pin(self.inner.set_settings(settings))
    }

    fn subscribe(&self) -> BoxStream<'static, Notification> {
        Box::pin(self.inner.bus.subscribe())
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.inner.shutdown())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_headers() {
        for h in [
            "Authorization",
            "Proxy-Authorization",
            "Cookie",
            "X-Auth-Token",
            "X-Api-Key",
            "X-CSRF-Token",
            "X-Session-Id",
        ] {
            assert!(is_secret_header(h), "{h}");
        }
        for h in ["Accept", "Referer", "User-Agent", "Range"] {
            assert!(!is_secret_header(h), "{h}");
        }
    }

    #[test]
    fn etas() {
        assert_eq!(eta(0, Some(100), 10), Some(10));
        assert_eq!(eta(95, Some(100), 10), Some(1));
        assert_eq!(eta(0, None, 10), None);
        assert_eq!(eta(0, Some(100), 0), None);
    }
}
