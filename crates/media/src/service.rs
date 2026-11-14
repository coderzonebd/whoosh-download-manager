//! [`MediaService`]: the engine plus media downloads.
//!
//! A media download is one entry for clients (`md_…`) made of hidden
//! engine downloads (the parts: a video and an audio format, or one
//! format) that are merged with ffmpeg once they finish. Parts are normal
//! engine downloads, so they get the engine's speed, crash safety, queues,
//! the scheduler and speed limits. Formats the engine cannot fetch (HLS,
//! DASH fragments, other streaming protocols) are downloaded by yt-dlp
//! itself instead.
//!
//! All engine events pass through one pump that hides the parts and turns
//! their progress and state into the media download's.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::StreamExt;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tokio::task::AbortHandle;
use whoosh_proto::service::{Notification, Service, ServiceError, ServiceResult};
use whoosh_proto::{
    AddParams, AddResult, DownloadDetail, DownloadState, DownloadSummary, ErrorInfo, IdParams,
    IdsParams, ListParams, MEDIA_CAPABILITY, MediaAddParams, MediaChoice, MediaConfigureParams,
    MediaDownloadInfo, MediaFormatKind, MediaProbeParams, MediaProbeResult, MediaStep,
    MediaToolsStatus, MediaUpdateParams, MediaVia, PreconnectParams, ProbeParams, ProbeResult,
    ProgressEvent, ProgressItem, Queue, QueueConfig, QueueNameParams, QueueRenameParams,
    RefreshUrlParams, RemoveParams, RemovedEvent, RequestContext, SegmentInfo, SetQueueParams,
    Settings, StateEvent, Stats, error_code,
};

use crate::fetch::{Fetcher, NetFetcher};
use crate::info::{self, Finish};
use crate::jobs::{self, Job, JobPart, Phase, SavedFinish};
use crate::process::Tool;
use crate::tools::{Kind, Tools, ToolsConfig};
use crate::upstream::Upstreams;
use crate::util::{now_ms, random_hex};
use crate::ytdlp::{self, Dirs, FallbackEnd, FallbackRun};
use crate::{ffmpeg, names};

/// How long a probe's answer is reused by `media.add`.
const PROBE_CACHE: Duration = Duration::from_secs(15 * 60);
const PROBE_CACHE_ENTRIES: usize = 16;
/// yt-dlp downloads running at once (they do not use the engine's queues'
/// slots).
const MAX_FALLBACK_RUNS: usize = 2;
/// Automatic link refreshes per media download (per daemon run).
const MAX_AUTO_REFRESHES: u32 = 3;
const MAX_SUBTITLE_LANGS: usize = 20;

fn lock<T>(m: &StdMutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// How [`MediaService`] is set up.
#[derive(Debug, Clone)]
pub struct MediaConfig {
    /// `<data dir>/media`: tools, jobs, caches.
    pub dir: PathBuf,
    pub upstreams: Upstreams,
    /// Use the system's yt-dlp/ffmpeg when there is no managed copy
    /// (Linux; ffmpeg on macOS).
    pub system_tools: bool,
    /// Fixed tools (tests): never found or fetched.
    pub yt_dlp: Option<Tool>,
    pub ffmpeg: Option<Tool>,
    /// The network for fetching tools; whoosh-net by default.
    pub fetcher: Option<Arc<dyn Fetcher>>,
    /// Run the daily update check when the user enabled it.
    pub auto_update: bool,
}

impl MediaConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            upstreams: Upstreams::default(),
            system_tools: true,
            yt_dlp: None,
            ffmpeg: None,
            fetcher: None,
            auto_update: true,
        }
    }
}

/// What the pump knows about a part, from the engine's events.
#[derive(Debug, Clone, Default)]
struct PartLive {
    state: Option<DownloadState>,
    downloaded: u64,
    total: Option<u64>,
    speed: u64,
    connections: u32,
    segments: Option<Vec<(u64, u64, u64)>>,
    error: Option<ErrorInfo>,
    resumable: bool,
}

/// What is not saved about a job.
#[derive(Debug, Default)]
struct JobLive {
    step: Option<MediaStep>,
    /// yt-dlp's progress (fallback downloads).
    fallback: ytdlp::FallbackProgress,
    /// A yt-dlp download runs; dropping the sender stops it.
    child: Option<oneshot::Sender<()>>,
    /// Which run `child` belongs to: a stopped run that ends late must not
    /// clear the handle of the one started after it.
    run: u64,
    /// Preparing, finishing, refreshing.
    task: Option<AbortHandle>,
    refreshes: u32,
    refreshing: bool,
    last_progress: Option<Instant>,
}

#[derive(Debug, Default)]
struct State {
    jobs: BTreeMap<String, Job>,
    live: HashMap<String, JobLive>,
    /// Part id → job id.
    part_of: HashMap<String, String>,
    parts: HashMap<String, PartLive>,
    /// Queue name → running (from `event.queues`).
    queues_running: HashMap<String, bool>,
}

struct Shared {
    inner: Arc<dyn Service>,
    tools: Arc<Tools>,
    dir: PathBuf,
    dirs: Dirs,
    state: Mutex<State>,
    save_lock: Mutex<()>,
    subs: StdMutex<Vec<UnboundedSender<Notification>>>,
    probes: StdMutex<HashMap<String, (Instant, Arc<Value>)>>,
    /// The page request (cookie, referer, user agent) per job, in memory
    /// only: secrets are never written down.
    requests: StdMutex<HashMap<String, RequestContext>>,
    closed: AtomicBool,
}

/// The engine (or any [`Service`]) with media downloads on top.
pub struct MediaService {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for MediaService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaService")
            .field("dir", &self.shared.dir)
            .finish()
    }
}

type Fail = (&'static str, String);

impl MediaService {
    /// Wraps `inner`: loads the saved media downloads, finds the tools,
    /// resumes what was interrupted.
    pub async fn start(inner: Arc<dyn Service>, cfg: MediaConfig) -> std::io::Result<Arc<Self>> {
        tokio::fs::create_dir_all(cfg.dir.join("jobs")).await?;
        let fetcher = cfg.fetcher.clone().unwrap_or_else(|| {
            Arc::new(NetFetcher::new(whoosh_net::HttpSourceFactory::new(
                whoosh_net::NetConfig::default(),
            )))
        });
        let tools = Tools::open(
            ToolsConfig {
                dir: cfg.dir.clone(),
                upstreams: cfg.upstreams.clone(),
                system: cfg.system_tools,
                yt_dlp: cfg.yt_dlp.clone(),
                ffmpeg: cfg.ffmpeg.clone(),
            },
            fetcher,
        )
        .await;
        let mut jobs = jobs::load(&cfg.dir.join("jobs.json")).await;
        for job in jobs.values_mut() {
            job.error = job.error.take().filter(|_| job.phase == Phase::Failed);
            // A merge or rename interrupted by a crash starts over once
            // the parts are seen complete.
            if job.phase == Phase::Finishing {
                job.phase = Phase::Downloading;
            }
        }
        let mut st = State::default();
        for job in jobs.values() {
            for p in &job.parts {
                st.part_of.insert(p.id.clone(), job.id.clone());
            }
            st.live.insert(job.id.clone(), JobLive::default());
        }
        st.jobs = jobs;
        let shared = Arc::new(Shared {
            inner,
            tools,
            dirs: Dirs {
                cache: cfg.dir.join("cache"),
                tmp: cfg.dir.join("tmp"),
            },
            dir: cfg.dir.clone(),
            state: Mutex::new(st),
            save_lock: Mutex::new(()),
            subs: StdMutex::new(Vec::new()),
            probes: StdMutex::new(HashMap::new()),
            requests: StdMutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        let _ = tokio::fs::remove_dir_all(&shared.dirs.tmp).await;
        {
            let weak = Arc::downgrade(&shared);
            shared.tools.set_listener(move |status| {
                if let Some(s) = weak.upgrade() {
                    s.emit(Notification::MediaTools(status));
                }
            });
        }
        // Subscribe before reading the parts' state, so nothing is missed.
        let events = shared.inner.subscribe();
        if let Ok(all) = shared.inner.list(ListParams::default()).await {
            let mut st = shared.state.lock().await;
            for s in all {
                if st.part_of.contains_key(&s.id) {
                    let live = st.parts.entry(s.id.clone()).or_default();
                    absorb_summary(live, &s);
                }
            }
        }
        if let Ok(queues) = shared.inner.queues().await {
            let mut st = shared.state.lock().await;
            st.queues_running = queues.into_iter().map(|q| (q.name, q.running)).collect();
        }
        tokio::spawn(pump(Arc::downgrade(&shared), events));
        shared.resume_all().await;
        if cfg.auto_update {
            let weak = Arc::downgrade(&shared);
            tokio::spawn(async move {
                loop {
                    let Some(s) = weak.upgrade() else { return };
                    if s.closed.load(Ordering::Acquire) {
                        return;
                    }
                    s.tools.auto_update_if_due().await;
                    drop(s);
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                }
            });
        }
        Ok(Arc::new(Self { shared }))
    }
}

/// Engine events → client events.
async fn pump(weak: std::sync::Weak<Shared>, mut events: BoxStream<'static, Notification>) {
    while let Some(n) = events.next().await {
        let Some(shared) = weak.upgrade() else { return };
        let (out, actions) = {
            let mut st = shared.state.lock().await;
            translate(&mut st, n)
        };
        for n in out {
            shared.emit(n);
        }
        for a in actions {
            shared.act(a);
        }
    }
    if let Some(shared) = weak.upgrade() {
        lock(&shared.subs).clear();
    }
}

#[derive(Debug)]
enum Action {
    Finish(String),
    Refresh(String),
    Fallbacks,
}

fn absorb_summary(live: &mut PartLive, s: &DownloadSummary) {
    live.state = Some(s.state);
    live.downloaded = s.downloaded_bytes;
    live.total = s.total_bytes;
    live.speed = s.speed_bps;
    live.connections = s.connections;
    live.error = s.error.clone();
    live.resumable = s.resumable;
}

/// The media download's state from its parts' (engine path).
fn parts_state(job: &Job, st: &State) -> (DownloadState, Option<ErrorInfo>) {
    let states: Vec<(DownloadState, Option<&ErrorInfo>)> = job
        .parts
        .iter()
        .map(|p| {
            st.parts
                .get(&p.id)
                .map_or((DownloadState::Queued, None), |l| {
                    (l.state.unwrap_or(DownloadState::Queued), l.error.as_ref())
                })
        })
        .collect();
    let any = |s: DownloadState| states.iter().any(|(x, _)| *x == s);
    if let Some((_, e)) = states.iter().find(|(s, _)| *s == DownloadState::Failed) {
        return (DownloadState::Failed, e.cloned());
    }
    if any(DownloadState::NeedsRefresh) {
        return (DownloadState::NeedsRefresh, None);
    }
    if !states.is_empty() && states.iter().all(|(s, _)| *s == DownloadState::Completed) {
        return (DownloadState::Verifying, None);
    }
    if any(DownloadState::Downloading) || any(DownloadState::Verifying) {
        return (DownloadState::Downloading, None);
    }
    if any(DownloadState::Probing) {
        return (DownloadState::Probing, None);
    }
    if any(DownloadState::Queued) {
        return (DownloadState::Queued, None);
    }
    (DownloadState::Paused, None)
}

fn job_state(job: &Job, st: &State) -> (DownloadState, Option<ErrorInfo>) {
    let live = st.live.get(&job.id);
    match job.phase {
        Phase::Preparing if job.paused => (DownloadState::Paused, None),
        Phase::Preparing => (DownloadState::Probing, None),
        Phase::Downloading => match job.via {
            MediaVia::Engine => {
                let (s, e) = parts_state(job, st);
                if live.is_some_and(|l| l.refreshing) && s == DownloadState::NeedsRefresh {
                    (DownloadState::Probing, None)
                } else {
                    (s, e)
                }
            }
            MediaVia::YtDlp if job.paused => (DownloadState::Paused, None),
            MediaVia::YtDlp if live.is_some_and(|l| l.child.is_some()) => {
                (DownloadState::Downloading, None)
            }
            MediaVia::YtDlp => (DownloadState::Queued, None),
        },
        Phase::Finishing => (DownloadState::Verifying, None),
        Phase::Done => (DownloadState::Completed, None),
        Phase::Failed => (DownloadState::Failed, job.error.clone()),
    }
}

/// `(downloaded, total, speed, connections)`
fn job_bytes(job: &Job, st: &State) -> (u64, Option<u64>, u64, u32) {
    match (job.phase, job.via) {
        (Phase::Downloading | Phase::Finishing | Phase::Failed, MediaVia::Engine)
            if !job.parts.is_empty() =>
        {
            let mut downloaded = 0;
            let mut total = Some(0u64);
            let (mut speed, mut conns) = (0, 0);
            for p in &job.parts {
                let l = st.parts.get(&p.id).cloned().unwrap_or_default();
                downloaded += l.downloaded;
                total = total.and_then(|t| l.total.map(|x| t + x));
                if job.phase == Phase::Downloading {
                    speed += l.speed;
                    conns += l.connections;
                }
            }
            (downloaded, total.or(job.total_bytes), speed, conns)
        }
        (Phase::Downloading, MediaVia::YtDlp) => {
            let f = st.live.get(&job.id).map(|l| l.fallback).unwrap_or_default();
            let running = st.live.get(&job.id).is_some_and(|l| l.child.is_some());
            (
                f.downloaded,
                f.total.or(job.total_bytes),
                if running { f.speed } else { 0 },
                u32::from(running),
            )
        }
        _ => (job.downloaded_bytes, job.total_bytes, 0, 0),
    }
}

fn eta(downloaded: u64, total: Option<u64>, speed: u64) -> Option<u64> {
    let total = total?;
    (speed > 0 && total >= downloaded).then(|| (total - downloaded).div_ceil(speed))
}

fn summary(job: &Job, st: &State) -> DownloadSummary {
    let (state, error) = job_state(job, st);
    let (downloaded, total, speed, connections) = job_bytes(job, st);
    let active = matches!(
        state,
        DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying
    );
    let resumable = match job.via {
        MediaVia::Engine => job
            .parts
            .iter()
            .all(|p| st.parts.get(&p.id).is_none_or(|l| l.resumable)),
        MediaVia::YtDlp => false,
    };
    DownloadSummary {
        id: job.id.clone(),
        url: job.url.clone(),
        filename: job.filename.clone(),
        directory: job.directory.clone(),
        state,
        total_bytes: total,
        downloaded_bytes: downloaded,
        speed_bps: speed,
        eta_secs: eta(downloaded, total, speed),
        connections,
        resumable,
        category: Some(job.category.clone()),
        queue: job.queue.clone(),
        added_at: job.added_at,
        completed_at: job.completed_at,
        error,
        media: Some(MediaDownloadInfo {
            title: job.title.clone(),
            format: job.format.clone(),
            via: job.via,
            step: if active {
                st.live.get(&job.id).and_then(|l| l.step)
            } else {
                None
            },
        }),
    }
}

fn progress_item(job: &Job, st: &State) -> ProgressItem {
    let (downloaded, total, speed, connections) = job_bytes(job, st);
    let mut segments: Option<Vec<(u64, u64, u64)>> = None;
    if job.via == MediaVia::Engine {
        let mut offset = 0;
        for p in &job.parts {
            let l = st.parts.get(&p.id).cloned().unwrap_or_default();
            if let Some(segs) = &l.segments {
                let v = segments.get_or_insert_with(Vec::new);
                v.extend(segs.iter().map(|(s, e, d)| (s + offset, e + offset, *d)));
            }
            offset += l.total.unwrap_or(l.downloaded);
        }
    }
    ProgressItem {
        id: job.id.clone(),
        downloaded_bytes: downloaded,
        total_bytes: total,
        speed_bps: speed,
        eta_secs: eta(downloaded, total, speed),
        connections,
        segments,
    }
}

/// One engine event → what clients get, and what to do next.
fn translate(st: &mut State, n: Notification) -> (Vec<Notification>, Vec<Action>) {
    let mut out = Vec::new();
    let mut actions = Vec::new();
    match n {
        Notification::DownloadAdded(s) | Notification::DownloadUpdated(s)
            if st.part_of.contains_key(&s.id) =>
        {
            let job_id = st.part_of[&s.id].clone();
            let before = st.jobs.get(&job_id).map(|j| job_state(j, st).0);
            absorb_summary(st.parts.entry(s.id.clone()).or_default(), &s);
            if let Some(job) = st.jobs.get(&job_id) {
                if job.phase == Phase::Downloading {
                    out.push(Notification::DownloadUpdated(summary(job, st)));
                }
                let after = job_state(job, st).0;
                if before != Some(after) {
                    state_changed(st, &job_id, after, &mut out, &mut actions);
                }
            }
        }
        Notification::DownloadProgress(ev) => {
            let mut items = Vec::new();
            let mut touched: Vec<String> = Vec::new();
            for item in ev.items {
                match st.part_of.get(&item.id).cloned() {
                    Some(job_id) => {
                        let live = st.parts.entry(item.id.clone()).or_default();
                        live.downloaded = item.downloaded_bytes;
                        live.total = item.total_bytes.or(live.total);
                        live.speed = item.speed_bps;
                        live.connections = item.connections;
                        if item.segments.is_some() {
                            live.segments = item.segments;
                        }
                        if !touched.contains(&job_id) {
                            touched.push(job_id);
                        }
                    }
                    None => items.push(item),
                }
            }
            for job_id in touched {
                if let Some(job) = st.jobs.get(&job_id)
                    && job.phase == Phase::Downloading
                {
                    items.push(progress_item(job, st));
                }
            }
            if !items.is_empty() {
                out.push(Notification::DownloadProgress(ProgressEvent { items }));
            }
        }
        Notification::DownloadState(ev) if st.part_of.contains_key(&ev.id) => {
            let job_id = st.part_of[&ev.id].clone();
            let before = st.jobs.get(&job_id).map(|j| job_state(j, st).0);
            {
                let live = st.parts.entry(ev.id.clone()).or_default();
                live.state = Some(ev.state);
                live.error = ev.error;
                if ev.state != DownloadState::Downloading {
                    live.speed = 0;
                    live.connections = 0;
                }
            }
            if let Some(job) = st.jobs.get(&job_id) {
                let after = job_state(job, st).0;
                if before != Some(after) {
                    state_changed(st, &job_id, after, &mut out, &mut actions);
                }
            }
        }
        Notification::DownloadRemoved(ev) => {
            let ids: Vec<String> = ev
                .ids
                .into_iter()
                .filter(|id| {
                    if st.part_of.remove(id).is_some() {
                        st.parts.remove(id);
                        false
                    } else {
                        true
                    }
                })
                .collect();
            if !ids.is_empty() {
                out.push(Notification::DownloadRemoved(RemovedEvent { ids }));
            }
        }
        Notification::Stats(s) => out.push(Notification::Stats(adjust_stats(st, s))),
        Notification::Queues(queues) => {
            st.queues_running = queues.iter().map(|q| (q.name.clone(), q.running)).collect();
            actions.push(Action::Fallbacks);
            out.push(Notification::Queues(queues));
        }
        other => out.push(other),
    }
    (out, actions)
}

fn state_changed(
    st: &State,
    job_id: &str,
    state: DownloadState,
    out: &mut Vec<Notification>,
    actions: &mut Vec<Action>,
) {
    let Some(job) = st.jobs.get(job_id) else {
        return;
    };
    if job.phase != Phase::Downloading {
        return;
    }
    let (_, error) = job_state(job, st);
    match state {
        // Every part is in: merge (the job reports `verifying` from now).
        DownloadState::Verifying => actions.push(Action::Finish(job_id.to_owned())),
        DownloadState::NeedsRefresh => {
            let live = st.live.get(job_id);
            if live.is_none_or(|l| !l.refreshing && l.refreshes < MAX_AUTO_REFRESHES) {
                actions.push(Action::Refresh(job_id.to_owned()));
                return;
            }
        }
        _ => {}
    }
    out.push(Notification::DownloadState(StateEvent {
        id: job_id.to_owned(),
        state,
        error,
    }));
}

/// The engine counts parts; clients count media downloads.
fn adjust_stats(st: &State, mut s: Stats) -> Stats {
    let (mut active_parts, mut queued_parts) = (0u32, 0u32);
    for live in st.parts.values() {
        match live.state {
            Some(
                DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying,
            ) => {
                active_parts += 1;
            }
            Some(DownloadState::Queued) => queued_parts += 1,
            _ => {}
        }
    }
    let (mut active, mut queued) = (0u32, 0u32);
    for job in st.jobs.values() {
        match job_state(job, st).0 {
            DownloadState::Probing | DownloadState::Downloading | DownloadState::Verifying => {
                active += 1;
            }
            DownloadState::Queued => queued += 1,
            _ => {}
        }
        if job.via == MediaVia::YtDlp
            && let Some(l) = st.live.get(&job.id)
            && l.child.is_some()
        {
            s.download_bps += l.fallback.speed;
        }
    }
    s.active = s.active.saturating_sub(active_parts) + active;
    s.queued = s.queued.saturating_sub(queued_parts) + queued;
    s
}

fn failed(code: &'static str, message: impl Into<String>) -> Fail {
    (code, message.into())
}

impl Shared {
    fn emit(&self, n: Notification) {
        lock(&self.subs).retain(|tx| tx.unbounded_send(n.clone()).is_ok());
    }

    fn act(self: &Arc<Self>, action: Action) {
        let me = self.clone();
        match action {
            Action::Finish(id) => {
                tokio::spawn(async move { me.finish(id).await });
            }
            Action::Refresh(id) => {
                tokio::spawn(async move { me.refresh(&id, false).await });
            }
            Action::Fallbacks => {
                tokio::spawn(async move { me.run_fallbacks().await });
            }
        }
    }

    async fn save(&self) {
        let _one = self.save_lock.lock().await;
        let jobs: Vec<Job> = self.state.lock().await.jobs.values().cloned().collect();
        if let Err(e) = jobs::save(&self.dir.join("jobs.json"), jobs).await {
            tracing::error!("cannot save media/jobs.json: {e}");
        }
    }

    fn info_path(&self, id: &str) -> PathBuf {
        self.dir.join("jobs").join(format!("{id}.json"))
    }

    async fn emit_job(&self, id: &str, updated: bool) {
        let st = self.state.lock().await;
        if let Some(job) = st.jobs.get(id) {
            let s = summary(job, &st);
            let state = StateEvent {
                id: s.id.clone(),
                state: s.state,
                error: s.error.clone(),
            };
            if updated {
                self.emit(Notification::DownloadUpdated(s));
            }
            self.emit(Notification::DownloadState(state));
        }
    }

    async fn set_step(&self, id: &str, step: Option<MediaStep>) {
        let mut st = self.state.lock().await;
        if let Some(l) = st.live.get_mut(id) {
            l.step = step;
        }
        if let Some(job) = st.jobs.get(id) {
            self.emit(Notification::DownloadUpdated(summary(job, &st)));
        }
    }

    /// Marks the job failed, keeps what was downloaded.
    async fn fail(&self, id: &str, (code, message): Fail) {
        {
            let mut st = self.state.lock().await;
            let Some(job) = st.jobs.get_mut(id) else {
                return;
            };
            tracing::warn!(id, code, %message, "media download failed");
            job.phase = Phase::Failed;
            job.error = Some(ErrorInfo {
                code: code.to_owned(),
                message,
            });
            if let Some(l) = st.live.get_mut(id) {
                l.step = None;
                l.task = None;
            }
        }
        self.save().await;
        self.emit_job(id, true).await;
    }

    async fn ensure(&self, id: &str, kind: Kind) -> Result<Tool, Fail> {
        if let Some(t) = self.tools.get(kind) {
            return Ok(t);
        }
        self.set_step(id, Some(MediaStep::PreparingTools)).await;
        let r = self.tools.ensure(kind).await;
        self.set_step(id, None).await;
        r.map_err(|e| failed("tools_unavailable", format!("{}: {e}", kind.name())))
    }

    /// The page's info JSON: from a recent probe, else asks yt-dlp.
    async fn info(
        &self,
        tool: &Tool,
        url: &str,
        request: Option<&RequestContext>,
        fresh: bool,
    ) -> Result<Arc<Value>, Fail> {
        if !fresh
            && let Some((at, v)) = lock(&self.probes).get(url)
            && at.elapsed() < PROBE_CACHE
        {
            return Ok(v.clone());
        }
        let v = Arc::new(
            ytdlp::probe(tool, &self.dirs, url, request)
                .await
                .map_err(|e| failed("media_unavailable", e))?,
        );
        let mut probes = lock(&self.probes);
        probes.retain(|_, (at, _)| at.elapsed() < PROBE_CACHE);
        if probes.len() >= PROBE_CACHE_ENTRIES
            && let Some(oldest) = probes
                .iter()
                .min_by_key(|(_, (at, _))| *at)
                .map(|(k, _)| k.clone())
        {
            probes.remove(&oldest);
        }
        probes.insert(url.to_owned(), (Instant::now(), v.clone()));
        Ok(v)
    }

    async fn write_info(&self, id: &str, info: &Value) -> Result<PathBuf, Fail> {
        let path = self.info_path(id);
        let bytes = serde_json::to_vec(info).map_err(|e| failed("io", e.to_string()))?;
        crate::util::write_atomic(&path, &bytes)
            .await
            .map_err(|e| failed("io", format!("{}: {e}", path.display())))?;
        Ok(path)
    }

    fn request(&self, id: &str) -> Option<RequestContext> {
        lock(&self.requests).get(id).cloned()
    }

    // ------------------------------------------------------------ add

    async fn add(self: &Arc<Self>, p: MediaAddParams) -> ServiceResult<AddResult> {
        if self.closed.load(Ordering::Acquire) {
            return Err(ServiceError::invalid_state("the daemon is shutting down"));
        }
        let url = check_url(&p.url)?;
        info::selector(&p.choice).map_err(ServiceError::invalid_params)?;
        if p.subtitles.len() > MAX_SUBTITLE_LANGS
            || !p.subtitles.iter().all(|l| info::valid_lang(l))
        {
            return Err(ServiceError::invalid_params(
                "subtitles: up to 20 language codes",
            ));
        }
        let user_stem = match &p.filename {
            Some(f) => Some(
                names::stem(f)
                    .ok_or_else(|| ServiceError::invalid_params("filename is not usable"))?,
            ),
            None => None,
        };
        let directory = match &p.directory {
            Some(d) => d.clone(),
            None => self.inner.settings().await?.default_directory,
        };
        if !Path::new(&directory).is_absolute() {
            return Err(ServiceError::invalid_params("directory must be absolute"));
        }
        let queue = p
            .queue
            .clone()
            .filter(|q| q != whoosh_proto::MAIN_QUEUE && !q.is_empty());
        if let Some(q) = &queue
            && !whoosh_proto::queue::is_queue_name(q)
        {
            return Err(ServiceError::invalid_params(
                "a queue name is 1–64 characters, without control characters",
            ));
        }
        let audio = matches!(p.choice, MediaChoice::Audio { .. });
        let cached_title = lock(&self.probes).get(&url).map(|(_, v)| info::title_of(v));
        let title = cached_title.unwrap_or_else(|| {
            url::Url::parse(&url)
                .ok()
                .and_then(|u| u.host_str().map(str::to_owned))
                .unwrap_or_else(|| "video".into())
        });
        let id = format!("md_{}", random_hex(8));
        let job = Job {
            id: id.clone(),
            url: url.clone(),
            filename: user_stem.clone().unwrap_or_else(|| title.clone()),
            title,
            format: String::new(),
            via: MediaVia::Engine,
            choice: p.choice,
            subtitles: p.subtitles,
            user_stem,
            stem: None,
            directory,
            category: p
                .category
                .filter(|c| !c.trim().is_empty())
                .unwrap_or_else(|| if audio { "music" } else { "video" }.into()),
            queue,
            added_at: now_ms(),
            completed_at: None,
            phase: Phase::Preparing,
            paused: !p.start,
            parts: Vec::new(),
            finish: None,
            selector: String::new(),
            total_bytes: None,
            downloaded_bytes: 0,
            error: None,
            subtitle_files: Vec::new(),
        };
        if let Some(req) = p.request {
            lock(&self.requests).insert(id.clone(), req);
        }
        let summary_now = {
            let mut st = self.state.lock().await;
            st.jobs.insert(id.clone(), job);
            st.live.insert(id.clone(), JobLive::default());
            summary(&st.jobs[&id], &st)
        };
        self.save().await;
        self.emit(Notification::DownloadAdded(summary_now));
        self.spawn_prepare(&id).await;
        Ok(AddResult { id })
    }

    async fn spawn_prepare(self: &Arc<Self>, id: &str) {
        let me = self.clone();
        let job_id = id.to_owned();
        let handle = tokio::spawn(async move {
            if let Err(e) = me.prepare(&job_id).await {
                me.fail(&job_id, e).await;
            }
        })
        .abort_handle();
        if let Some(l) = self.state.lock().await.live.get_mut(id) {
            l.task = Some(handle);
        }
    }

    /// Tools, formats, then the parts (or the yt-dlp download).
    async fn prepare(self: &Arc<Self>, id: &str) -> Result<(), Fail> {
        let Some(job) = self.state.lock().await.jobs.get(id).cloned() else {
            return Ok(());
        };
        let request = self.request(id);
        let yt = self.ensure(id, Kind::YtDlp).await?;
        let info = self.info(&yt, &job.url, request.as_ref(), false).await?;
        let info_path = self.write_info(id, &info).await?;
        let selector = info::selector(&job.choice).map_err(|e| failed("invalid", e))?;
        let resolved = ytdlp::resolve(&yt, &self.dirs, &info_path, &selector)
            .await
            .map_err(|e| failed("format_unavailable", e))?;
        let mut parts = info::parts_of(&resolved).map_err(|e| failed("format_unavailable", e))?;
        // Video first: ffmpeg maps video from the first input.
        parts.sort_by_key(|p| p.kind == MediaFormatKind::AudioOnly);
        let finish = info::finish_for(&parts, &job.choice);
        let title = info::title_of(&info);
        let stem = job
            .user_stem
            .clone()
            .or_else(|| names::stem(&title))
            .unwrap_or_else(|| "video".into());
        let via = if parts.iter().all(|p| p.direct) {
            MediaVia::Engine
        } else {
            MediaVia::YtDlp
        };
        let label = info::label(&parts, &finish);
        let exact = parts
            .iter()
            .map(|p| p.format_id.as_str())
            .collect::<Vec<_>>()
            .join("+");
        let total = parts
            .iter()
            .map(|p| p.size)
            .try_fold(0u64, |acc, s| s.map(|s| acc + s));
        let wants_ffmpeg = finish.needs_ffmpeg() || (via == MediaVia::YtDlp && parts.len() > 1);
        if wants_ffmpeg && self.tools.get(Kind::Ffmpeg).is_none() {
            // Fetch it while the parts download.
            let tools = self.tools.clone();
            tokio::spawn(async move {
                let _ = tools.ensure(Kind::Ffmpeg).await;
            });
        }
        let filename = format!("{stem}.{}", finish.ext());
        match via {
            MediaVia::Engine => {
                let mut st = self.state.lock().await;
                let Some(job) = st.jobs.get(id).cloned() else {
                    return Ok(());
                };
                let mut added: Vec<JobPart> = Vec::new();
                let mut error = None;
                for part in &parts {
                    let r = self
                        .inner
                        .add(AddParams {
                            url: part.url.clone(),
                            request: Some(part.request.clone()),
                            filename: Some(format!("{stem}.f{}.{}", part.format_id, part.ext)),
                            directory: Some(job.directory.clone()),
                            category: Some(job.category.clone()),
                            start: !job.paused,
                            mirrors: Vec::new(),
                            checksum: None,
                            connections: None,
                            queue: job.queue.clone(),
                        })
                        .await;
                    match r {
                        Ok(a) => {
                            st.part_of.insert(a.id.clone(), id.to_owned());
                            added.push(JobPart {
                                id: a.id,
                                format_id: part.format_id.clone(),
                                kind: part.kind,
                            });
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
                if let Some(e) = error {
                    let ids: Vec<String> = added.iter().map(|p| p.id.clone()).collect();
                    drop(st);
                    if !ids.is_empty() {
                        let _ = self
                            .inner
                            .remove(RemoveParams {
                                ids,
                                delete_files: true,
                            })
                            .await;
                    }
                    return Err(failed("add_failed", e.message));
                }
                let j = st.jobs.get_mut(id).expect("held");
                j.parts = added;
                j.via = via;
                j.title = title;
                j.format = label;
                j.stem = Some(stem.clone());
                j.filename = filename;
                j.finish = Some(SavedFinish::from(&finish));
                j.selector = exact;
                j.total_bytes = total;
                j.phase = Phase::Downloading;
                if let Some(l) = st.live.get_mut(id) {
                    l.task = None;
                }
                let s = summary(&st.jobs[id], &st);
                let all_done = parts_state(&st.jobs[id], &st).0 == DownloadState::Verifying;
                drop(st);
                self.emit(Notification::DownloadUpdated(s.clone()));
                self.emit(Notification::DownloadState(StateEvent {
                    id: s.id,
                    state: s.state,
                    error: s.error,
                }));
                self.save().await;
                if all_done {
                    self.act(Action::Finish(id.to_owned()));
                }
                if !job.subtitles.is_empty() {
                    let me = self.clone();
                    let id = id.to_owned();
                    tokio::spawn(async move { me.subtitles(&id, &yt, &info_path).await });
                }
            }
            MediaVia::YtDlp => {
                {
                    let mut st = self.state.lock().await;
                    let Some(job) = st.jobs.get_mut(id) else {
                        return Ok(());
                    };
                    job.via = via;
                    job.title = title;
                    job.format = label;
                    job.stem = Some(stem);
                    job.filename = filename;
                    job.finish = Some(SavedFinish::from(&finish));
                    job.selector = exact;
                    job.total_bytes = total;
                    job.phase = Phase::Downloading;
                    if let Some(l) = st.live.get_mut(id) {
                        l.task = None;
                    }
                }
                self.save().await;
                self.emit_job(id, true).await;
                self.run_fallbacks().await;
            }
        }
        Ok(())
    }

    /// Subtitles next to an engine download (not fatal when they fail).
    async fn subtitles(&self, id: &str, yt: &Tool, info: &Path) {
        let Some(job) = self.state.lock().await.jobs.get(id).cloned() else {
            return;
        };
        let Some(stem) = job.stem.clone() else { return };
        let dir = PathBuf::from(&job.directory);
        if let Err(e) = ytdlp::subtitles(yt, &self.dirs, info, &job.subtitles, &dir, &stem).await {
            tracing::warn!(id, error = %e, "subtitles failed");
            return;
        }
        let files: Vec<String> = ytdlp::subtitle_files(&dir, &stem, &job.subtitles)
            .into_iter()
            .map(|p| p.display().to_string())
            .collect();
        if let Some(job) = self.state.lock().await.jobs.get_mut(id) {
            job.subtitle_files = files;
        }
        self.save().await;
    }

    // ------------------------------------------------------------ finish

    /// Every part is in: merge, convert or rename; then drop the parts.
    async fn finish(self: &Arc<Self>, id: String) {
        let job = {
            let mut st = self.state.lock().await;
            let Some(job) = st.jobs.get_mut(&id) else {
                return;
            };
            if !matches!(job.phase, Phase::Downloading | Phase::Failed) {
                return;
            }
            job.phase = Phase::Finishing;
            job.error = None;
            let finish: Finish = job
                .finish
                .as_ref()
                .map(Finish::from)
                .unwrap_or(Finish::Rename { ext: "bin".into() });
            let step = match finish {
                Finish::Merge { .. } => Some(MediaStep::Merging),
                Finish::Audio { .. } => Some(MediaStep::Converting),
                Finish::Rename { .. } => None,
            };
            let job = job.clone();
            if let Some(l) = st.live.get_mut(&id) {
                l.step = step;
            }
            job
        };
        self.save().await;
        self.emit_job(&id, true).await;
        match self.finish_inner(&job).await {
            Ok((path, size)) => {
                let part_ids: Vec<String> = job.parts.iter().map(|p| p.id.clone()).collect();
                let renamed = matches!(job.finish, Some(SavedFinish::Rename { .. }) | None);
                if !part_ids.is_empty()
                    && let Err(e) = self
                        .inner
                        .remove(RemoveParams {
                            ids: part_ids,
                            delete_files: !renamed,
                        })
                        .await
                {
                    tracing::warn!(id, error = %e, "removing the parts failed");
                }
                {
                    let mut st = self.state.lock().await;
                    if let Some(job) = st.jobs.get_mut(&id) {
                        job.phase = Phase::Done;
                        job.completed_at = Some(now_ms());
                        job.filename = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if let Some(dir) = path.parent() {
                            job.directory = dir.display().to_string();
                        }
                        job.total_bytes = Some(size);
                        job.downloaded_bytes = size;
                        job.error = None;
                        job.parts.clear();
                    }
                    if let Some(l) = st.live.get_mut(&id) {
                        l.step = None;
                    }
                }
                let _ = tokio::fs::remove_file(self.info_path(&id)).await;
                lock(&self.requests).remove(&id);
                self.save().await;
                self.emit_job(&id, true).await;
                tracing::info!(id, file = %path.display(), "media download completed");
            }
            Err(e) => self.fail(&id, e).await,
        }
    }

    async fn finish_inner(&self, job: &Job) -> Result<(PathBuf, u64), Fail> {
        let mut paths = Vec::new();
        for p in &job.parts {
            let d = self
                .inner
                .get(IdParams { id: p.id.clone() })
                .await
                .map_err(|e| failed("part_missing", e.message))?;
            if d.summary.state != DownloadState::Completed {
                return Err(failed("part_missing", "a part is not complete"));
            }
            paths.push(PathBuf::from(&d.summary.directory).join(&d.summary.filename));
        }
        let first = paths
            .first()
            .cloned()
            .ok_or_else(|| failed("part_missing", "no parts"))?;
        let dir = first.parent().map(Path::to_path_buf).unwrap_or_default();
        let stem = job.stem.clone().unwrap_or_else(|| "video".into());
        let finish: Finish = job
            .finish
            .as_ref()
            .map(Finish::from)
            .unwrap_or(Finish::Rename { ext: "bin".into() });
        let out = names::unique(&dir, &stem, finish.ext());
        match &finish {
            Finish::Rename { .. } => {
                tokio::fs::rename(&first, &out)
                    .await
                    .map_err(|e| failed("io", format!("{}: {e}", out.display())))?;
            }
            Finish::Merge { ext } | Finish::Audio { ext, .. } => {
                let ffmpeg = self
                    .tools
                    .ensure(Kind::Ffmpeg)
                    .await
                    .map_err(|e| failed("tools_unavailable", format!("ffmpeg: {e}")))?;
                let tmp = ytdlp::temp_output(&out);
                let args = match &finish {
                    Finish::Merge { .. } => {
                        let audio = paths
                            .get(1)
                            .ok_or_else(|| failed("part_missing", "no audio part"))?;
                        ffmpeg::merge_args(&first, audio, &tmp, ext)
                    }
                    Finish::Audio { mp3, .. } => ffmpeg::audio_args(&first, &tmp, ext, *mp3),
                    Finish::Rename { .. } => unreachable!(),
                };
                if let Err(e) = ffmpeg::run(&ffmpeg, args).await {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(failed("merge_failed", e));
                }
                tokio::fs::rename(&tmp, &out)
                    .await
                    .map_err(|e| failed("io", format!("{}: {e}", out.display())))?;
            }
        }
        let size = tokio::fs::metadata(&out)
            .await
            .map(|m| m.len())
            .unwrap_or(0);
        Ok((out, size))
    }

    // ------------------------------------------------------------ refresh

    /// The parts' links expired: ask the site again for the same formats.
    async fn refresh(self: &Arc<Self>, id: &str, manual: bool) {
        {
            let mut st = self.state.lock().await;
            let Some(l) = st.live.get_mut(id) else { return };
            if l.refreshing || (!manual && l.refreshes >= MAX_AUTO_REFRESHES) {
                return;
            }
            l.refreshing = true;
            l.refreshes += 1;
            l.step = Some(MediaStep::Refreshing);
        }
        self.emit_job(id, true).await;
        let r = self.refresh_inner(id).await;
        {
            let mut st = self.state.lock().await;
            if let Some(l) = st.live.get_mut(id) {
                l.refreshing = false;
                l.step = None;
            }
        }
        if let Err((code, msg)) = r {
            tracing::warn!(id, code, %msg, "refreshing the media links failed");
        }
        self.emit_job(id, true).await;
    }

    async fn refresh_inner(&self, id: &str) -> Result<(), Fail> {
        let Some(job) = self.state.lock().await.jobs.get(id).cloned() else {
            return Ok(());
        };
        if job.via != MediaVia::Engine || job.parts.is_empty() {
            return Ok(());
        }
        let yt = self.ensure(id, Kind::YtDlp).await?;
        let request = self.request(id);
        let info = self.info(&yt, &job.url, request.as_ref(), true).await?;
        let info_path = self.write_info(id, &info).await?;
        let resolved = ytdlp::resolve(&yt, &self.dirs, &info_path, &job.selector)
            .await
            .map_err(|e| failed("format_unavailable", e))?;
        let fresh = info::parts_of(&resolved).map_err(|e| failed("format_unavailable", e))?;
        for part in &job.parts {
            let Some(f) = fresh.iter().find(|f| f.format_id == part.format_id) else {
                return Err(failed(
                    "format_unavailable",
                    format!("format {} is gone", part.format_id),
                ));
            };
            let r = self
                .inner
                .refresh_url(RefreshUrlParams {
                    id: part.id.clone(),
                    url: f.url.clone(),
                    request: Some(f.request.clone()),
                })
                .await;
            if let Err(e) = r
                && e.code != error_code::INVALID_STATE
            {
                return Err(failed("refresh_failed", e.message));
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------ yt-dlp downloads

    /// Starts yt-dlp downloads whose queue runs (up to the limit) and stops
    /// those whose queue was stopped.
    async fn run_fallbacks(self: &Arc<Self>) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let mut start = Vec::new();
        {
            let mut st = self.state.lock().await;
            let running_queue = |st: &State, q: &Option<String>| {
                let name = q.as_deref().unwrap_or(whoosh_proto::MAIN_QUEUE);
                st.queues_running.get(name).copied().unwrap_or(true)
            };
            let ids: Vec<String> = st
                .jobs
                .values()
                .filter(|j| j.via == MediaVia::YtDlp && j.phase == Phase::Downloading)
                .map(|j| j.id.clone())
                .collect();
            let mut running = ids
                .iter()
                .filter(|id| st.live.get(*id).is_some_and(|l| l.child.is_some()))
                .count();
            for id in ids {
                let job = &st.jobs[&id];
                let go = !job.paused && running_queue(&st, &job.queue);
                let has_child = st.live.get(&id).is_some_and(|l| l.child.is_some());
                if !go && has_child {
                    if let Some(l) = st.live.get_mut(&id) {
                        l.child = None; // stops it
                    }
                    running -= 1;
                } else if go && !has_child && running < MAX_FALLBACK_RUNS {
                    let (tx, rx) = oneshot::channel();
                    let live = st.live.entry(id.clone()).or_default();
                    live.child = Some(tx);
                    live.run += 1;
                    let run = live.run;
                    running += 1;
                    start.push((id, rx, run));
                }
            }
        }
        for (id, rx, run) in start {
            let me = self.clone();
            self.emit_job(&id, false).await;
            tokio::spawn(async move { me.fallback(id, rx, run).await });
        }
    }

    async fn fallback(self: Arc<Self>, id: String, cancel: oneshot::Receiver<()>, run: u64) {
        let end = self.fallback_inner(&id, cancel).await;
        {
            let mut st = self.state.lock().await;
            if let Some(l) = st.live.get_mut(&id)
                && l.run == run
            {
                l.child = None;
                l.step = None;
            }
        }
        match end {
            Ok(FallbackEnd::Done(path)) => {
                let size = tokio::fs::metadata(&path)
                    .await
                    .map(|m| m.len())
                    .unwrap_or(0);
                {
                    let mut st = self.state.lock().await;
                    if let Some(job) = st.jobs.get_mut(&id) {
                        job.phase = Phase::Done;
                        job.completed_at = Some(now_ms());
                        job.filename = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if let Some(dir) = path.parent() {
                            job.directory = dir.display().to_string();
                        }
                        job.total_bytes = Some(size);
                        job.downloaded_bytes = size;
                        if let Some(stem) = &job.stem {
                            job.subtitle_files = ytdlp::subtitle_files(
                                Path::new(&job.directory),
                                stem,
                                &job.subtitles,
                            )
                            .into_iter()
                            .map(|p| p.display().to_string())
                            .collect();
                        }
                    }
                }
                let _ = tokio::fs::remove_file(self.info_path(&id)).await;
                lock(&self.requests).remove(&id);
                self.save().await;
                self.emit_job(&id, true).await;
            }
            Ok(FallbackEnd::Cancelled) => self.emit_job(&id, false).await,
            Ok(FallbackEnd::Failed(e)) => self.fail(&id, failed("download_failed", e)).await,
            Err(e) => self.fail(&id, e).await,
        }
        // A slot is free.
        self.act(Action::Fallbacks);
    }

    async fn fallback_inner(
        self: &Arc<Self>,
        id: &str,
        cancel: oneshot::Receiver<()>,
    ) -> Result<FallbackEnd, Fail> {
        let Some(job) = self.state.lock().await.jobs.get(id).cloned() else {
            return Ok(FallbackEnd::Cancelled);
        };
        let yt = self.ensure(id, Kind::YtDlp).await?;
        let finish: Finish = job
            .finish
            .as_ref()
            .map(Finish::from)
            .unwrap_or(Finish::Rename { ext: "bin".into() });
        let ffmpeg = if finish.needs_ffmpeg() || job.selector.contains('+') {
            Some(self.ensure(id, Kind::Ffmpeg).await?)
        } else {
            self.tools.get(Kind::Ffmpeg)
        };
        let request = self.request(id);
        let info_path = self.info_path(id);
        if !info_path.is_file() {
            let info = self.info(&yt, &job.url, request.as_ref(), true).await?;
            self.write_info(id, &info).await?;
        }
        let limit = self
            .inner
            .settings()
            .await
            .ok()
            .and_then(|s| s.speed_limit_bps);
        let stem = job.stem.clone().unwrap_or_else(|| "video".into());
        let dir = PathBuf::from(&job.directory);
        let (mp3, extract) = match finish {
            Finish::Audio { mp3, .. } => (mp3, !mp3),
            _ => (false, false),
        };
        let run = FallbackRun {
            info: &info_path,
            selector: &job.selector,
            dir: &dir,
            stem: &stem,
            ffmpeg: ffmpeg.as_ref().map(|t| t.program.as_path()),
            subtitles: &job.subtitles,
            mp3,
            extract_audio: extract,
            limit_bps: limit,
        };
        self.set_step(id, Some(MediaStep::Downloading)).await;
        let progress_sink = {
            let me = Arc::downgrade(self);
            let id = id.to_owned();
            move |p: ytdlp::FallbackProgress| {
                if let Some(me) = me.upgrade() {
                    me.fallback_progress(&id, p);
                }
            }
        };
        let req = request.as_ref().map(|r| (job.url.as_str(), r));
        Ok(ytdlp::download(&yt, &self.dirs, &run, req, cancel, progress_sink).await)
    }

    fn fallback_progress(self: &Arc<Self>, id: &str, p: ytdlp::FallbackProgress) {
        let me = self.clone();
        let id = id.to_owned();
        // Called from the reading loop; do the locking elsewhere.
        tokio::spawn(async move {
            let mut st = me.state.lock().await;
            let Some(l) = st.live.get_mut(&id) else {
                return;
            };
            l.fallback = p;
            let due = l
                .last_progress
                .is_none_or(|t| t.elapsed() >= Duration::from_millis(250));
            if !due {
                return;
            }
            l.last_progress = Some(Instant::now());
            if let Some(job) = st.jobs.get(&id) {
                let item = progress_item(job, &st);
                me.emit(Notification::DownloadProgress(ProgressEvent {
                    items: vec![item],
                }));
            }
        });
    }

    // ------------------------------------------------------------ startup

    async fn resume_all(self: &Arc<Self>) {
        let jobs: Vec<Job> = self.state.lock().await.jobs.values().cloned().collect();
        for job in jobs {
            match (job.phase, job.via) {
                (Phase::Preparing, _) => self.spawn_prepare(&job.id).await,
                (Phase::Downloading, MediaVia::Engine) => {
                    let st = self.state.lock().await;
                    let missing = job.parts.iter().any(|p| !st.parts.contains_key(&p.id));
                    let all_done = parts_state(&job, &st).0 == DownloadState::Verifying;
                    drop(st);
                    if missing {
                        self.fail(
                            &job.id,
                            failed("part_missing", "a part of this download is gone"),
                        )
                        .await;
                    } else if all_done {
                        self.act(Action::Finish(job.id.clone()));
                    }
                }
                _ => {}
            }
        }
        self.run_fallbacks().await;
    }

    // ------------------------------------------------------------ requests on ids

    /// Splits ids into media downloads and the engine's own.
    async fn split(&self, ids: &[String]) -> (Vec<String>, Vec<String>) {
        let st = self.state.lock().await;
        ids.iter().cloned().partition(|id| st.jobs.contains_key(id))
    }

    async fn pause(self: &Arc<Self>, ids: Vec<String>) -> ServiceResult<()> {
        let (media, other) = self.split(&ids).await;
        if !other.is_empty() {
            self.inner.pause(IdsParams { ids: other }).await?;
        }
        let mut parts = Vec::new();
        let mut changed = Vec::new();
        {
            let mut st = self.state.lock().await;
            for id in &media {
                let Some(job) = st.jobs.get_mut(id) else {
                    continue;
                };
                match job.phase {
                    Phase::Preparing | Phase::Downloading => {
                        job.paused = true;
                        parts.extend(job.parts.iter().map(|p| p.id.clone()));
                        changed.push(id.clone());
                        if let Some(l) = st.live.get_mut(id) {
                            l.child = None;
                        }
                    }
                    _ => {}
                }
            }
        }
        if !parts.is_empty() {
            self.inner.pause(IdsParams { ids: parts }).await?;
        }
        if !changed.is_empty() {
            self.save().await;
            for id in changed {
                self.emit_job(&id, false).await;
            }
        }
        Ok(())
    }

    async fn resume(self: &Arc<Self>, ids: Vec<String>) -> ServiceResult<()> {
        let (media, other) = self.split(&ids).await;
        if !other.is_empty() {
            self.inner.resume(IdsParams { ids: other }).await?;
        }
        let mut parts = Vec::new();
        let mut finish = Vec::new();
        let mut prepare = Vec::new();
        let mut changed = Vec::new();
        {
            let mut st = self.state.lock().await;
            for id in &media {
                let all_done = st
                    .jobs
                    .get(id)
                    .is_some_and(|j| parts_state(j, &st).0 == DownloadState::Verifying);
                let has_task = st
                    .live
                    .get(id)
                    .is_some_and(|l| l.task.as_ref().is_some_and(|t| !t.is_finished()));
                let Some(job) = st.jobs.get_mut(id) else {
                    continue;
                };
                job.paused = false;
                changed.push(id.clone());
                match job.phase {
                    Phase::Preparing if !has_task => prepare.push(id.clone()),
                    Phase::Downloading => parts.extend(job.parts.iter().map(|p| p.id.clone())),
                    Phase::Failed if job.parts.is_empty() && job.via == MediaVia::Engine => {
                        job.phase = Phase::Preparing;
                        job.error = None;
                        prepare.push(id.clone());
                    }
                    Phase::Failed if job.via == MediaVia::YtDlp => {
                        job.phase = Phase::Downloading;
                        job.error = None;
                    }
                    Phase::Failed if all_done => finish.push(id.clone()),
                    Phase::Failed => {
                        job.phase = Phase::Downloading;
                        job.error = None;
                        parts.extend(job.parts.iter().map(|p| p.id.clone()));
                    }
                    _ => {}
                }
            }
        }
        if !parts.is_empty() {
            self.inner.resume(IdsParams { ids: parts }).await?;
        }
        self.save().await;
        for id in &changed {
            self.emit_job(id, false).await;
        }
        for id in prepare {
            self.spawn_prepare(&id).await;
        }
        for id in finish {
            self.act(Action::Finish(id));
        }
        self.run_fallbacks().await;
        Ok(())
    }

    async fn remove(self: &Arc<Self>, p: RemoveParams) -> ServiceResult<()> {
        let (media, other) = self.split(&p.ids).await;
        if !other.is_empty() {
            self.inner
                .remove(RemoveParams {
                    ids: other,
                    delete_files: p.delete_files,
                })
                .await?;
        }
        if media.is_empty() {
            return Ok(());
        }
        let mut removed = Vec::new();
        let mut parts = Vec::new();
        {
            let mut st = self.state.lock().await;
            for id in &media {
                let Some(job) = st.jobs.remove(id) else {
                    continue;
                };
                if let Some(l) = st.live.remove(id) {
                    if let Some(t) = l.task {
                        t.abort();
                    }
                    drop(l.child);
                }
                parts.extend(job.parts.iter().map(|p| p.id.clone()));
                removed.push(job);
            }
        }
        if !parts.is_empty() {
            // Parts are never the user's files: always deleted.
            if let Err(e) = self
                .inner
                .remove(RemoveParams {
                    ids: parts,
                    delete_files: true,
                })
                .await
            {
                tracing::warn!(error = %e, "removing media parts failed");
            }
        }
        for job in &removed {
            lock(&self.requests).remove(&job.id);
            let _ = tokio::fs::remove_file(self.info_path(&job.id)).await;
            let dir = PathBuf::from(&job.directory);
            if let Some(stem) = &job.stem {
                for f in ytdlp::partial_files(&dir, stem) {
                    let _ = tokio::fs::remove_file(f).await;
                }
                if let Some(f) = &job.finish {
                    let ext = Finish::from(f).ext().to_owned();
                    let _ = tokio::fs::remove_file(ytdlp::temp_output(
                        &dir.join(format!("{stem}.{ext}")),
                    ))
                    .await;
                }
            }
            if p.delete_files {
                if job.phase == Phase::Done {
                    let _ = tokio::fs::remove_file(dir.join(&job.filename)).await;
                }
                for f in &job.subtitle_files {
                    let _ = tokio::fs::remove_file(f).await;
                }
            }
        }
        self.save().await;
        self.emit(Notification::DownloadRemoved(RemovedEvent {
            ids: removed.into_iter().map(|j| j.id).collect(),
        }));
        Ok(())
    }

    async fn set_queue(self: &Arc<Self>, p: SetQueueParams) -> ServiceResult<()> {
        let (media, other) = self.split(&p.ids).await;
        if !other.is_empty() {
            self.inner
                .set_queue(SetQueueParams {
                    ids: other,
                    queue: p.queue.clone(),
                })
                .await?;
        }
        if media.is_empty() {
            return Ok(());
        }
        let queue = p
            .queue
            .clone()
            .filter(|q| q != whoosh_proto::MAIN_QUEUE && !q.is_empty());
        let mut parts = Vec::new();
        {
            let mut st = self.state.lock().await;
            for id in &media {
                if let Some(job) = st.jobs.get_mut(id) {
                    job.queue = queue.clone();
                    parts.extend(job.parts.iter().map(|p| p.id.clone()));
                }
            }
        }
        if !parts.is_empty() {
            self.inner
                .set_queue(SetQueueParams {
                    ids: parts,
                    queue: p.queue,
                })
                .await?;
        }
        self.save().await;
        for id in media {
            self.emit_job(&id, true).await;
        }
        self.run_fallbacks().await;
        Ok(())
    }

    async fn list(&self, p: ListParams) -> ServiceResult<Vec<DownloadSummary>> {
        let all = self.inner.list(ListParams::default()).await?;
        let st = self.state.lock().await;
        let mut v: Vec<DownloadSummary> = all
            .into_iter()
            .filter(|s| !st.part_of.contains_key(&s.id))
            .collect();
        v.extend(st.jobs.values().map(|j| summary(j, &st)));
        drop(st);
        v.retain(|s| p.states.is_empty() || p.states.contains(&s.state));
        if let Some(c) = &p.category {
            v.retain(|s| s.category.as_ref() == Some(c));
        }
        v.sort_by_key(|s| std::cmp::Reverse(s.added_at));
        let offset = p.offset.unwrap_or(0) as usize;
        let limit = p.limit.map_or(usize::MAX, |l| l as usize);
        Ok(v.into_iter().skip(offset).take(limit).collect())
    }

    async fn get(&self, id: String) -> ServiceResult<DownloadDetail> {
        let job = {
            let st = self.state.lock().await;
            if st.part_of.contains_key(&id) {
                return Err(ServiceError::not_found(&id));
            }
            match st.jobs.get(&id) {
                Some(job) => (job.clone(), summary(job, &st)),
                None => {
                    drop(st);
                    return self.inner.get(IdParams { id }).await;
                }
            }
        };
        let (job, summary) = job;
        let mut segments = Vec::new();
        let mut offset = 0u64;
        for p in &job.parts {
            if let Ok(d) = self.inner.get(IdParams { id: p.id.clone() }).await {
                segments.extend(d.segments.into_iter().map(|s| SegmentInfo {
                    start: s.start + offset,
                    end: s.end + offset,
                    ..s
                }));
                offset += d.summary.total_bytes.unwrap_or(d.summary.downloaded_bytes);
            }
        }
        Ok(DownloadDetail { summary, segments })
    }

    async fn probe(&self, p: MediaProbeParams) -> ServiceResult<MediaProbeResult> {
        let url = check_url(&p.url)?;
        let yt = self
            .tools
            .ensure(Kind::YtDlp)
            .await
            .map_err(|e| ServiceError::network(format!("yt-dlp is not available: {e}")))?;
        let info = self
            .info(&yt, &url, p.request.as_ref(), true)
            .await
            .map_err(|(_, e)| ServiceError::network(e))?;
        let r = info::probe_result(&info, &url).map_err(ServiceError::invalid_params)?;
        if r.url != url {
            lock(&self.probes).insert(r.url.clone(), (Instant::now(), info.clone()));
        }
        Ok(r)
    }

    async fn stats(&self) -> ServiceResult<Stats> {
        let s = self.inner.stats().await?;
        let st = self.state.lock().await;
        Ok(adjust_stats(&st, s))
    }

    async fn update_tools(
        self: &Arc<Self>,
        p: MediaUpdateParams,
    ) -> ServiceResult<MediaToolsStatus> {
        let kinds: Vec<Kind> = if p.tools.is_empty() {
            Kind::ALL.to_vec()
        } else {
            p.tools
                .iter()
                .map(|n| {
                    Kind::from_name(n)
                        .ok_or_else(|| ServiceError::invalid_params(format!("no tool named {n:?}")))
                })
                .collect::<Result<_, _>>()?
        };
        for kind in kinds {
            self.tools.begin_install(kind);
            let tools = self.tools.clone();
            tokio::spawn(async move {
                let _ = tools.install(kind, true).await;
            });
        }
        Ok(self.tools.status())
    }

    async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        {
            let mut st = self.state.lock().await;
            for l in st.live.values_mut() {
                if let Some(t) = l.task.take() {
                    t.abort();
                }
                l.child = None;
            }
        }
        self.save().await;
        self.inner.shutdown().await;
        lock(&self.subs).clear();
    }
}

fn check_url(url: &str) -> ServiceResult<String> {
    let url = url.trim();
    let parsed = url::Url::parse(url)
        .map_err(|e| ServiceError::invalid_params(format!("invalid URL: {e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(ServiceError::invalid_params("only http and https pages"));
    }
    Ok(url.to_owned())
}

impl Service for MediaService {
    fn capabilities(&self) -> Vec<String> {
        let mut caps = self.shared.inner.capabilities();
        if !caps.iter().any(|c| c == MEDIA_CAPABILITY) {
            caps.push(MEDIA_CAPABILITY.into());
        }
        caps
    }

    fn probe(&self, params: ProbeParams) -> BoxFuture<'_, ServiceResult<ProbeResult>> {
        self.shared.inner.probe(params)
    }

    fn add(&self, params: AddParams) -> BoxFuture<'_, ServiceResult<AddResult>> {
        self.shared.inner.add(params)
    }

    fn list(&self, params: ListParams) -> BoxFuture<'_, ServiceResult<Vec<DownloadSummary>>> {
        Box::pin(self.shared.list(params))
    }

    fn get(&self, params: IdParams) -> BoxFuture<'_, ServiceResult<DownloadDetail>> {
        Box::pin(self.shared.get(params.id))
    }

    fn pause(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.shared.pause(params.ids))
    }

    fn resume(&self, params: IdsParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.shared.resume(params.ids))
    }

    fn remove(&self, params: RemoveParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.shared.remove(params))
    }

    fn refresh_url(&self, params: RefreshUrlParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(async move {
            let is_media = self.shared.state.lock().await.jobs.contains_key(&params.id);
            if !is_media {
                return self.shared.inner.refresh_url(params).await;
            }
            // The media download's own page gives the fresh links.
            let me = self.shared.clone();
            tokio::spawn(async move { me.refresh(&params.id, true).await });
            Ok(())
        })
    }

    fn preconnect(&self, params: PreconnectParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.shared.inner.preconnect(params)
    }

    fn stats(&self) -> BoxFuture<'_, ServiceResult<Stats>> {
        Box::pin(self.shared.stats())
    }

    fn settings(&self) -> BoxFuture<'_, ServiceResult<Settings>> {
        self.shared.inner.settings()
    }

    fn set_settings(&self, settings: Settings) -> BoxFuture<'_, ServiceResult<Settings>> {
        self.shared.inner.set_settings(settings)
    }

    fn set_queue(&self, params: SetQueueParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(self.shared.set_queue(params))
    }

    fn queues(&self) -> BoxFuture<'_, ServiceResult<Vec<Queue>>> {
        self.shared.inner.queues()
    }

    fn create_queue(&self, config: QueueConfig) -> BoxFuture<'_, ServiceResult<Queue>> {
        self.shared.inner.create_queue(config)
    }

    fn update_queue(&self, config: QueueConfig) -> BoxFuture<'_, ServiceResult<Queue>> {
        self.shared.inner.update_queue(config)
    }

    fn rename_queue(&self, params: QueueRenameParams) -> BoxFuture<'_, ServiceResult<Queue>> {
        Box::pin(async move {
            let q = self.shared.inner.rename_queue(params.clone()).await?;
            {
                let mut st = self.shared.state.lock().await;
                for job in st.jobs.values_mut() {
                    if job.queue.as_deref() == Some(params.name.as_str()) {
                        job.queue = Some(params.new_name.clone());
                    }
                }
            }
            self.shared.save().await;
            Ok(q)
        })
    }

    fn remove_queue(&self, params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        Box::pin(async move {
            self.shared.inner.remove_queue(params.clone()).await?;
            {
                let mut st = self.shared.state.lock().await;
                for job in st.jobs.values_mut() {
                    if job.queue.as_deref() == Some(params.name.as_str()) {
                        job.queue = None;
                    }
                }
            }
            self.shared.save().await;
            Ok(())
        })
    }

    fn start_queue(&self, params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.shared.inner.start_queue(params)
    }

    fn stop_queue(&self, params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.shared.inner.stop_queue(params)
    }

    fn cancel_queue_action(&self, params: QueueNameParams) -> BoxFuture<'_, ServiceResult<()>> {
        self.shared.inner.cancel_queue_action(params)
    }

    fn media_probe(
        &self,
        params: MediaProbeParams,
    ) -> BoxFuture<'_, ServiceResult<MediaProbeResult>> {
        Box::pin(self.shared.probe(params))
    }

    fn media_add(&self, params: MediaAddParams) -> BoxFuture<'_, ServiceResult<AddResult>> {
        Box::pin(self.shared.add(params))
    }

    fn media_tools(&self) -> BoxFuture<'_, ServiceResult<MediaToolsStatus>> {
        Box::pin(async move { Ok(self.shared.tools.status()) })
    }

    fn media_check_updates(&self) -> BoxFuture<'_, ServiceResult<MediaToolsStatus>> {
        Box::pin(async move {
            self.shared
                .tools
                .check_updates()
                .await
                .map_err(ServiceError::network)?;
            Ok(self.shared.tools.status())
        })
    }

    fn media_update_tools(
        &self,
        params: MediaUpdateParams,
    ) -> BoxFuture<'_, ServiceResult<MediaToolsStatus>> {
        Box::pin(self.shared.update_tools(params))
    }

    fn media_configure(
        &self,
        params: MediaConfigureParams,
    ) -> BoxFuture<'_, ServiceResult<MediaToolsStatus>> {
        Box::pin(async move {
            self.shared
                .tools
                .set_auto_update(params.auto_update)
                .await
                .map_err(ServiceError::io)?;
            if params.auto_update {
                let tools = self.shared.tools.clone();
                tokio::spawn(async move { tools.auto_update_if_due().await });
            }
            Ok(self.shared.tools.status())
        })
    }

    fn subscribe(&self) -> BoxStream<'static, Notification> {
        let (tx, rx) = unbounded();
        if !self.shared.closed.load(Ordering::Acquire) {
            lock(&self.shared.subs).push(tx);
        }
        Box::pin(rx)
    }

    fn shutdown(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.shared.shutdown())
    }
}
